"""Syslog text embedding background worker for Bonsai ML.

EV1-3 T1: Polls GET /api/events/unembedded?type=syslog&limit=200 for events
with needs_embedding=true. Batches them, calls TextEmbedder, and POSTs
results to POST /api/events/embeddings.

WP6.5A: embedding computation goes through the persistent three-stage
template dedup in :mod:`bonsai_ml.message_templates` — exact fingerprint,
then normalized template fingerprint, then provider call only on a cache
miss.  Repeated messages reuse stored vectors; every event still gets its
own EventEmbedding record so raw evidence and occurrences stay independently
queryable.  Set ``dedup_enabled: false`` in the embedding config to restore
the legacy per-event path.

Architecture:
  Hot path (write_blocking): writes raw syslog to graph with needs_embedding=true
  Background (this file): polls every POLL_INTERVAL_SECS, embeds batches,
                           writes EventEmbedding records back to graph
  Inference path (STGNN): reads pre-computed embeddings via DeviceEmbedding nodes

Run as:
    python -m bonsai_ml.syslog_embedding_worker
"""
from __future__ import annotations

import logging
import os
import time
from typing import Any, Optional

log = logging.getLogger(__name__)

from .api_auth import auth_headers

POLL_INTERVAL_SECS = 60
FETCH_LIMIT = 200
BACKOFF_SECS = [60, 120, 300]
DEFAULT_API_URL = "http://localhost:3000"

# Most recent dedup stats snapshot, for ops introspection and tests.
_LAST_DEDUP_STATS: dict[str, Any] = {}


def get_dedup_stats() -> dict[str, Any]:
    """Return the most recent template-dedup metrics snapshot (or {})."""
    return dict(_LAST_DEDUP_STATS)


def run_syslog_embedding_worker(
    api_url: str = DEFAULT_API_URL,
    embedding_config: dict[str, Any] | None = None,
    poll_interval: int = POLL_INTERVAL_SECS,
    run_once: bool = False,
    dedup: Any = None,
) -> None:
    """Run the syslog embedding background worker loop.

    Args:
        api_url: Bonsai core API base URL.
        embedding_config: Dict passed to load_from_config(). None = defaults.
            Recognized dedup keys: ``dedup_enabled`` (default true),
            ``template_registry_path``, ``dedup_scope``,
            ``dedup_claim_ttl_secs``, ``dedup_lru_entries``.
        poll_interval: Seconds between polling cycles.
        run_once: If True, run one cycle and exit (for testing/cron use).
        dedup: Optional pre-built TemplateDedup (tests).  When None, one is
            built from ``embedding_config`` unless ``dedup_enabled`` is false.
    """
    from .text_embeddings import load_from_config

    cfg = embedding_config or {}
    embedder = load_from_config(cfg)
    owns_dedup = dedup is None
    if dedup is None:
        from .message_templates import build_template_dedup

        dedup = build_template_dedup(cfg, embedder)
    log.info(
        "SyslogEmbeddingWorker: starting (model=%s rev=%s, dedup=%s, "
        "poll_interval=%ds)",
        embedder.model_name,
        getattr(embedder, "model_revision", "") or "-",
        "on" if dedup is not None else "off",
        poll_interval,
    )

    backoff_idx = 0
    total_embedded = 0

    try:
        while True:
            try:
                events = _fetch_unembedded_events(api_url, limit=FETCH_LIMIT)

                if events:
                    embedded_count = _embed_and_post(api_url, events, embedder, dedup)
                    total_embedded += embedded_count
                    log.info(
                        "SyslogEmbeddingWorker: embedded %d events (total=%d, model=%s)",
                        embedded_count, total_embedded, embedder.model_name,
                    )
                    if dedup is not None:
                        snap = dedup.stats_snapshot()
                        _LAST_DEDUP_STATS.clear()
                        _LAST_DEDUP_STATS.update(snap)
                        log.info(
                            "SyslogEmbeddingWorker dedup: %s",
                            {k: v for k, v in snap.items()
                             if k in ("exact_match_hits", "template_match_hits",
                                      "new_templates", "provider_calls",
                                      "provider_calls_avoided",
                                      "embedding_cache_hit_ratio",
                                      "template_cardinality",
                                      "estimated_provider_ms_saved")},
                        )
                    backoff_idx = 0
                else:
                    log.debug("SyslogEmbeddingWorker: no unembedded events found")

            except Exception as exc:
                sleep = BACKOFF_SECS[min(backoff_idx, len(BACKOFF_SECS) - 1)]
                log.error(
                    "SyslogEmbeddingWorker: error (backing off %ds): %s", sleep, exc
                )
                backoff_idx += 1
                if run_once:
                    raise
                time.sleep(sleep)
                continue

            if run_once:
                break

            time.sleep(poll_interval)
    finally:
        # run_once callers (e.g. the 60s job-engine schedule) open a fresh
        # registry per invocation; close it so connections do not leak.
        if owns_dedup and dedup is not None:
            try:
                dedup.registry.close()
            except Exception:
                pass


def _fetch_unembedded_events(api_url: str, limit: int) -> list[dict[str, Any]]:
    try:
        import requests
        resp = requests.get(
            f"{api_url}/api/events/unembedded",
            params={"type": "syslog", "limit": limit},
            headers=auth_headers(),
            timeout=15,
        )
        resp.raise_for_status()
        data = resp.json()
        return data.get("events", data) if isinstance(data, dict) else data
    except Exception as exc:
        log.warning("Failed to fetch unembedded events: %s", exc)
        return []


def _extract_event_text(e: dict[str, Any]) -> str:
    text = e.get("message") or e.get("reason") or ""
    if not text and "detail_json" in e:
        try:
            import json
            detail = json.loads(e["detail_json"])
            text = detail.get("message") or detail.get("reason") or ""
        except Exception:
            pass
    return text


def _embed_and_post(
    api_url: str,
    events: list[dict[str, Any]],
    embedder: Any,
    dedup: Any = None,
) -> int:
    texts = [_extract_event_text(e) for e in events]
    event_ids = [e.get("id") or e.get("event_id") for e in events]
    event_types = [e.get("event_type") or e.get("type") or "" for e in events]

    valid_pairs = [
        (i, eid, text)
        for i, (eid, text) in enumerate(zip(event_ids, texts))
        if eid and text
    ]
    if not valid_pairs:
        return 0

    # vectors[i] aligned with valid_pairs, or None for unresolved events.
    if dedup is not None:
        valid_texts = [p[2] for p in valid_pairs]
        vectors = dedup.resolve_batch(valid_texts)
    else:
        t0 = time.monotonic()
        all_vectors = embedder.embed_batch([p[2] for p in valid_pairs])
        latency_ms = (time.monotonic() - t0) * 1000.0
        log.debug(
            "Embedded %d syslog texts in %.1fms (%.1fms/text)",
            len(valid_pairs), latency_ms, latency_ms / max(1, len(valid_pairs)),
        )
        vectors = list(all_vectors)

    computed_at_ns = time.time_ns()
    embedding_records = []
    for pos, (i, eid, _text) in enumerate(valid_pairs):
        vec = vectors[pos] if pos < len(vectors) else None
        if vec is None:
            continue
        embedding_records.append(
            {
                "event_id": str(eid),
                "vector": vec.tolist(),
                "model_name": embedder.model_name,
                "model_revision": str(
                    getattr(embedder, "model_revision", "") or ""
                ),
                "dim": embedder.dim,
                "computed_at_ns": computed_at_ns,
                "event_type": event_types[i] if i < len(event_types) else "",
            }
        )
    if not embedding_records:
        return 0

    try:
        import requests
        resp = requests.post(
            f"{api_url}/api/events/embeddings",
            json={"embeddings": embedding_records},
            headers=auth_headers(),
            timeout=30,
        )
        resp.raise_for_status()
        if dedup is not None:
            dedup.metrics.events_posted += len(embedding_records)
        return len(embedding_records)
    except Exception as exc:
        log.error("Failed to POST event embeddings: %s", exc)
        return 0


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    api_url = os.environ.get("BONSAI_API_URL", DEFAULT_API_URL)
    run_syslog_embedding_worker(api_url=api_url)

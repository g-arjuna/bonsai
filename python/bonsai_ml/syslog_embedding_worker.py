"""Syslog text embedding background worker for Bonsai ML.

EV1-3 T1: Polls GET /api/events/unembedded?type=syslog&limit=200 for events
with needs_embedding=true. Batches them, calls TextEmbedder, and POSTs
results to POST /api/events/embeddings.

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
from typing import Any

log = logging.getLogger(__name__)

POLL_INTERVAL_SECS = 60
FETCH_LIMIT = 200
BACKOFF_SECS = [60, 120, 300]
DEFAULT_API_URL = "http://localhost:3000"


def run_syslog_embedding_worker(
    api_url: str = DEFAULT_API_URL,
    embedding_config: dict[str, Any] | None = None,
    poll_interval: int = POLL_INTERVAL_SECS,
    run_once: bool = False,
) -> None:
    """Run the syslog embedding background worker loop.

    Args:
        api_url: Bonsai core API base URL.
        embedding_config: Dict passed to load_from_config(). None = defaults.
        poll_interval: Seconds between polling cycles.
        run_once: If True, run one cycle and exit (for testing/cron use).
    """
    from .text_embeddings import load_from_config, EmbeddingConfig

    embedder = load_from_config(embedding_config or {})
    log.info(
        "SyslogEmbeddingWorker: starting (model=%s, poll_interval=%ds)",
        embedder.model_name, poll_interval,
    )

    backoff_idx = 0
    total_embedded = 0

    while True:
        try:
            events = _fetch_unembedded_events(api_url, limit=FETCH_LIMIT)

            if events:
                embedded_count = _embed_and_post(api_url, events, embedder)
                total_embedded += embedded_count
                log.info(
                    "SyslogEmbeddingWorker: embedded %d events (total=%d, model=%s)",
                    embedded_count, total_embedded, embedder.model_name,
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


def _fetch_unembedded_events(api_url: str, limit: int) -> list[dict[str, Any]]:
    try:
        import requests
        resp = requests.get(
            f"{api_url}/api/events/unembedded",
            params={"type": "syslog", "limit": limit},
            timeout=15,
        )
        resp.raise_for_status()
        return resp.json()
    except Exception as exc:
        log.debug("Failed to fetch unembedded events: %s", exc)
        return []


def _embed_and_post(
    api_url: str,
    events: list[dict[str, Any]],
    embedder: Any,
) -> int:
    import numpy as np

    texts = [e.get("message") or e.get("reason") or "" for e in events]
    event_ids = [e.get("id") or e.get("event_id") for e in events]

    valid_pairs = [
        (eid, text)
        for eid, text in zip(event_ids, texts)
        if eid and text
    ]
    if not valid_pairs:
        return 0

    valid_ids, valid_texts = zip(*valid_pairs)

    t0 = time.monotonic()
    vectors = embedder.embed_batch(list(valid_texts))
    latency_ms = (time.monotonic() - t0) * 1000.0

    log.debug(
        "Embedded %d syslog texts in %.1fms (%.1fms/text)",
        len(valid_texts), latency_ms, latency_ms / max(1, len(valid_texts)),
    )

    computed_at_ns = time.time_ns()
    embedding_records = [
        {
            "event_id": str(eid),
            "vector": vectors[i].tolist(),
            "model_name": embedder.model_name,
            "dim": embedder.dim,
            "computed_at_ns": computed_at_ns,
        }
        for i, eid in enumerate(valid_ids)
    ]

    try:
        import requests
        resp = requests.post(
            f"{api_url}/api/events/embeddings",
            json={"embeddings": embedding_records},
            timeout=30,
        )
        resp.raise_for_status()
        return len(embedding_records)
    except Exception as exc:
        log.error("Failed to POST event embeddings: %s", exc)
        return 0


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    api_url = os.environ.get("BONSAI_API_URL", DEFAULT_API_URL)
    run_syslog_embedding_worker(api_url=api_url)

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

# WP65-12: embedding record-schema identity sent with every write-back.
# Combined with model_name/model_revision/normalization_version/dim this
# forms the coherent versioned semantic identity persisted by core.
EMBEDDING_SCHEMA_ID = "event-embedding-v1"
# Legacy (dedup-disabled) path embeds raw text — no template
# normalization is applied, which is itself provenance.
LEGACY_NORMALIZATION_VERSION = "none"

# Most recent dedup stats snapshot, for ops introspection and tests.
_LAST_DEDUP_STATS: dict[str, Any] = {}

# WP65-20/22: outcome of the most recent worker cycle, for ops
# introspection and tests. Statuses: empty | fetch_failed |
# provider_error | contended | degraded | invalid_only | post_failed |
# ack_invalid | partial | success | no_actionable | error.
_LAST_CYCLE: dict[str, Any] = {}

#: Cycle statuses that represent an operational failure — the scheduler
#: wrapper converts these into job failures so a broken fetch/provider/
#: acknowledgement can never masquerade as a green empty cycle.
FAILURE_STATUSES = frozenset(
    {"fetch_failed", "provider_error", "post_failed", "ack_invalid"}
)


def get_dedup_stats() -> dict[str, Any]:
    """Return the most recent template-dedup metrics snapshot (or {})."""
    return dict(_LAST_DEDUP_STATS)


def get_last_cycle() -> dict[str, Any]:
    """Return the most recent embedding-worker cycle outcome (or {})."""
    return dict(_LAST_CYCLE)


def _record_cycle(outcome: dict[str, Any]) -> dict[str, Any]:
    _LAST_CYCLE.clear()
    _LAST_CYCLE.update(outcome)
    return dict(outcome)


def run_syslog_embedding_worker(
    api_url: str = DEFAULT_API_URL,
    embedding_config: dict[str, Any] | None = None,
    poll_interval: int = POLL_INTERVAL_SECS,
    run_once: bool = False,
    dedup: Any = None,
) -> Optional[dict[str, Any]]:
    """Run the syslog embedding background worker loop.

    WP65-20: when ``run_once`` is true the cycle outcome dict is
    returned — ``status`` is one of ``empty``, ``fetch_failed``,
    ``provider_error``, ``post_failed``, ``ack_invalid``, ``partial`` or
    ``success`` — so a scheduler/cron caller can distinguish a genuine
    empty queue from HTTP, provider and acknowledgement failures. The
    same outcome is always published via :func:`get_last_cycle`.

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
    last_outcome: dict[str, Any] = {}

    try:
        while True:
            try:
                events = _fetch_unembedded_events(api_url, limit=FETCH_LIMIT)

                if events:
                    post_report: dict[str, Any] = {}
                    embedded_count = _embed_and_post(
                        api_url, events, embedder, dedup, report=post_report
                    )
                    total_embedded += embedded_count
                    fetched = len(events)
                    status = post_report.get("status", "success")
                    last_outcome = _record_cycle(
                        {
                            "status": status,
                            "fetched": fetched,
                            "embedded": embedded_count,
                            "total_embedded": total_embedded,
                            "detail": post_report.get("detail", ""),
                        }
                    )
                    log.info(
                        "SyslogEmbeddingWorker: cycle %s — fetched=%d "
                        "embedded=%d (total=%d, model=%s)",
                        status, fetched, embedded_count, total_embedded,
                        embedder.model_name,
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
                    last_outcome = _record_cycle(
                        {"status": "empty", "fetched": 0, "embedded": 0,
                         "total_embedded": total_embedded, "detail": ""}
                    )
                    log.debug("SyslogEmbeddingWorker: no unembedded events found")

            except _FetchFailed as exc:
                # WP65-20: a fetch failure is NOT an empty queue — surface
                # it as an explicit failed cycle, never a false-green idle.
                last_outcome = _record_cycle(
                    {"status": "fetch_failed", "fetched": 0, "embedded": 0,
                     "total_embedded": total_embedded,
                     "detail": f"{type(exc).__name__}: {exc}"}
                )
                log.error("SyslogEmbeddingWorker: fetch failed: %s", exc)
                if run_once:
                    return last_outcome
                sleep = BACKOFF_SECS[min(backoff_idx, len(BACKOFF_SECS) - 1)]
                backoff_idx += 1
                time.sleep(sleep)
                continue
            except Exception as exc:
                last_outcome = _record_cycle(
                    {"status": "error", "fetched": 0, "embedded": 0,
                     "total_embedded": total_embedded,
                     "detail": f"{type(exc).__name__}: {exc}"}
                )
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
                return last_outcome

            time.sleep(poll_interval)
    finally:
        # run_once callers (e.g. the 60s job-engine schedule) open a fresh
        # registry per invocation; close it so connections do not leak.
        if owns_dedup and dedup is not None:
            try:
                dedup.registry.close()
            except Exception:
                pass


class _FetchFailed(Exception):
    """The unembedded-events fetch failed (HTTP/auth/network/parse).

    WP65-20: distinct from a genuinely empty queue — callers must never
    report a fetch failure as an idle cycle.
    """


def _fetch_unembedded_events(api_url: str, limit: int) -> list[dict[str, Any]]:
    import requests
    try:
        resp = requests.get(
            f"{api_url}/api/events/unembedded",
            params={"type": "syslog", "limit": limit},
            headers=auth_headers(),
            timeout=15,
        )
        resp.raise_for_status()
        data = resp.json()
    except Exception as exc:
        raise _FetchFailed(f"unembedded fetch failed: {exc}") from exc
    events = data.get("events", data) if isinstance(data, dict) else data
    if not isinstance(events, list):
        raise _FetchFailed("unembedded fetch returned malformed payload")
    return events


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


_ACK_STATUSES = frozenset({"accepted", "already_existing", "rejected"})


def _is_int(v: Any) -> bool:
    return isinstance(v, int) and not isinstance(v, bool)


def _validate_embedding_ack(
    body: Any, records: list[dict[str, Any]], written: int
) -> Optional[list[int]]:
    """Fully validate a write-back acknowledgement (WP65-18).

    Returns the list of submitted-record indexes the server marked
    ``accepted``, or ``None`` when the acknowledgement is malformed or
    internally inconsistent — in which case NO occurrence bookkeeping
    may be committed.
    """
    if not isinstance(body, dict):
        return None
    items = body.get("items")
    if not isinstance(items, list) or len(items) != len(records):
        return None
    accepted: list[int] = []
    seen: set[int] = set()
    status_counts = {"accepted": 0, "already_existing": 0, "rejected": 0}
    for item in items:
        if not isinstance(item, dict):
            return None
        idx = item.get("index")
        if not _is_int(idx) or idx < 0 or idx >= len(records) or idx in seen:
            return None
        seen.add(idx)
        status = item.get("status")
        if status not in _ACK_STATUSES:
            return None
        status_counts[status] += 1
        if item.get("event_id") != records[idx].get("event_id"):
            return None
        if status == "accepted":
            accepted.append(idx)
    # Every submitted item must have exactly one attributable status.
    if len(seen) != len(records):
        return None
    # Aggregate counts must reconcile with the per-item statuses.
    for field in ("accepted", "already_existing", "persisted", "submitted"):
        v = body.get(field)
        if not _is_int(v) or v < 0:
            return None
    rejected_list = body.get("rejected")
    if not isinstance(rejected_list, list):
        return None
    if body["accepted"] != status_counts["accepted"]:
        return None
    if body["already_existing"] != status_counts["already_existing"]:
        return None
    if body["persisted"] != status_counts["accepted"] + status_counts[
        "already_existing"
    ]:
        return None
    if body["persisted"] != written:
        return None
    if body["submitted"] != len(records):
        return None
    if len(rejected_list) != status_counts["rejected"]:
        return None
    return accepted


def _embed_and_post(
    api_url: str,
    events: list[dict[str, Any]],
    embedder: Any,
    dedup: Any = None,
    report: Optional[dict[str, Any]] = None,
) -> int:
    """Embed a batch and POST write-backs. Returns records persisted.

    WP65-20/22: when ``report`` is given it is populated with the cycle
    outcome — ``status`` is one of ``no_actionable``, ``provider_error``,
    ``contended``, ``degraded``, ``invalid_only``, ``post_failed``,
    ``ack_invalid``, ``partial`` or ``success`` — plus ``detail`` and
    per-class counts, so callers can distinguish a genuine empty queue
    from HTTP/provider/acknowledgement failures and partial work.
    """
    def _out(status: str, detail: str = "", **counts: Any) -> int:
        if report is not None:
            report.clear()
            report.update({"status": status, "detail": detail, **counts})
        return 0

    texts = [_extract_event_text(e) for e in events]
    event_ids = [e.get("id") or e.get("event_id") for e in events]
    event_types = [e.get("event_type") or e.get("type") or "" for e in events]
    # WP65-17: per-event processing-generation claim echoed from the
    # unembedded fetch — the server applies lifecycle reports only
    # while the stored attempt count still equals this claim, so
    # duplicate deliveries and stale reports cannot double-consume
    # attempts or alter completed enrichment.
    claims = {
        str(e.get("id") or e.get("event_id")): e.get("claim_attempts")
        for e in events
        if e.get("id") or e.get("event_id")
    }
    # WP65-21: the fetch-acquired processing-generation token. When the
    # server returns one it is authoritative — a superseded fetch's
    # reports are fenced out even if the attempt count still matches.
    claim_tokens = {
        str(e.get("id") or e.get("event_id")): e.get("claim_token")
        for e in events
        if e.get("id") or e.get("event_id")
    }

    # WP65-14: events with an id but no extractable text can never be
    # enriched — report them permanently invalid instead of letting them
    # occupy the oldest-first fetch forever.
    invalid_reports = [
        {
            "event_id": str(eid),
            "reason": "no_extractable_text",
            "claim_attempts": claims.get(str(eid)),
            "claim_token": claim_tokens.get(str(eid)),
        }
        for eid, text in zip(event_ids, texts)
        if eid and not (text or "").strip()
    ]

    valid_pairs = [
        (i, eid, text)
        for i, (eid, text) in enumerate(zip(event_ids, texts))
        if eid and text and text.strip()
    ]
    if not valid_pairs and not invalid_reports:
        return _out("no_actionable", events=len(events))

    # vectors[i] aligned with valid_pairs, or None for unresolved events.
    if dedup is not None:
        valid_texts = [p[2] for p in valid_pairs]
        vectors = dedup.resolve_batch(valid_texts)
        resolution_keys = dedup.last_resolution_keys()
        # WP65-22: per-item unresolved reason ("pending" = template
        # claimed by another worker → claim contention, reschedules
        # without consuming an attempt; anything else is a provider/
        # processing failure that must consume a bounded attempt).
        unresolved_states = dedup.last_unresolved_states()
        normalization_version = str(getattr(dedup, "norm_version", "") or "")
    else:
        from .message_templates import _validate_vector

        resolution_keys = []
        unresolved_states = []
        normalization_version = LEGACY_NORMALIZATION_VERSION
        t0 = time.monotonic()
        try:
            all_vectors = embedder.embed_batch([p[2] for p in valid_pairs])
        except Exception as exc:
            log.error("Provider embed_batch failed: %s", exc)
            all_vectors = None
        latency_ms = (time.monotonic() - t0) * 1000.0
        log.debug(
            "Embedded %d syslog texts in %.1fms (%.1fms/text)",
            len(valid_pairs), latency_ms, latency_ms / max(1, len(valid_pairs)),
        )
        dim = int(getattr(embedder, "dim", 0) or 0)
        # WP65 provider contract (C7): the response must have EXACT
        # cardinality aligned with the request — a short/long/missing
        # response is a provider failure for the whole batch, never a
        # partial success. Each vector must pass the same validation as
        # the dedup path: dim, finite, non-zero norm — zero-vector
        # provider fallbacks are never persisted as enrichment.
        if all_vectors is None or len(all_vectors) != len(valid_pairs):
            log.error(
                "Provider returned %s vectors for %d texts — rejecting batch",
                "no" if all_vectors is None else len(all_vectors),
                len(valid_pairs),
            )
            vectors = [None] * len(valid_pairs)
        else:
            vectors = [
                v if _validate_vector(v, dim) else None for v in all_vectors
            ]

    computed_at_ns = time.time_ns()
    embedding_records = []
    # record index -> position in valid_pairs (for ack attribution)
    record_pairs: list[int] = []
    for pos, (i, eid, _text) in enumerate(valid_pairs):
        vec = vectors[pos] if pos < len(vectors) else None
        if vec is None:
            continue
        record_pairs.append(pos)
        embedding_records.append(
            {
                "event_id": str(eid),
                "vector": vec.tolist(),
                "model_name": embedder.model_name,
                "model_revision": str(
                    getattr(embedder, "model_revision", "") or ""
                ),
                "dim": embedder.dim,
                "normalization_version": normalization_version,
                "schema_hash": EMBEDDING_SCHEMA_ID,
                "computed_at_ns": computed_at_ns,
                # Authoritative event_type is read from the persisted
                # graph event server-side; this is informational only.
                "event_type": event_types[i] if i < len(event_types) else "",
                # WP65-21: echo the processing claim so lifecycle
                # completion is fenced to the fetch generation that
                # acquired it — a stale worker cannot close a newer
                # obligation even with a successful write-back.
                "claim_attempts": claims.get(str(eid)),
                "claim_token": claim_tokens.get(str(eid)),
            }
        )

    # WP65-14: unresolved events are reported deferred so core applies a
    # bounded retry deadline — failed/backed-off events cannot starve
    # healthy events in the oldest-first fetch.
    deferred_reports = [
        {
            "event_id": str(valid_pairs[pos][1]),
            "claim_attempts": claims.get(str(valid_pairs[pos][1])),
            "claim_token": claim_tokens.get(str(valid_pairs[pos][1])),
            # WP65-22: unresolved items are classified truthfully —
            # template claim contention reschedules WITHOUT consuming
            # an attempt; provider/backoff/new-claim failures consume
            # one bounded attempt so retry exhaustion stays reachable.
            "kind": (
                "claim_contention"
                if pos < len(unresolved_states)
                and unresolved_states[pos] == "pending"
                else "provider_error"
            ),
        }
        for pos in range(len(valid_pairs))
        if pos >= len(vectors) or vectors[pos] is None
    ]

    if not embedding_records and not deferred_reports and not invalid_reports:
        return _out("no_actionable", events=len(events))

    # WP65-20: provider_error = events were fetched and valid, but the
    # provider/template path resolved ZERO embedding records — every one
    # is reported deferred. Distinct from post/ack failures.
    # WP65-22: pure claim contention is NOT a provider failure — every
    # deferred item must be classified before deciding.
    contention_only = bool(deferred_reports) and all(
        d.get("kind") == "claim_contention" for d in deferred_reports
    )
    provider_failed = (
        bool(deferred_reports)
        and not embedding_records
        and not contention_only
    )
    counts = {
        "embedding_records": len(embedding_records),
        "deferred_reports": len(deferred_reports),
        "invalid_reports": len(invalid_reports),
    }

    try:
        import requests
        payload: dict[str, Any] = {"embeddings": embedding_records}
        if deferred_reports:
            payload["deferred"] = deferred_reports
        if invalid_reports:
            payload["invalid"] = invalid_reports
        resp = requests.post(
            f"{api_url}/api/events/embeddings",
            json=payload,
            headers=auth_headers(),
            timeout=30,
        )
        resp.raise_for_status()
        # Write-back acknowledgement contract (WP65-03/13): the server
        # must return explicit attributable counts AND per-item status.
        # A missing or malformed acknowledgement is a FAILURE — never
        # manufacture a successful count from the submitted batch size.
        try:
            body = resp.json()
        except Exception:
            log.error("Embedding write-back returned non-JSON acknowledgement")
            return _out("ack_invalid", "non-JSON acknowledgement", **counts)
        persisted = body.get("persisted") if isinstance(body, dict) else None
        if persisted is None:
            # Legacy "written" field is accepted only when it is a real
            # integer report, not a fabricated default.
            persisted = body.get("written") if isinstance(body, dict) else None
        if not isinstance(persisted, int) or isinstance(persisted, bool) \
                or persisted < 0 or persisted > len(embedding_records):
            log.error(
                "Embedding write-back acknowledgement malformed: %r",
                body,
            )
            return _out("ack_invalid", "malformed acknowledgement", **counts)
        written = persisted
        if written != len(embedding_records):
            rejected = body.get("rejected") if isinstance(body, dict) else None
            log.warning(
                "Embedding write-back partial: server persisted %d of %d "
                "submitted records (rejected=%d)",
                written, len(embedding_records),
                len(rejected) if isinstance(rejected, list) else -1,
            )
        # WP65-13/18: occurrence commits are attributed per-item. Only
        # records the server acknowledged as "accepted" (newly persisted
        # embedding identities) count as new template occurrences —
        # "already_existing" replays and rejected items never inflate
        # template_occurrences. The acknowledgement must FULLY validate
        # before any occurrence bookkeeping: every submitted item needs
        # exactly one attributable status with a valid unique non-
        # Boolean index, matching event_id and a recognized status, and
        # the aggregate counts must reconcile. An invalid acknowledgement
        # commits NO occurrences (registry counts are enrichment
        # bookkeeping, not operational event authority — a lost ack can
        # undercount, which is documented and acceptable).
        # WP65-22: full acknowledgement validation applies to EVERY
        # request shape — dedup on/off, embeddings-only, deferred-only,
        # invalid-only and mixed. Occurrence commits remain dedup-only
        # (registry bookkeeping), but a malformed ack is ack_invalid
        # regardless of whether dedup is enabled.
        ack_fully_valid = True
        accepted_indexes = (
            _validate_embedding_ack(body, embedding_records, written)
            if embedding_records
            else []
        )
        if embedding_records and accepted_indexes is None:
            ack_fully_valid = False
            log.warning(
                "Embedding write-back acknowledgement failed full "
                "validation; occurrence counts not committed"
            )
        # Lifecycle report outcomes: the server reports applied vs
        # skipped per category — surface the AUTHORITATIVE applied
        # counts, not just what we submitted. A missing/malformed
        # lifecycle block when reports were sent invalidates the ack.
        lifecycle_ok = True
        for key, submitted in (
            ("deferred", deferred_reports),
            ("invalid", invalid_reports),
        ):
            if submitted:
                applied = body.get(key)
                skipped = body.get(f"{key}_skipped")
                # Applied/skipped must be nonnegative integers that
                # reconcile EXACTLY with the submitted report count —
                # every submitted item lands in exactly one bucket.
                if (
                    not isinstance(applied, int)
                    or isinstance(applied, bool)
                    or applied < 0
                    or not isinstance(skipped, int)
                    or isinstance(skipped, bool)
                    or skipped < 0
                    or applied + skipped != len(submitted)
                ):
                    lifecycle_ok = False
                else:
                    counts[f"{key}_applied"] = applied
                    counts[f"{key}_skipped"] = skipped
        if not lifecycle_ok:
            ack_fully_valid = False
            log.warning(
                "Embedding write-back lifecycle acknowledgement "
                "malformed; applied/skipped counts unavailable"
            )
        # Occurrence bookkeeping commits only when the OVERALL ack is
        # valid — a malformed lifecycle block invalidates the whole
        # acknowledgement even if per-item embedding statuses parsed.
        if ack_fully_valid and dedup is not None and accepted_indexes:
            accepted_keys: list[Optional[str]] = []
            for rec_idx in accepted_indexes:
                pair_pos = record_pairs[rec_idx]
                key = (
                    resolution_keys[pair_pos]
                    if pair_pos < len(resolution_keys)
                    else None
                )
                accepted_keys.append(key)
            if accepted_keys:
                dedup.commit_occurrences(accepted_keys)
        if dedup is not None:
            dedup.metrics.events_posted += written
        # WP65-20/22 cycle status, in precedence order: an acknowledgement
        # we could not fully validate is the most severe (bookkeeping is
        # uncommitted even if writes landed); then a partial write; then
        # provider failure vs pure claim contention vs invalid-only when
        # nothing resolved; then a mixed batch that wrote some records
        # while others were deferred (degraded — never false-green
        # success); otherwise a clean cycle.
        if not ack_fully_valid:
            status, detail = "ack_invalid", "acknowledgement failed full validation"
        elif written < len(embedding_records):
            status, detail = "partial", (
                f"persisted {written} of {len(embedding_records)} records"
            )
        elif provider_failed:
            status, detail = "provider_error", "no embedding records resolved"
        elif not embedding_records and deferred_reports and contention_only:
            status, detail = "contended", (
                f"{len(deferred_reports)} events deferred on template claims"
            )
        elif embedding_records and deferred_reports:
            status, detail = "degraded", (
                f"persisted {written} records; {len(deferred_reports)} deferred"
            )
        elif not embedding_records and not deferred_reports and invalid_reports:
            status, detail = "invalid_only", (
                f"{len(invalid_reports)} events closed as invalid"
            )
        else:
            status, detail = "success", ""
        if report is not None:
            report.clear()
            report.update(
                {"status": status, "detail": detail, "written": written,
                 **counts}
            )
        return written
    except Exception as exc:
        log.error("Failed to POST event embeddings: %s", exc)
        return _out("post_failed", f"{type(exc).__name__}: {exc}", **counts)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    api_url = os.environ.get("BONSAI_API_URL", DEFAULT_API_URL)
    run_syslog_embedding_worker(api_url=api_url)

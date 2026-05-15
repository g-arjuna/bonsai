# Bonsai Code Review Discipline

> Authored DV1 D3-T5 — 2026-05-15. Addresses the failure mode that let F-1
> (HTTP bind panic) survive CV1–CV6 reviews uncaught. Referenced from
> [`docs/CANONICAL.md`](CANONICAL.md).

---

## Why this document exists

The chunked review of 2026-05-15 found F-1 (critical: HTTP bind panic inside
`tokio::spawn`, JoinHandle discarded) and F-2 (critical: sidecar never starts
because `/health` never responds due to F-1). Prior reviews — CV1 through CV6 —
missed both. The cause is diagnosable: reviews scanned for surface-level code
deltas rather than walking integration boundaries. They counted lines changed,
not events flowing.

This document encodes the discipline that would have caught F-1 on first review.

---

## The chunked-review pattern

Read the codebase in deliberate sections, not as a stream. For each section:

1. **Name observations as they arise.** Don't buffer them for a summary at the
   end — the summary step introduces normalisation pressure that softens findings.
2. **Finish a section before moving to the next.** Skipping to conclusions
   produces a finding list that reflects what the reviewer *expected* to find,
   not what is actually there.
3. **Consolidate findings before authoring a backlog.** A finding that is
   unclear after consolidation is not ready to be a task.

The 2026-05-15 review used this pattern across seven chunks. Chunks 1–5 were
observational. The backlog was authored only after Chunk 7 (validation log
tails) confirmed the findings operationally.

---

## Walk the data flow, not the diff

For each major subsystem, follow a real event from ingestion through detection
through output. This is the only review method that catches integration
regressions — per-module reviews can each pass while the integration silently
breaks.

**For bonsai, the canonical data-flow walk is:**

1. A gNMI `SubscribeResponse` arrives at the collector.
2. `ingest::run_core_forwarder` (or `run_collector_manager`) deserialises it
   and publishes to `InProcessBus`.
3. `write_coordinator` picks it up, applies it to LadybugDB, emits a
   `StateChangeEvent` on `store.subscribe_events()`.
4. The Python rules sidecar's `StreamEvents` gRPC stream delivers the event.
5. The sidecar evaluates the matching rule, calls `CreateDetection`.
6. `GraphStore::write_detection` persists the row.
7. Output adapters pick it up from the Detection table on their polling loop.

Ask: *is there any point in this chain that can silently fail, and would I see
that failure in the validation report?*

F-1 was at step 3½ — between bonsai startup and the first event arriving. The
HTTP server bind happened inside a spawned task, so its panic never surfaced.
Nobody walked that path; they read main.rs top-to-bottom as a list of
initialisations rather than as a lifecycle with failure modes.

---

## Log tail beats summary verdict

When investigating an operational failure, read the side-channel logs before
reading the summary verdict. The summary verdict is produced after the logs;
it interprets them. The interpretation can be wrong. The logs cannot be wrong.

**The right order for a validation report review:**

1. Read `scripts/ops/rebuild_and_validate.sh` to understand what steps exist.
2. Read `.logs/` for the last validation run: `bonsai.log`, `sidecar.log`,
   `validate.log`.
3. *Then* read the summary verdict and check whether it matches the logs.

F-2 was diagnosed by reading `scripts/ops/start_bonsai_with_sidecar.sh` and
noticing that the wrapper waits for `/health` before starting the sidecar.
That single fact explained why `bonsai-sidecar.log` was always empty — the
wrapper never reached the sidecar start line.

---

## Checklist for AI agents reviewing bonsai code drops

Before submitting findings on any bonsai code review, confirm each item:

- [ ] **Did you read `src/main.rs`'s HTTP bind site?**
  Look for `TcpListener::bind` — is it inside `tokio::spawn` or on the main
  task? If inside spawn with `.expect()`, it is F-1 class bug.

- [ ] **Did you read at least one validation report's `.logs/` directory?**
  Not just the summary — the actual log files. `bonsai.log` and
  `bonsai-sidecar.log` in particular.

- [ ] **Did you grep for `tokio::spawn` without a stored JoinHandle?**
  `grep -n "tokio::spawn" src/main.rs` and check that every spawn either stores
  the handle or is genuinely fire-and-forget (one-way background task that
  should never stop).

- [ ] **Did you follow one event from gNMI arrival to detection?**
  Trace through `ingest.rs → write_coordinator.rs → store.subscribe_events() →
  sidecar → CreateDetection`. Any break in this chain is a silent "Detections: 0"
  regression.

- [ ] **Did you check `/api/sidecars` in the validation run logs?**
  A registered + heartbeating sidecar is the operational fact. A running python
  process is not sufficient — it must have completed `RegisterSidecar`.

- [ ] **For files over 2000 lines: did you read the structural map first?**
  `src/http_server.rs` (7,779 lines) and `src/main.rs` (2,515 lines before DV1
  D3-T1) cannot be read as flat text. Map the sections first, then dive into
  the integration boundaries.

---

## Structural red flags (instant escalation)

These patterns in source code are always worth a direct comment in the backlog:

| Pattern | Risk | Reference finding |
|---|---|---|
| `tokio::spawn(async move { ... .expect(...) })` with handle discarded | Panic kills spawned task silently; parent process unaffected | F-1 |
| Wrapper script that waits on HTTP before starting a dependency | Circular dependency if HTTP is broken | F-2 |
| File over 2000 lines with mixed concerns (lifecycle + handlers + CLI + tests) | AI code review misses integration boundaries; human review becomes impractical | F-4 |
| Unconditional `.expect()`/`.unwrap()` in async-spawned task | Any error at that point silently kills the task | F-1 class |
| Feature with "0 detections" symptom but passing per-subsystem tests | Integration gap; per-component test suite is not sufficient | F-1, F-2, F-3 |

---

## Cross-references

- Backlog that drove this: [`BONSAI_CONSOLIDATED_BACKLOG_DV1.md`](../BONSAI_CONSOLIDATED_BACKLOG_DV1.md#tier-d3)
- The chunked review that found F-1/F-2: [`CHUNKED_CODE_REVIEW_2026-05-15.md`](../CHUNKED_CODE_REVIEW_2026-05-15.md)
- Canonical entry point: [`docs/CANONICAL.md`](CANONICAL.md)
- Sidecar architecture: [`docs/architecture/sidecars.md`](architecture/sidecars.md)
- The fix: `src/main.rs` D1-T1 change (DV1, 2026-05-15)

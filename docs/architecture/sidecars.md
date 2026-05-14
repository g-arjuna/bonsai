# Bonsai Sidecars — Architectural Reference

> Authored CV7 Tier 4 — 2026-05-14. Supersedes the deleted `detection_paths.md`
> draft from earlier the same day, after the operator and I dug into the
> runtime model and decided the right answer was visibility, not parallel
> logic. The full reasoning is in [`DECISIONS.md`](../../DECISIONS.md) under
> "2026-05-14 — CV7 T4: Retire Rust event-detection fastpath."

---

## What a sidecar is

A **sidecar** is a Python process that runs alongside the bonsai Rust core,
talks to it over gRPC, and provides functionality the Rust core delegates
out — today that means detection (rules + ML inference); over time it will
mean syslog parsing offload, GNN training, and whatever else benefits from
Python's ecosystem.

Sidecars are **not** subprocesses of bonsai. Bonsai does not start them.
The operator (or systemd, or the laptop startup wrapper) starts them.
Bonsai's job is to **know which sidecars are bound** and surface that as a
first-class operational fact.

---

## The sidecar types

| `kind` | Process | Owns | Status today |
|---|---|---|---|
| `rules` | `python/collector_engine.py` | Evaluating the 18 rule_ids from `RULE_CATALOGUE`. Writes `DetectionEvent` rows via `CreateDetection`. | **Live and required for any detection to fire.** |
| `ml-inference` | (currently bundled into `rules`) | IsolationForest + GBT model serving. Loads `models/*.joblib`. | Bundled today; may split out in CV8 for memory isolation. |
| `syslog-parser` | (currently in Rust) | Vendor-pattern syslog parsing per `config/syslog_patterns/*.yaml`. | In Rust today; only split out if syslog volume warrants. |
| `gnn-trainer` | (planned) | Offline GNN training pipeline reading the parquet archive. Not in the live detection loop. | Not built — earliest CV8 or later when 30-day archive lands. |

The registry is **extensible**. New `kind` strings can be added without
changing the protocol.

---

## The registration protocol

Two RPCs on the existing `BonsaiGraph` gRPC service (defined in
[`proto/bonsai_service.proto`](../../proto/bonsai_service.proto)):

```proto
rpc RegisterSidecar(RegisterSidecarRequest) returns (RegisterSidecarResponse);
rpc Heartbeat(HeartbeatRequest)            returns (HeartbeatResponse);
```

**Lifecycle**:

1. Sidecar starts. Reads its config (env vars, default values).
2. Sidecar calls `RegisterSidecar(name, kind, version, capabilities, address)`. Receives `sidecar_id` (UUID).
3. Sidecar enters its main loop (e.g. `StreamEvents` consumption for `rules`).
4. Sidecar background thread calls `Heartbeat(sidecar_id, events_in_total, detections_out_total, status_json)` every **15 seconds**.
5. Bonsai marks an entry `stale` after **45s** (3× heartbeat) without an update, and `lost` after **120s**. Lost entries remain visible but flagged.
6. Re-registration (same `name+kind`) **replaces** the existing entry — sidecar restarts are explicit, not duplicate entries.
7. There is no explicit `Deregister` RPC; clean exit is "stop heartbeating." This matches systemd's failure model — if the process is gone, the registry sees it within 45s.

**Where the registry lives**: `src/sidecar_registry.rs` (new in CV7 T4-2).
In-memory `HashMap<sidecar_id, SidecarEntry>` behind `tokio::sync::RwLock`.
Bonsai does not persist the registry — it is rebuilt as sidecars heartbeat
in after a bonsai restart.

---

## Visibility surface

| Surface | Path | Consumer | Notes |
|---|---|---|---|
| HTTP API | `GET /api/sidecars` | bonpy UI, scripts, monitoring | JSON, includes `required_kinds` and `missing_required` |
| Health gate | `GET /health` | Load balancers, ops scripts | Returns `degraded` when `BONSAI_REQUIRE_SIDECAR` is unmet after grace period |
| **Bonpy UI** | `/bonpy/` (separate Svelte SPA at [`ui-bonpy/`](../../ui-bonpy/)) | Operator | Distinct from bonsai UI. Read-only in CV7 (sidecar registry, per-rule firing, ML model status). Rule editor / retraining / GNN console in CV8+. |
| Prometheus | `bonsai_sidecar_*` metrics (planned) | Grafana | Heartbeat lag, events_in_rate, detections_out_rate — deferred to CV8 |

**Why a separate UI (bonpy) instead of a tab in bonsai UI**: bonsai UI shows what bonsai sees — the live network graph, topology, events, trace timelines. Bonpy shows what Python/ML sidecars are doing — registry, rule firing, model status, eventually retraining controls and the GNN console. Bundling them conflates two distinct operator mental models. Separation also lets bonpy's interactivity grow into AIOps territory without dragging bonsai UI into a controller-style product (which the project guardrails explicitly reject). See the DECISIONS.md addendum 2026-05-14.

The `/health` gate is the most important. Setting
`BONSAI_REQUIRE_SIDECAR=rules` at process start tells bonsai "I am not
healthy unless a `rules` sidecar is bound." After a 60-second startup grace
window, `/health` returns `degraded` with `missing_required_sidecars:
["rules"]` until the sidecar registers. This makes the "Detections: 0"
failure mode operationally loud the moment it occurs, instead of silent for
days.

---

## What this replaces

CV6 introduced `src/event_detection.rs` — a 191-line Rust fastpath catching
three rule_ids (`bgp_session_down`, `bfd_session_down`, `interface_down`)
directly inside the Rust process. The CV7 T4 ADR retires it. The fastpath
solved the symptom (no detections appearing) but masked the disease (the
sidecar wasn't running and bonsai had no way to know). With sidecar
visibility, the disease is loud. Once Tier 2 codifies the sidecar's
startup and Tier 4 lands the visibility plumbing, `src/event_detection.rs`
is deleted in T4-7.

---

## Operator-facing rules

1. **A sidecar is supposed to be running. If it isn't, the UI says so.** No more "is detection working" mystery.
2. **The sidecar's source of truth is `/api/sidecars`.** Not "is the python process listed in `ps`." A registered + heartbeating sidecar is the operational fact.
3. **Required sidecars are configured at bonsai startup**, not in the sidecar itself. `BONSAI_REQUIRE_SIDECAR=rules` means "bonsai needs this to consider itself healthy."
4. **Restarts are atomic on cloud** (`BindsTo=bonsai.service` on the sidecar's unit). On laptop, the startup wrapper supervises both pids with `trap`-based termination.
5. **Multiple sidecars of the same kind are allowed** (e.g. two `rules` sidecars sharding by device). Each gets its own `sidecar_id`. The UI shows them as separate cards.

---

## Sequencing note

Tier 4's deletion of `src/event_detection.rs` is the **last** step. It does
not run until:

- T4-1 through T4-6 (this protocol + API + UI + health gate) are merged.
- Tier 2 amendments (startup wrapper for laptop, systemd units for cloud) are merged.
- A 1-hour live smoke shows the three retired rule_ids firing through the Python sidecar.

Deleting earlier regresses to the original "Detections: 0" gap with no
safety net. This is recorded explicitly so the operator (and any future AI
agent reading this doc) knows not to delete `event_detection.rs` opportunistically.

---

## Cross-references

- CV7 backlog Tier 4: [`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`](../../BONSAI_CONSOLIDATED_BACKLOG_CV7.md#tier-4)
- CV7 backlog Tier 2: [`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`](../../BONSAI_CONSOLIDATED_BACKLOG_CV7.md#tier-2)
- ADR: [`DECISIONS.md`](../../DECISIONS.md) — "2026-05-14 — CV7 T4: Retire Rust event-detection fastpath."
- Canonical orientation: [`docs/CANONICAL.md`](../CANONICAL.md)
- Sidecar implementation (Python): [`python/collector_engine.py`](../../python/collector_engine.py)
- Sidecar registry (Rust, to be authored): `src/sidecar_registry.rs`
- gRPC service definition: [`proto/bonsai_service.proto`](../../proto/bonsai_service.proto)

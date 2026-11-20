# M6C-1 — Device-effective YANG discovery and applicability

Date: 20 November 2026. Branch: `feat/bn015-m6c1-device-applicability`.
Base: `origin/main` `50055bb7bc0cd960a8ac0876177cf3249c7f6a1a`.

## Scope delivered

- Complete gNMI Capabilities evidence (`ModelData` name/organization/
  version, gNMI version, encodings, security mode, collector/target
  provenance) — additive proto fields; legacy consumers unchanged.
- RFC 8525 YANG Library discovery over bounded gNMI `Get`: NMDA
  `yang-library` module-set/schema/datastore parsing + legacy
  `/modules-state` fallback; strict bounds on response size, update
  count, JSON depth/bytes, module/feature/deviation counts; explicit
  outcome taxonomy — no failure class fabricates a module set.
- Deterministic resolver `src/yang_schema/applicability.rs`:
  tri-state feature expressions, exact revision/namespace/org
  identity, deviations, import-only, datastore linkage, registry-bound
  device identity, deterministic ordering + evidence fingerprint.
- Durable snapshots: `bonsai_config.db` schema v9
  (`yang_applicability_snapshots`), bounded payloads, freshness
  (24 h evidence age + active `schema_id`/`content_id` binding),
  16-entry history, read-time staleness — history never silently
  erased.
- API: `GET /api/devices/{address}/yang-applicability` (Viewer,
  bounded filters, truthful counts, never connects) and
  `POST .../refresh` (Admin; bounded capabilities+library probe via
  existing credential/transport policy; never Set, never Subscribe,
  never changes acquisition intent).

## Executed verification (this branch)

- `cargo test -p bonsai --test yang_device_applicability` —
  **12/12 PASS** (T1–T12: positive applicability, capabilities-only,
  revision/org conflicts, features+deviations, import-only+datastore,
  catalogue failure modes, parser bounds/outcomes, live mock-gNMI
  NMDA+legacy probe, identity isolation, persistence/restart/
  staleness/history bound, refresh persistence + zero Subscribe/Set,
  no-secret-payload).
- `cargo test -p bonsai --lib` auth RBAC unit tests — PASS
  (refresh=Admin, reads=Viewer).
- `cargo check -p bonsai --lib` — PASS.
- Canonical trusted-local gate `scripts/ci/run-local.sh --trusted
  l0 l1 l2 l3` on the committed production SHA — see gate log below.

## Not demonstrated / known limits

- **No real NOS YANG Library interop** — all device evidence is
  mock-gNMI; RFC 8525 parsing is fixture-verified only.
- **Remote-collector discovery unsupported** — refresh executes from
  Core over the registry-approved connection; devices reachable only
  from a remote collector are not discoverable (explicit limitation,
  handoff to M6C-2).
- Unsupported YANG semantics: device-advertised deviation modules cap
  affected paths at `unknown`
  (`deviation_semantics_unsupported`); `if-feature` evaluation is
  tri-state over device-reported feature sets.
- Applicability is **schema-level** — not transport-probed,
  subscribed, received or projected. Acquisition enforcement is
  M6C-2; UI is M6D.

## Gate-tested SHA / merge SHA

- Gate-tested production SHA: recorded in the PR body and final
  report (filled at gate completion).
- Merge SHA: recorded post-merge in the closure report.

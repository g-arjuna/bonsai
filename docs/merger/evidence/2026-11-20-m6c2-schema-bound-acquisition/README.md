# M6C-2 — Schema-bound acquisition planning and execution authority

Date: 20 November 2026. Branch: `feat/bn015-m6c2-schema-bound-acquisition`.
Gate-tested production SHA: recorded in "Gate" below.

## Scope delivered

- **Deterministic schema-plan evaluator** (`src/yang_schema/acquisition.rs`):
  consumes the verified compiled catalogue plus the freshest persisted
  M6C-1 applicability snapshot; explicit operator path selection only;
  fail-closed rejections for missing/stale/mismatched/ambiguous
  evidence, invalid paths, config data, unresolved list keys
  (`no_exact_schema_path_result`), mode/interval violations, duplicate
  or conflicting selections, module-revision mismatch and invalid
  origins. Produces deterministic `plan_id` + `binding_fingerprint`,
  per-path decisions, proposed subscriptions and cost estimates.
  Catalogue authority requires `status == "complete"`, a non-empty
  verified `schema_id`, zero diagnostics and bounded path count.
- **Server-side orchestration** (`src/schema_binding.rs`): read-only
  preview (catalogue state, active schema id, snapshot freshness +
  reasons, current binding, evaluation, binding state) and activation
  that re-evaluates against current state and requires the client to
  echo the exact server-computed `plan_id`/`binding_fingerprint`.
- **Durable authority**: `devices.schema_binding_json` (schema v11)
  plus `schema_bindings` policy/audit table; activation is one SQLite
  transaction with optimistic revision check; revocation marks policy
  `revoked` with reason and clears runtime binding; JSON fallback
  cannot provide schema-bound authority.
- **HTTP API**: `POST /api/devices/{address}/schema-binding/{preview,
  activate,revoke}` — preview is read-only; activate/revoke require
  authenticated Admin + non-empty subject; OpenAPI documented.
- **Wire contract**: additive `ManagedDevice` schema-binding fields and
  `SubscriptionLifecycleEvent` echo fields incl. dedicated
  `schema_binding_plan_id` (separate from the submitted-manifest
  `plan_id`); collectors below `SCHEMA_BOUND_CONTRACT_VERSION` never
  receive schema-bound assignments.
- **Collector/subscriber enforcement**: invalid/incomplete/mismatched
  bindings publish a rejected plan, emit failure lifecycle and idle —
  no Subscribe RPC, no automatic/capability-derived fallback. Explicit
  mode emits exactly the approved selected paths; no capability-derived
  broadening exists on that path.
- **Core enforcement** (`src/api.rs`): authenticated collector,
  current assignment, authority epoch/generation windows, exact
  durable-binding echo equality, dedicated binding plan id, and exact
  approved-manifest equality for schema-bound devices; forged, partial,
  broadened, stale, wrong-collector and revoked claims rejected;
  unbound devices carrying a binding echo rejected.
- **Time-driven evidence expiry — two layers** (`src/schema_binding.rs`,
  `src/api.rs`, `src/assignment.rs`, `src/server_startup.rs`):
  - *Authorization:* `binding_evidence_state` evaluates the APPROVED
    persisted applicability snapshot row (never the latest) at every
    execution/lifecycle decision — assignment build and lifecycle
    admission. Evidence older than `EVIDENCE_MAX_AGE_NS` (24 h),
    future-dated beyond skew tolerance, missing its observation
    timestamp, or unverifiable (missing row / fingerprint mismatch)
    is not authority: new executions cannot start and lifecycle
    evidence is rejected before any reconciler runs.
  - *Reconciliation:* `reconcile_schema_binding_expiry` runs once at
    startup and every `SCHEMA_BINDING_RECONCILE_INTERVAL` (60 s) in
    production (`run_schema_binding_expiry_reconciler`), revokes stale
    approvals through the fingerprint-guarded durable path
    (`revoke_schema_binding_if_current`, reasons `evidence_expired` /
    `evidence_unverifiable`) and resyncs the assigned collector so the
    obsolete subscriber is withdrawn through the established
    assignment mechanism. Disconnected collectors need no special
    handling — durable revocation is authoritative and the next sync
    fails closed.
  - Tests inject `SchemaBindingClock` (shared atomic cell) — the
    24-hour window is exercised deterministically, never via sleeps.
- **Reconciliation (schema/evidence change)**: explicit revocation;
  YANG catalogue compile/activate revokes bindings whose approved
  `schema_id` is not the newly active catalogue and resyncs
  collectors; applicability evidence refresh supersedes stale
  snapshots (freshness re-checked at every preview/activation).
- **Mutation boundary**: registry rejects generic add/update/assign
  changes to an approval-managed acquisition plan, manifest or
  collector assignment — HTTP, CLI and gRPC all traverse these
  registry functions.

## Executed verification (this branch)

Focused suites, all PASS on the pre-merge head:

- `cargo test --lib` — **479/479 PASS**, including:
  - `yang_schema::acquisition` 4/4 (deterministic positive
    evaluation, stale snapshot rejection, config-path exclusion,
    keyless list segment rejection — corrected keyed-list behaviour
    retained);
  - `schema_binding` 8/8 — 4 new expiry unit tests
    (`evidence_state_classifies_current_expired_and_unverifiable`,
    `reconcile_revokes_expired_and_spares_fresh_and_reapproved`,
    `reconcile_spares_current_evidence_and_newer_reapproval`,
    `reconcile_revokes_unverifiable_evidence`) plus existing
    binding/echo/atomic-activation tests;
  - `assignment::tests::schema_bound_assignment_requires_v5_and_roundtrips_binding`,
    subscriber schema-bound guard accept/reject cases.
- `cargo test --test yang_schema_bound_acquisition` — **6/6 PASS**
  (new file, ~2 000 lines, real production boundaries only):
  - `schema_bound_activation_to_revocation_e2e` — the required
    integrated scenario: integrity-verified compiled catalogue →
    mock-gNMI applicability refresh + persisted snapshot → explicit
    selection → real evaluator preview (activation permitted) → real
    activation + durable approval → collector registration →
    assignment carrying the exact approved binding (schema_id,
    snapshot_id, evidence/binding fingerprints, plan_id,
    approval_revision asserted on the wire) → real
    `handle_assignment_update` → actual device-facing gNMI
    `SubscribeRequest` captured and asserted: exactly 2 subscriptions,
    `/system/hostname` origin `ta` SAMPLE 10 s and
    `/interfaces/interface[name=eth0]/oper-status` origin `ta`
    ON_CHANGE — no capability fallback → real telemetry →
    authenticated lifecycle accepted with binding+manifest echo →
    coverage `observed` for both paths → forged manifests (added
    path, changed mode — self-consistent recomputed plan_id) both
    rejected by the authorized-manifest equality check → operator
    revocation → collector withdrawal → subscriber stopped, device
    stream closed, session authority retired, coverage superseded →
    stale telemetry with the previous session's real plan/session
    identity cannot resurrect coverage → replayed lifecycle rejected →
    zero gNMI `Set` requests → binding JSON contains no secrets.
  - `schema_bound_assignment_blocked_for_old_collector` — pre-v5
    collector receives no schema-bound assignment.
  - `schema_bound_expiry_withdraws_active_execution` — injected clock
    past the 24 h window: lifecycle rejected at the authorization
    layer before any reconciler; resync delivers explicit
    `schema_binding_evidence_expired` rejection; subscriber stopped;
    reconciler revokes durably (`evidence_expired`); reapproval with
    fresh evidence restores authority through the same path; old
    binding fingerprint can never echo authority again.
  - `schema_bound_expiry_disconnected_collector` — expiry with no
    collector registered still revokes durable authority; the next
    registration sync fails closed, no Subscribe RPC.
  - `schema_bound_expired_evidence_blocks_activation_and_restart` —
    stale latest snapshot blocks preview/activation; fresh evidence
    permits activation; new CollectorManager after expiry fails
    closed.
  - `schema_bound_expiry_failed_delivery_then_recovery` — revocation
    delivered while the assignment channel is dead still commits
    durable authority removal; collector reconnect receives the
    fail-closed state, no new Subscribe.
- `cargo test --test gnmi_assignment_lifecycle` — **14/14 PASS**
  (assignment fencing, withdrawal, credential rotation, epoch
  restart-safety regressions).
- `cargo test --test gnmi_subscription_e2e` — **6/6 PASS**
  (distributed coverage, manifest/assignment agreement, stale
  collector session fencing, authority renewal across Core restart —
  139 s).
- `cargo test --test yang_catalogue` — **42/42 PASS**.
- `cargo test --test yang_device_applicability` — **12/12 PASS**
  (M6C-1 suite; `t9` schema-version assertion updated to
  `CURRENT_SCHEMA_VERSION` — latent stale literal from the v11 bump).
- `cargo check -p bonsai --tests` equivalent — all targets compile.

## Not demonstrated / known limits

- Real-NOS YANG Library interop remains mock-gNMI only (inherited
  M6C-1 limitation); no live device writes were performed.
- Remote-collector-only applicability discovery remains unsupported
  (inherited M6C-1 limitation).
- Unsupported deviations and wildcard semantics remain explicit
  rejections — no silent broadening.
- Bonsai does not claim to detect device-side schema changes before
  it observes new evidence; expiry is bounded by the persisted
  evidence timestamps plus the 60 s reconciliation interval, and the
  authorization layer rejects stale authority immediately regardless
  of reconciler timing.
- M6D UI integration (schema browser, plan preview) is separate
  follow-up work; authenticated remote-collector discovery is a
  separate dependency.

## Gate

- Gate-tested production SHA: `13d0be58426aeda33c6ea6e3f7087c5355655fdd`
  (branch tip; a subsequent docs/dead-code cleanup commit carries no
  production-code delta).
- Gate command: `CARGO_BUILD_JOBS=2 scripts/ci/run-local.sh --trusted
  --shared-target-dir /home/arjuna/.cache/bonsai-ci/build-lane
  --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native l0 l1 l2 l3`
- Result: PASS — all four tiers on the trusted local lane
  (~30 min wall, CARGO_BUILD_JOBS=2).
  - L0 source checks: PASS (2 pre-existing fmt-drift files at base
    unchanged; all M6C-2 files formatted).
  - L1 compile/static: PASS (`cargo check` profile, bonsai lib + all
    test targets).
  - L2 behavioral: PASS — 479 lib tests (incl. `schema_binding` expiry
    unit tests, `api::schema_binding_tests` echo/forgery tests,
    `assignment` v5 contract tests), 32 integration targets incl.
    `yang_schema_bound_acquisition` 6/6, `yang_device_applicability`
    12/12, `yang_catalogue` 42/42, `gnmi_subscription_e2e`,
    `gnmi_assignment_lifecycle`, `bn021g`/`bn022` auth regressions.
  - L3 cross-component: PASS — 13 integration targets + python suites
    (259 passed / 3 skipped / 1 deselected; skips are pre-existing
    environment-gated cases, not M6C-2).
- Preliminary gate on `737702c` (pre-rustfmt): L0 FAIL on formatting
  only; superseded by the `13d0be5` run above.

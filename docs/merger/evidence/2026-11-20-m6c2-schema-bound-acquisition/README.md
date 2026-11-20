# M6C-2 — Schema-bound acquisition planning and execution authority

Date: 20 November 2026. Branch: `feat/bn015-m6c2-schema-bound-acquisition`.
Gate-tested production SHA: `455c423` (see "Gate" below).

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
  no Subscribe RPC, no automatic/capability-derived fallback.
- **Core enforcement** (`src/api.rs`): authenticated collector,
  current assignment, authority epoch/generation windows, exact
  durable-binding echo equality, dedicated binding plan id, and exact
  approved-manifest equality for schema-bound devices; forged, partial,
  broadened, stale, wrong-collector and revoked claims rejected;
  unbound devices carrying a binding echo rejected.
- **Reconciliation**: explicit revocation; YANG catalogue
  compile/activate revokes bindings whose approved `schema_id` is not
  the newly active catalogue and resyncs collectors; applicability
  evidence refresh supersedes stale snapshots (freshness re-checked at
  every preview/activation).
- **Mutation boundary**: registry rejects generic add/update/assign
  changes to an approval-managed acquisition plan, manifest or
  collector assignment.

## Executed verification (this branch, pre-commit `455c423`)

- `cargo test -p bonsai --lib yang_schema::acquisition` — **4/4 PASS**
  (deterministic positive evaluation incl. plan-id == submitted-manifest
  identity, stale snapshot rejection, config-path exclusion, keyless
  list segment rejection).
- `cargo test -p bonsai --lib schema_bound` — **4/4 PASS** (subscriber
  fail-closed guard accept/reject cases; v5 assignment contract
  round-trip + old-collector blocking).
- `cargo test -p bonsai --lib schema_binding` — **4/4 PASS** (dedicated
  binding plan id on lifecycle echo, forged/partial echo fail-closed,
  ingest proto echo without plan-id conflation, atomic revisioned
  revocable SQLite activation).
- `cargo check -p bonsai --tests` — **PASS** (all integration targets
  compile; `TargetConfig`/`ManagedDevice`/`SubscriptionLifecycleEvent`
  constructor fallout fixed).
- Canonical trusted-local gate `scripts/ci/run-local.sh --trusted
  --shared-target-dir /home/arjuna/.cache/bonsai-ci/build-lane
  --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native l0 l1 l2 l3`
  with `CARGO_BUILD_JOBS=2` on `455c423` — **see gate log below**.

## Not demonstrated / known limits

- **No dedicated wire-level E2E** for the full
  activation→assignment→Subscribe→lifecycle→coverage→revocation loop;
  each boundary is covered by focused unit/integration tests above and
  the M5.2B lifecycle suite remains green, but a single scripted
  schema-bound lifecycle scenario is follow-up work.
- **Time-based evidence expiry is enforced at approval time** (and on
  every preview/activation re-evaluation); there is no periodic sweeper
  that revokes an active binding when its snapshot ages past 24 h
  without a refresh/catalogue/evidence mutation.
- Remote-collector-only applicability discovery remains unsupported
  (inherited M6C-1 limitation).
- Real-NOS YANG Library interop remains mock-gNMI only (inherited).

## Gate

- Gate-tested production SHA: `455c423`
- Gate command: `CARGO_BUILD_JOBS=2 scripts/ci/run-local.sh --trusted
  --shared-target-dir /home/arjuna/.cache/bonsai-ci/build-lane
  --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native l0 l1 l2 l3`
- Result: PENDING — update this section with per-tier outcomes and wall
  time when the run completes.

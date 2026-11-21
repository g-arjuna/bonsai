# M6D — Integrated device-effective YANG browser and collection-plan workspace

Date: 21 November 2026. Branch: `feat/bn015-m6d-schema-workspace`.
Base: `origin/main` @ `183da0719c01b4f14aff49a7c1a072186faa386a`
(M6C-2 merge, PR #42). Gate-tested production SHA: recorded in "Gate"
below after the canonical L0–L3 run.

## Scope delivered

### Four integration corrections (M6D-A)

- **A1 — distinct instantiated list paths**
  (`src/yang_schema/acquisition.rs`): selection dedup now uses the
  full structured instantiated identity via `gnmi_path::canonical_key`
  (origin + ordered path elements + sorted key name/value pairs +
  mode + interval). `…/interface[name=Ethernet1]/…` and
  `…/interface[name=Ethernet2]/…` are distinct eligible selections;
  exact duplicates still follow the documented duplicate policy and
  same-concrete-path parameter conflicts are rejected. Wildcards and
  key-name validation from M6C-2 are unchanged. Regression tests:
  `two_distinct_list_instances_are_eligible`,
  `duplicate_concrete_path_rejected`.
- **A2 — preview configuration-revision concurrency**
  (`acquisition.rs`, `schema_binding.rs`, `registry.rs`,
  `http_server/device.rs`): preview exposes `config_revision` (the
  device `updated_at_ns` the evaluation ran against);
  `SchemaPlanRequest.expected_config_revision` is required on
  activation; the handler rejects a mismatch with
  `config_revision_mismatch`, and `ApiRegistry::activate_schema_binding`
  atomically compares the live registry revision before committing —
  any intervening device update (identity, collector, acquisition
  mode, paths, transport security, credentials) fails closed with 409
  instead of overwriting newer state. Regression tests:
  `stale_config_revision_rejects_evaluation` (unit),
  `stale_config_revision_rejects_activation`,
  `concurrent_registry_update_preserved` (registry).
- **A3 — gNMI origin vs provenance** (`ui/src/lib/DeviceDrawer.svelte`,
  `ui/src/lib/pathkey.js`, `ui/src/lib/gnmi.js`): manual path entry no
  longer writes `origin: 'manual_override'`; the operator supplies a
  genuine gNMI model origin (or blank for device default) and the
  selection source is recorded in `provenance`. Frontend selection
  identity (`selectionKey`) covers origin + instantiated path + mode +
  interval, matching backend exact-selection semantics — selections
  differing only in origin or interval can no longer collapse.
- **A4 — preview authorization** (`src/http_server/auth.rs`):
  `required_role` maps `POST …/schema-binding/preview` to Viewer
  (read-only evaluation: no mutation, no device RPC, no subscription
  change); activate/revoke and applicability refresh remain Admin.
  `/api/auth/me` exposes the authenticated subject + effective role
  for role-aware UI; backend enforcement is unchanged. Regression
  tests: `schema_preview_viewer_allowed`,
  `schema_activate_viewer_denied`, `schema_revoke_viewer_denied`.

### Reusable workspace (M6D-B–J)

- `ui/src/lib/SchemaCollectionWorkspace.svelte` — one canonical
  component mounted as the device drawer's **schema** tab
  (`DeviceDrawer.svelte`), reachable directly at
  `/#/devices/<address>/schema` and reused from Devices, Profiles and
  Onboarding entry points. No second planning authority: every decision
  comes from the backend evaluator.
- **Schema/evidence browser**: module list (name, revision,
  namespace, organization, implemented/import-only, device
  applicability, features, deviations, provenance, reason codes) and
  paginated path browser (canonical path, module/revision, kind,
  config/state, type, units, list keys, required features, status +
  reasons). Server-side filters: `status`, `module`, `kind`,
  `config`, `feature`, `q`, `limit`, `offset`; `paths_matching`
  reports truthful totals; the embedded snapshot never serializes the
  unpaginated path list.
- **Evidence truth**: freshness card distinguishes fresh/stale/
  missing/partial evidence, shows schema id, snapshot id, observation
  time, datastore, content-id, collector, provenance and expiration
  warnings. Capabilities-only evidence is labelled candidate-level —
  never a support claim. Refresh is an explicit Admin action that
  warns it may invalidate an active approval and reports
  `schema_binding_revoked` truthfully.
- **Structured selection**: list-key entry per declared key
  (multi-key, slash-bearing values, escaping), incomplete keys block
  the draft with an explicit warning; SAMPLE/ON_CHANGE mode and
  human-readable interval → nanoseconds with validation; origin is a
  separate field from provenance; drafts persist across
  filter/pagination/tab/profile changes and are initially populated
  from the approved binding for schema-bound devices.
- **Preview/diff**: `POST …/schema-binding/preview` submits the full
  draft + datastore; the UI renders per-path decisions (eligible,
  rejected, candidate, unknown, stale, not-applicable,
  config-excluded, ambiguous, origin warnings), proposed
  subscriptions, plan id, binding fingerprint, `config_revision`,
  schema/evidence identity, and estimates labelled by their actual
  basis ("subscription_entries … not measured bytes or device CPU").
  `activation_permitted` is displayed exactly as the server reports.
  Preview invalidation covers draft/mode/interval/origin/datastore/
  device/collector/refresh/schema/config-revision changes via
  request-generation tracking.
- **Approval/revocation**: activation requires explicit confirmation
  (device, collector, path count, added/removed/changed subscriptions,
  schema/evidence identity, replacement warning) and submits plan id +
  binding fingerprint + `expected_config_revision`; 409 preserves the
  draft, explains the conflict, invalidates the preview and never
  silently retries. Revocation is a separate Admin action with a
  required operator-visible reason and explicit "does not restore a
  previous plan" wording.
- **Execution/coverage truth**: desired → approved → assigned →
  submitted → synchronized → observed are rendered as separate stages
  from device/assignment/subscription-status data; approval alone is
  never shown as observed telemetry; missing evidence displays
  "Not verified"/"Unavailable" with backend reason codes.
- **RBAC UX**: `/api/auth/me` drives disabled (not hidden)
  activate/revoke/refresh controls for Viewer; server 403s are surfaced
  explicitly.

### API surface changes

- `GET /api/devices/{address}/yang-applicability` — added bounded
  filters `kind`, `config`, `feature`, `q` alongside existing
  `status`/`module`/`limit`/`offset`; embedded snapshot `result.paths`
  cleared before serialization (bounded response preserved).
- `GET /api/devices/{address}` and managed-device JSON — added
  `acquisition`, `schema_binding`, `inventory_only`, `external_id`,
  `updated_at_ns` fields needed by the workspace.
- `POST …/schema-binding/{preview,activate}` — `expected_config_revision`
  / `config_revision` contract documented in OpenAPI
  (`src/http_server/schema.rs`).
- `GET /api/auth/me` — authenticated subject + effective role.

## Executed verification (this branch)

### Focused Rust

- `cargo test -p bonsai --lib -- schema_binding::tests` — **6/6 PASS**
  incl. new `stale_config_revision_rejects_activation` and
  `concurrent_registry_update_preserved`.
- `cargo test -p bonsai --lib -- yang_schema::acquisition::tests
  http_server::auth::rbac_tests` — **13/13 PASS** incl.
  `two_distinct_list_instances_are_eligible`,
  `duplicate_concrete_path_rejected`,
  `stale_config_revision_rejects_evaluation`,
  `schema_preview_viewer_allowed`, `schema_activate_viewer_denied`,
  `schema_revoke_viewer_denied`.

### Integration suites (mock-gNMI, real production boundaries)

- `cargo test -p bonsai --test yang_schema_bound_acquisition` —
  **6/6 PASS** (wire-level activate → assignment → Subscribe →
  lifecycle → coverage → revoke/expiry proof on the corrected
  evaluator; all call sites updated for `expected_config_revision`).
- `cargo test -p bonsai --test yang_device_applicability` —
  **12/12 PASS**.
- `cargo test -p bonsai --test yang_catalogue` — **42/42 PASS**.
- `cargo test -p bonsai --test gnmi_selective_acquisition` —
  **19/19 PASS**.
- `cargo test -p bonsai --test gnmi_assignment_lifecycle` —
  **14/14 PASS**.
- `cargo test -p bonsai --test gnmi_security_connectivity` —
  **11/11 PASS**.
- `cargo test -p bonsai --test gnmi_subscription_e2e` — **6/6 PASS**
  (incl. `authority_renewal_recovers_coverage_across_core_restart`,
  139.8 s).

### UI build

- `cd ui && npm run build` — PASS (~8 s, Vite/rolldown, no errors).

### Browser acceptance

- `tests/ui_driver/yang_schema_workspace.spec.js` — **13/13 PASS**
  (10.3 s, real built `ui/dist` served statically; API fixtures mirror
  authoritative response shapes). Coverage: T1/T2 browser + filters +
  pagination; T3 evidence distinctions; T4/T5/T6 structured keys,
  invalid keys, origin correctness; T7 draft persistence; T8 read-only
  preview; T9 admin activation; T10 stale-preview 409; T11 Viewer
  preview-allowed/mutations-blocked; T12 admin revocation; T13
  observed-vs-pending execution truth; T14 error states without false
  success; T15 bounded large-catalogue requests/DOM; T16 legacy drawer
  tabs regression; T17/T18 accessibility + no page errors.
  Command:
  `BONSAI_URL=http://127.0.0.1:4173 PLAYWRIGHT_BROWSERS_PATH=~/.cache/bonsai-ci/ms-playwright npx playwright test yang_schema_workspace.spec.js`
  (Chromium 1254 headless shell; `ui/dist` served via `python3 -m
  http.server`).

### Screenshots

`docs/merger/evidence/2026-11-21-m6d-schema-workspace/screenshots/`:

- `t1-schema-browser.png` — module browser + paginated path table.
- `t3-evidence-states.png` — stale/conflicted/candidate evidence chips
  with disabled selection where evidence does not justify it.
- `t8-preview-eligible.png` — read-only preview: plan id, binding
  fingerprint, per-path decisions, basis-labelled estimates.
- `t9-activated.png` — post-activation state with binding metadata.
- `t11-viewer-blocked.png` — Viewer role: preview available,
  activate/revoke disabled.
- `t12-revoked.png` — post-revocation state ("does not restore").
- `t13-execution-status.png` — observed/pending/failed subscription
  stages kept distinct.

### Role/access checks

- RBAC unit tests: preview → Viewer; activate/revoke/refresh → Admin.
- Browser T11: Viewer sees preview + disabled mutation controls;
  binding metadata shown truthfully.
- Browser T12/T9: Admin activate/revoke flows succeed only after
  explicit confirmation and server response.

### API contract evidence

- Preview response carries `config_revision`, `plan_id`,
  `binding_fingerprint`, `activation_permitted`, per-path `decisions`,
  `proposed_subscriptions`, `estimated_*` with explicit `basis`.
- Activate requires `expected_config_revision`; mismatch → 409
  `config_revision_mismatch` (unit + registry tests; HTTP handler
  propagates the error).
- Applicability GET honours `status/module/kind/config/feature/q/
  limit/offset` with truthful `paths_matching`.
- OpenAPI (`src/http_server/schema.rs`) documents the new fields and
  filters.

## Canonical gate

Production SHA tested: `0246af6d4eae9a9385d919856c83905494a646e7`.

Command (trusted local mode, shared build lane, verified prebuilt
lbug bundle `4cdf4207…`):

```
scripts/ci/run-local.sh --trusted \
  --shared-target-dir /home/arjuna/.cache/bonsai-m6d-lane \
  --lbug-native /home/arjuna/.cache/bonsai-m6d-lbug-native \
  l0 l1 l2 l3
```

- L0: **PASS** (pre-existing rustfmt drift warnings in
  `src/graph/mod.rs` / `src/lib.rs` only; unchanged by this branch).
- L1: **PASS** (Rust compile/static checks, UI build, Python checks).
- L2: **PASS** — 487 lib unit tests + all integration targets, incl.
  `yang_schema_bound_acquisition` 6/6.
- L3: **PASS** — 13 integration targets + Python suites
  (259 passed, 3 skipped).
- UI build inside gate: **PASS**.

Gate log: `target/ci/local-run-20261121-102949`.

Two earlier L0–L3 attempts on the same SHA failed in unrelated,
pre-existing code: attempt 1 hit a syslog listener-readiness flake in
`bn021e_syslog_source_identity` (passed standalone and in-gate on
retry); attempt 2 hit `bn005_closeout::
query_deadline_and_request_cancellation_stop_engine_workers`
(active-diagnostic-queries == 1) which also passed on retry — both are
load-sensitive timing flakes outside the M6D diff. No M6D test failed
in any attempt.

## Known limitations

- Browser acceptance uses API fixtures mirroring authoritative shapes;
  the wire-level activate→Subscribe→lifecycle→coverage path is proven
  by `tests/yang_schema_bound_acquisition.rs` against a real mock-gNMI
  backend, not by the browser spec.
- Real-NOS YANG Library interop remains unproven (mock gNMI only).
- Remote collector-executed discovery is unsupported by design
  (`collector_discovery_required` class) — recorded for a future
  milestone.
- Applicability is schema-level evidence; transport acceptance and
  received telemetry remain separate claims in the UI.
- L4 lab/deployment acceptance was not executed.

# M6E — Authenticated remote-collector YANG discovery — evidence

Branch `feat/bn015-m6e-remote-yang-discovery` from `origin/main`
`c8f7ee3d0052a9a55f035b87e38d6ec9c123dc14` (M6D / PR #43).

## What landed

- **M6E-A corrections**: fingerprint+recency-guarded approval
  invalidation at the registry mutation boundary
  (`schema_binding::revoke_binding_superseded_by_evidence`,
  outcomes NotBound/Current/Revoked/NewerApproval/ConcurrentlyReplaced);
  segment-qualified nested-list key state in `ui/src/lib/gnmi.js`;
  async response fencing + draft preservation in
  `SchemaCollectionWorkspace.svelte`; executor selector
  (auto/core_direct/assigned_collector); refresh confirmation on
  schema-bound devices; truthful "NOT restored" revocation messaging.
- **Trust model**: `CollectorIdentity.features` advertises
  `yang_discovery_v1`; contract protocol version 6. Poll/submit RPCs
  (`PollYangDiscoveryJobs`, `SubmitYangDiscoveryResult`) require a
  transport-attested, credential-bound collector principal; payload
  `collector_id` is independently checked against the job assignee.
- **Durable jobs**: `yang_discovery_jobs` table (schema v12) in
  `bonsai_config.db` — bounded states, deadlines, attempt counts,
  result classification, snapshot/fingerprint/digest references,
  non-secret diagnostics. Per-collector 8 / per-device 2 / global 256
  active caps; 8-job poll batch; 512-row history; restart
  reconciliation expires deadlines and stale authority.
- **Collector executor**: bounded worker (max 2 concurrent), 90 s
  budget inside the durable deadline; read-only gNMI Capabilities +
  RFC 8525 YANG Library Get via the shared acquisition primitives;
  targets resolved only from current assignments; no credentials in
  job rows/evidence/logs.
- **Core admission**: validates principal, job state/deadline,
  collector binding, session generation (job stamp AND result echo),
  authority epoch, assignment generation
  (`validate_discovery_assignment` — deliberately NOT acquisition
  authority, since discovery is pre-plan), canonical device identity,
  datastore, payload bounds (4 MiB/component, 64 diagnostics),
  observation-time bounds, out-of-order fencing; then resolves via
  the SAME M6C-1 resolver and persists via the SAME snapshot path.
- **Provenance**: `EvidenceProvenance` on every snapshot —
  executor `core_direct`/`authenticated_collector`/`legacy_unknown`,
  observing collector, job id, session generation, authority epoch,
  receipt time. Pre-M6E rows read `legacy_unknown`.
- **API/UI**: refresh accepts `executor`; remote path returns
  `202 Accepted` + job view; `GET /api/devices/{address}/
  yang-discovery/jobs` bounded status polling; workspace shows
  executor/job state/collector/reason/snapshot/provenance.
- **Transport**: `max_decoding_message_size` raised to
  `YANG_DISCOVERY_MAX_RESULT_MESSAGE_BYTES` (2×4 MiB + 1 MiB) on the
  Core gRPC service so maximum-size evidence is not rejected by
  tonic's default 4 MiB decode limit before admission bounds run.

## Executed verification

- `cargo test -p bonsai --test yang_remote_collector_discovery
  --offline` — **14/14 PASS** (real mock-gNMI device over real mTLS
  collector registration channel; wire-level poll/submit). Covers
  T1 remote discovery→snapshot→schema-bound preview, T3 wrong
  collector, T4 unauthenticated, T5 stale session + redispatch, T6
  reassignment, T7 device deletion, T8 idempotent duplicate, T9
  out-of-order, T10 deadline, T11 oversized/malformed evidence, T12
  pre-contract collector, T14 explicit failure classes, T15 no
  secrets/zero writes, T16 refresh/reapproval races.
- `cargo test -p bonsai --test yang_schema_bound_acquisition
  --offline` — **6/6 PASS**.
- `cargo test -p bonsai --test yang_device_applicability
  --offline` — **12/12 PASS**.
- `cd ui && npm run build` — PASS.
- `tests/ui_driver/yang_schema_workspace.spec.js` — **15/15 PASS**
  (incl. M6E-T17 remote job status/completion/provenance and
  M6E-T17b failure reason; real built `ui/dist` on :4173, chromium
  headless shell, API fixtures).
- Canonical L0–L3 on tested SHA `92c6c998221b53cc5165a845b8b16945aed5ee1f`
  (PR head; merge `c1be72e`):
  `CARGO_BUILD_JOBS=2 scripts/ci/run-local.sh --trusted
  --shared-target-dir ~/.cache/bonsai-ci/build-lane
  --lbug-native ~/.cache/bonsai-ci/lbug-native l0 l1 l2 l3` —
  **L0/L1/L2/L3 ALL PASS** (~40 min wall). L2: 34 integration targets
  incl. `yang_remote_collector_discovery` 14/14 in-gate;
  L3: 13 targets + pytest 259 passed / 3 skipped.

## Zero device writes

Every remote test asserts `set_calls == 0` and `subscribe_calls == 0`
on the mock gNMI device; discovery issues only Capabilities + Get.

## Limitations

- Mock gNMI only — no real-NOS interoperability claim (M6F).
- Legacy `/modules-state` fallback exercised via shared probe code;
  vendor-specific deviations unsupported as before.
- gNMI transport security modes beyond the existing collector
  credential path are unchanged.

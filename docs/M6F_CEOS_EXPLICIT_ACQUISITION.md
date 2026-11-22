# M6F-C Stage D — safe cEOS acquisition strategy (design)

Status: design + lab-validated explicit-mode slice (see evidence).
Date: 2026-11-23. Scope: Arista cEOS 4.37.0F only; the pattern is
vendor-agnostic but must not be generalized silently.

## 1. What the evidence supports

- cEOS 4.37.0F gNMI: Capabilities PASS (303 models), SAMPLE and
  ON_CHANGE subscribe PASS at probe level
  (`docs/merger/evidence/2026-11-22-m6f-ceos-probe/`).
- cEOS exposes **no** RFC 8525 or legacy YANG Library — every variant
  is rejected (unknown origin / node not found). Under current Core
  resolution, applicability can never reach `applicable`; the
  schema-bound plan preview correctly refuses activation. This is
  fail-closed behavior working as designed, not a defect.
- The pinned Arista YANG repository
  (`aristanetworks/yang@f7fc5de`, `EOS-4.37.0F/`) is audited in
  `docs/merger/evidence/2026-11-23-m6fc-arista-yang-audit/`:
  `release/openconfig` (56 augment/deviation files) is the correct
  supported closure; `experimental/` (101 advertised modules) must
  not enter the authoritative catalogue; `pathreport.csv` is advisory
  versioned documentation, never device authority.

## 2. Authority model — what may and may not claim support

Evidence classes (kept distinct everywhere they surface):

| Class | Meaning | cEOS status |
|---|---|---|
| `device_effective_schema_verified` | RFC 8525/legacy YANG Library resolved against compiled catalogue | NOT AVAILABLE |
| `vendor_release_schema_pinned` | Immutable vendor repo commit + compiled catalogue provenance | AVAILABLE (audit) |
| `capabilities_advertised` | gNMI Capabilities `supported_models` | AVAILABLE (303 models) |
| `path_get_observed` / `subscription_observed` | Live RPC evidence for a specific path | AVAILABLE per path |
| `runtime_feature_state_unknown` | Features/deviations not observable | ALWAYS for cEOS |

Rules:

- Vendor release material assists **operator path selection only**.
  It never promotes a path to schema-authoritative.
- Capabilities advertisement never substitutes for YANG Library.
- No automatic `repo contains path → applicable` promotion.
- No silent fallback from failed schema-bound validation to legacy
  acquisition — the modes are distinct bindings, not a downgrade.

## 3. The safe mode that already exists

`AcquisitionMode::Explicit` + absent `schema_binding` is the existing
bounded, admin-authorized, non-schema-bound acquisition contract
(`src/config.rs`, `src/subscriber.rs`,
`apply_device_selected_paths_handler` in `src/http_server/device.rs`):

- Operator-selected paths only — the resolved `selected_paths` set IS
  the complete plan; no automatic defaults are appended.
- `POST/PATCH /api/devices/{addr}/selected-paths` validates every path
  through `gnmi_path::parse_path`, requires `SAMPLE` intervals, caps
  at `MAX_SELECTED_PATHS` (512), and records `api_apply_recommendation_paths`
  audit.
- Empty selection = explicit no-collection, never silent automatic.
- Requires authorized transport (TLS modes or explicit lab plaintext
  opt-in); legacy-inferred plaintext is refused.
- Collector assignment, credentials, and revocation go through the
  same registry/assignment/lifecycle authority as schema-bound plans.
- `schema_binding` presence makes the endpoint reject path edits —
  approval-managed plans cannot be casually widened.

What explicit mode does NOT provide (and evidence must say so):
compiled-schema identity, applicability status, feature/deviation
resolution, or coverage verification against a verified catalogue.
Coverage tracking still records observed paths truthfully; qualified
observations simply cannot establish schema coverage without a
verified catalogue (post-M6F-C behavior).

## 4. cEOS vertical slice (executed)

1. Deploy `ceos:4.37.0F` (`sha256:0c2645f9…`) via containerlab kind
   `arista_ceos`, mgmt `172.100.104.11` (subnet .104.0/24 — .102 is
   SR Linux, .103 is XRd), gNMI `6030` plaintext lab channel.
2. Register device + credential through the managed-device API
   (collector-bound, `acquisition=disabled` initially).
3. `POST /api/devices/{addr}/selected-paths` with one documented
   OpenConfig counter path
   (`interfaces/interface[name=Management0]/state/counters/in-octets`,
   origin `openconfig`, SAMPLE 10 s) — path support cross-checked
   against `pathreport.csv` (`read`).
4. Observe real telemetry → Core ingest → metric projection →
   API read-back; record values with timestamps.
5. Withdraw: `selected-paths: []` → collector resync → verify no
   continued collection (subscription count, frozen timestamps,
   lifecycle state).
6. Record everything under
   `docs/merger/evidence/<date>-m6f-ceos-e2e/` including the Stage A3
   provenance stage fields; `acquisition_mode` will read `explicit`
   with `schema_binding: null` — labelled, not hidden.

## 5. Negative cases required (D6) — backlog

- **gNMI origin echo**: cEOS does not populate `Path.origin` in
  notifications. `observation_covers` requires exact origin equality
  for non-empty expectation origins, so cEOS explicit plans MUST use
  `origin: ""` (verified live: identical path with `origin:
  "openconfig"` streamed but never reached `observed`; `origin: ""`
  reached `observed` within one interval). Whether non-echoing
  origins should be tolerated is a schema-authority policy decision —
  not relaxed here.

- EOS 4.37.0F device vs 4.36.x release catalogue (revision/source
  mismatch must not silently pass).
- Advertised module absent from pinned source.
- `deviate not-supported` path attempted anyway → must fail at
  Subscribe or be flagged, never counted as covered.
- `not-supported` / CloudVision-only pathreport rows used as plan
  input → advisory warning, still requires live observation.
- Config/collector reassignment after approval → existing fencing
  must reject stale sessions (covered by Stage G resilience tests).

## 6. Explicit non-goals

- No YANG-Library synthesis from capabilities or the Arista repo.
- No experimental-module imports into the authoritative catalogue.
- No gNMI Set, no write paths, no wildcard expansion beyond the
  approved selection.
- No claim that explicit-mode telemetry equals schema-bound
  applicability — MATRIX.md keeps the two columns separate.

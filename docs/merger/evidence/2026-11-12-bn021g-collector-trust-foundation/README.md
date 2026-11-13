# BN-021g-1 — Authenticated collector trust foundation

**Date:** 12 November 2026 · **Branch:** `feat/bn021g-collector-trust-foundation` · **Base:** `origin/main` @ `9b80cd4` (post PR #11 merge)

## Scope

First bounded slice of BN-021g: authenticate the **forwarding
collector** on the existing collector→core gRPC channel and enforce
scoped authorization — without granting authority over downstream
observations. No parallel collector architecture; reuses tonic mTLS,
`VerifiedIngressPrincipal`, `exporter_bindings.match_ingress_credential`
and the BN-021f credential-pin convention (`sha256:<DER-hex>`).

## Trust layers implemented

1. **Transport authentication** — `CollectorAuthz::principal` reads
   `TlsConnectInfo<TcpConnectInfo>::peer_certs()` from tonic request
   extensions. tonic populates this only after the TLS handshake
   verified the client certificate against `client_ca_root`. RPC
   metadata and payload fields are never consulted.
2. **Credential→collector binding** — `authorize_collector` requires
   an ACTIVE `exporter_bindings` row with `protocol="collector"`,
   `exporter_id` == claimed `collector_id`, and
   `match_ingress_credential` == connection fingerprint.
3. **Scoped authorization** — per-RPC capability gates:
   - ingest plane (`telemetry_ingest`, `detection_ingest`,
     `register_collector`, `heartbeat`): per-message claimed
     `collector_id` must be bound to the connection credential;
   - `core-admin`: device/site/credential mutation, `query`,
     detection/remediation writes;
   - `sidecar`: sidecar register/heartbeat;
   - read plane (`get_devices`, `get_interfaces`, `get_bgp_neighbors`,
     `get_topology`, `stream_events`, `list_*`): any active
     credential-pinned binding;
   - `get_leader`: any pinned binding (collector re-homing).
4. **Provenance** — each authorized (credential, collector) pair per
   stream is audit-logged (`audit_log`, action `ingest_authorized`)
   with binding id, scope and canonical device id. Metrics:
   `bonsai_collector_authz_total`, `bonsai_collector_ingest_authorized_total`.
5. **Revocation/rotation** — decisions cached ≤5s
   (`AUTHZ_CACHE_TTL`, 4096-entry bounded cache, cleared on overflow);
   binding changes take effect within that window. Rotation =
   re-pin `match_ingress_credential` on the same binding (sequential,
   no overlap — same semantics as BN-021f).
6. **Replay safety** — principals exist only in request extensions of
   a live TLS connection; serialized queue records cannot recreate
   them (verified by test).

Plaintext mode (no client cert — `runtime.tls` disabled) is the
explicit legacy dev path: all gates pass. When server TLS is enabled
the handshake already requires a client certificate.

## What this slice deliberately does NOT do

- No canonical projection changes: collector-authenticated telemetry
  remains provisional evidence end-to-end (BN-021g-2+).
- No per-observation `VerifiedIngressPrincipal` on `TelemetryUpdate`
  yet — the typed plumbing point is `authorize_ingest` + audit; the
  graph boundary stays fail-closed.
- Binding `scope` is recorded as authorized scope; per-update
  site/target claims are self-reported and remain unverified.
- `core-admin`/`sidecar` capability bindings must be provisioned via
  `POST /api/identity/bindings` (`protocol` = `core-admin`/`sidecar`,
  `match_ingress_credential` = cert fingerprint) before mTLS clients
  can use those RPCs.

## Verification — real transport acceptance

`tests/bn021g_collector_trust_foundation.rs` — 6/6, real tonic mTLS
server (`ServerTlsConfig` + `client_ca_root`) and real client
channels, synthetic rcgen PKI:

| Test | Scenario | Result |
|---|---|---|
| authorized_collector_ingests_and_impersonation_is_rejected | F1/F3/F10/F12: bound cert ingests/heartbeats/registers/get_leader; same cert claiming collector B's id → `PermissionDenied`; second bound cert on same loopback IP works for its own id | PASS |
| unbound_certificate_is_denied | F2: CA-valid cert with no binding → ingest, read and admin RPCs all `PermissionDenied` | PASS |
| forged_payload_principal_is_inert | F5: payload carrying fabricated `identity_resolution`/`verified_ingress` accepted as data only; bus update preserves claims as inert JSON | PASS |
| revocation_stops_ingest_within_ttl | F7: binding revoked mid-connection; ingest denied after ≤5s cache TTL | PASS |
| credential_rotation_repins_without_impersonation | F9: re-pin fingerprint on same binding; new cert authorized, old cert denied | PASS |
| replayed_records_cannot_mint_authority | F8: old serialized record replayed over an unbound cert's connection → denied; rightful collector unaffected | PASS |

Missing-client-cert case: with `client_ca_root` configured, rustls
rejects the handshake itself — no RPC is reachable (transport-level,
not app-level).

## Resource notes (Phase G)

- Authz lookup = one indexed SQLite query per cache miss; steady-state
  cost is a mutex'd HashMap lookup per message — no per-point DB hit.
- Cache bounded at 4096 decisions; overflow clears (fail-safe, next
  lookup re-reads store).
- Revocation fan-out = next decision after TTL; no push channel —
  bounded 5s window documented.
- TLS handshakes unchanged (one per connection, tonic/rustls).

## Build timings

Appended to `docs/build-times/runs.csv`.

---

## Review-hardening round (same branch, same day)

Independent review of PR #12 found material authorization gaps (G1–G7).
This round closes them without redesigning the collector architecture.

### Findings disposition

| Finding | Disposition |
|---|---|
| G1 observation authorization | FIXED — per-message assigned-device + provisional-path boundary in `telemetry_ingest`; registry metadata overrides forged claims |
| G2 DetectionIngest bypass | FIXED — separate `detection-ingest` capability + target/event provenance verification; remediation fields ignored remotely |
| G3 unrestricted reads | FIXED — explicit capability matrix; global reads require `operator-read`; collector reads scoped to assigned devices; `Query` requires `core-admin` |
| G4 active-stream revocation | FIXED — per-delivery + periodic stream revalidation; generation-fenced sessions; credential-delivery caveat documented |
| G5 missing principal | FIXED — `runtime.auth_mode` = `required` (default) / `development-compat`; unknown values fail config parsing |
| G6 grants/principal hardening | FIXED — transport-attestation enforced on every authz path; ambiguous bindings fail closed; grants are internal context, not bearer tokens |
| G7 provenance/audit/cache | FIXED — real cert fingerprint in registration audit; audit failure logged + metric'd (no fabricated success); bounded indexed lookups |

### Verification — extended suite

`tests/bn021g_collector_trust_foundation.rs` — **19/19 PASS** (was 6/6),
real loopback mTLS + real `GraphStore` wired to the bus exactly as
`server_startup` does (bus → `BonsaiStore::write`):

- Cross-collector canonical telemetry denial; unknown-target denial;
  forged site/role/hostname/vendor rewritten from inventory.
- Provisional flow payload stays `flowsrc/`-scoped `AppFlow` evidence;
  relabeled canonical path on unassigned target denied; no canonical
  `Device` minted for the exporter.
- Detection ingest: cross-collector device denied, cross-device and
  provisional `source_event_ids` rejected, fabricated detection denied,
  valid authorized detection accepted.
- RPC matrix: collector credential denied `operator-read` globals and
  `core-admin`; exporter-only credential denied everything; sidecar
  credential limited to sidecar RPCs; operator-read gets scoped reads.
- Active assignment-stream revocation stops credential-bearing
  deliveries; idle `StreamEvents` revocation; reconnect generation
  fencing (old stream close cannot remove new session).
- `runtime.auth_mode=required` denies plaintext; `development-compat`
  permits it explicitly; missing TLS principal fails closed; serialized
  (unattested) principal denied; ambiguous binding fails closed;
  registration audit records the genuine certificate fingerprint.

Regression: `cargo test -p bonsai --lib` 290 pass / 5 fail — the same
5 documented BN-019-B6 baseline failures, none new.

---

## Final hardening round (H1–H6, same branch)

A second independent review of the hardened slice confirmed six
residual defects. All are closed without a second identity
architecture.

### Findings disposition

| Finding | Disposition |
|---|---|
| H1 provisional-path authorization bypass | FIXED — path-prefix exception removed; decode → `TelemetryEvent::authority()` classify → authorize by class. Canonical writers on `streaming/` paths (BMP, BGP-LS, interface summaries, config-state) now require the assigned-device check; provisional classes require a `*src/` key re-namespaced to the authenticated collector; unclassified → Ignored |
| H2 queue ack semantics | FIXED — `TelemetryIngestResponse.rejected` added; core consumes whole stream before responding so accepted+rejected==submitted; permanent rejections retired, RPC errors retain batch; verified with real `drain_queue_to_core` against a real dev-compat core |
| H3 stream-start-only detection capability | FIXED — `detection-ingest` revalidated per message; authz decisions epoch-stamped (binding upsert/revoke bumps epoch → immediate cache invalidation); verified by mid-stream revocation test |
| H4 weak detection provenance | FIXED — remote detections require non-empty, existing, same-device, server-provenanced (`source_type`) non-provisional source events; malformed JSON rejected; `remote_detection_ingest` marker stored; write failures are retryable stream errors |
| H5 read-scope ambiguity | FIXED — `collector_ids_for_credential` ambiguity rules match ingest; store rejects a second collector identity per credential; no `DISTINCT` first-match collapse |
| H6 session lifecycle gaps | FIXED — drop guard owns assignment cleanup on disconnect/cancellation (incl. pre-first-poll drop); generation check + cleanup atomic under `active_collectors` lock; idle-stream revocation bound ≈5s poll |

### Verification — final suite

`tests/bn021g_collector_trust_foundation.rs` — **25/25 PASS**
(new: `classified_authority_gates_canonical_writers_on_streaming_paths`,
`detection_stream_revocation_stops_authoring_mid_stream`,
`detection_ingest_requires_verified_nonprovisional_evidence`,
`ambiguous_collector_binding_denies_scoped_reads`,
`credential_cannot_bind_multiple_collector_identities`,
`assignment_stream_disconnect_still_cleans_up_session`; denial
assertions updated to the truthful `accepted`/`rejected` contract).

`src/ingest.rs` unit tests — **3/3 PASS** real-server drain tests:
interleaved accepted/rejected batch fully retired,
fully-rejected batch retired without blocking subsequent records,
core outage retains unacknowledged records for replay.

Focused lib modules (`ingest::`, `assignment::`, `collector_auth::`,
`telemetry::`) — 15/15 PASS.

### Remaining limitations

- "Accepted" telemetry = published to the in-memory bus, not a durable
  graph commit — at-least-once, never exactly-once.
- Idle-stream revocation bound ≈ `STREAM_REVALIDATE_INTERVAL` (5s);
  per-message paths are epoch-immediate.
- Canonical graph entities remain globally address-keyed; collector
  scoping is assignment-based, not multi-tenant isolation.
- Historical `StateChangeEvent`s without server-owned `source_type`
  provenance cannot back remote detections (fail closed).
- `Query` remains unrestricted Cypher behind `core-admin` — privileged
  administrative trust boundary; broader query-policy redesign and
  credential-storage migration stay under BN-022.

---

## Residual-findings round (R1–R4, same branch, 13 November)

A third independent review confirmed the H1–H6 closures but identified
four residual implementation defects. All are closed on the same
architecture; no redesign.

> **Correction to earlier evidence:** the previously documented
> `detection_ingest_requires_verified_nonprovisional_evidence` test
> created the `sce-no-prov` event but never submitted a detection
> referencing it — the missing-provenance path was untested. This
> round adds the submission and the full provenance matrix below.

### Findings disposition

| Finding | Disposition |
|---|---|
| R1 provisional identity normalization incomplete | FIXED — `authorize_provisional_source` now unconditionally pins effective `source_key`/`exporter_id` (missing/null/numeric/malformed producer claims can never reach `classify()`'s untrusted `exporter_address` fallback), pins `identity_status=provisional`/`authority=unverified`/null canonical fields regardless of producer claims, re-derives `otlpapp/` ids under the authenticated namespace, preserves all original claims under `claimed_*`/`claimed_payload_*` evidence keys, and requires the payload source prefix to match the classified protocol (`provisional_prefix_for_event`) |
| R2 detection provenance was a denylist | FIXED — `verify_detection_scope` rewritten as a positive allowlist of server-established `source_type` values with structured detail parsing; missing/null/empty/unknown provenance and malformed event details are permanent rejections; store/graph failures are retryable stream errors; the `sce-no-prov` case is now actually submitted |
| R3 queue ack contract unenforced | FIXED — `drain_queue_to_core` validates the response before acking: response `protocol_version` must not be major-skewed and `accepted + rejected` must exactly equal the submitted count via checked arithmetic; violations retain the batch for replay. Core-side `telemetry_ingest` verifies `protocol_version` on EVERY record (major skew = permanent per-record rejection). `InProcessBus::publish` returns a `PublishOutcome` so router-queue overload is a retryable failure, not silent loss |
| R4 collector identity uniqueness was scope-local | FIXED — schema v5 adds partial unique indexes making active `protocol="collector"` `exporter_id` (collector identity) and `match_ingress_credential` globally unique; `upsert_exporter_binding` enforces the same rules at provisioning (concurrent provisioning serialized — exactly one winner); authorization re-checks and pre-existing ambiguous rows fail closed for every claimant; rotation updates the same `binding_id` |

### Verification — final suite

`tests/bn021g_collector_trust_foundation.rs` — **31/31 PASS**
(new/updated: `provisional_source_identity_is_server_normalized`,
`otlp_provisional_app_id_is_renamespaced`,
`per_record_protocol_version_is_verified`,
`collector_identity_uniqueness_is_global`,
`concurrent_collector_provisioning_is_serialized`,
`credential_binding_lookups_use_sqlite_indexes` (EXPLAIN QUERY PLAN
evidence), `ambiguous_binding_fails_closed`,
`credential_cannot_bind_multiple_collector_identities`, corrected
`detection_ingest_requires_verified_nonprovisional_evidence` and
`forged_payload_principal_is_inert` assertions for the `claimed_*`
evidence contract).

`src/ingest.rs` drain tests — **16/16 PASS** including new scripted-core
contract tests: consistent accepted/rejected batch acked, accounting
mismatch retained, impossible counts retained, response protocol skew
retained, interleaved rejections retired, bus router saturation retains
batch for replay, core outage retains records, restart/replay.

`cargo test -p bonsai --lib` — **299 pass / 5 fail**, the same five
documented BN-019-B6 baseline failures; none new.

Risk-linked regressions — **110/110 PASS** across
`bn021b_trusted_identity`, `bn021c_exporter_isolation`,
`bn021d_flow_exporter_isolation`, `bn021e_{otlp,sflow,snmp,syslog}`,
`bn021f_{authenticated_ingress,hardening}`, `bn022_graph_read_boundary`.

### Phase-6 cache/index evidence

- Authz decisions are stamped with the epoch captured BEFORE the store
  read; a lookup begun before revocation can never cache an obsolete
  decision under the new epoch. Epoch bumps happen inside the binding
  mutation before audit persistence, so a later audit failure cannot
  leave stale grants.
- Revocation guarantee (accurate): within one core process, binding
  mutations are visible on the next authorization check. Cross-process
  or direct-DB changes remain bounded by the 5s TTL — recorded as a
  BN-021g-6 production-acceptance limitation, not silently claimed.
- `match_ingress_credential` lookups are served by real SQLite
  indexes (`idx_exporter_bindings_credential`, and for collector
  protocol the partial unique `idx_exporter_bindings_collector_credential`);
  verified via `EXPLAIN QUERY PLAN` in
  `credential_binding_lookups_use_sqlite_indexes`.

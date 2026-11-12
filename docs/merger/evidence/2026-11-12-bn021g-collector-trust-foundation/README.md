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

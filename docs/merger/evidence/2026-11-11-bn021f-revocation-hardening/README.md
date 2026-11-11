# BN-021f hardening — revocation, TLS availability, deployment policy

Date: 2026-11-11. Branch: `feat/bn021c-bmp-exporter-isolation` (PR #2).
Builds on `d130de0` (authenticated BMP ingress). This slice closes the
remaining security/lifecycle findings from the PR #2 security review
without starting BN-021d.

## Findings addressed

| # | Finding | Class | Resolution |
|---|---------|-------|------------|
| F1 | TLS handshakes serialized the BMP accept loop — a client that opens TCP and never handshakes blocks all later exporters for up to the 10 s timeout | Confirmed defect (availability) | Handshake moved into per-connection tasks bounded by a semaphore (`BONSAI_ROUTING_BMP_TLS_MAX_HANDSHAKES`, default 64) + timeout (`BONSAI_ROUTING_BMP_TLS_HANDSHAKE_TIMEOUT_SECS`, default 10 s). Excess connections are dropped predictably at accept; the accept loop never waits on a handshake. |
| F2 | Active sessions kept `vexp:*` authority after their binding/credential was revoked (bindings were only re-read per NEW connection) | Confirmed design limitation → fixed | `ActiveBmpSession` registry + periodic batched revalidation (`BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS`, default 5000). Each authorized session re-resolves its stored evidence envelope against a fresh binding snapshot; any change to the (binding, exporter, device) triple signals revocation. The session's ingestion loop fails closed on the signal, flows through the normal durable disconnect path (`bmp_authorization_revoked` gap), and preserves last-known state per BN-005. |
| F3 | Certificate rotation required changing canonical identity or left old sessions authorized indefinitely | Confirmed design limitation → procedure + enforcement | Rotation = re-pin `match_ingress_credential` on the SAME binding (canonical exporter/device unchanged). Old sessions are revoked at the next revalidation; the replacement credential resolves the same `vexp:*` scope. Simultaneous old+new credential authorization would require a credential list column — deferred, sequential procedure documented below. |
| F4 | `BmpIdentityMode::Verified` default but `bmp_mtls` optional; packaged compose exposed plaintext BMP with no explicit opt-in | Confirmed deployment-safety gap | `validate_deployment_with` policy: non-loopback BMP without mTLS refused unless `BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT=1`/dev umbrella; `ClaimCompatible` refused unless `BONSAI_ROUTING_ALLOW_CLAIM_COMPATIBLE=1`/dev umbrella; configured mTLS material is fully parsed at startup (fail fast). Loopback plaintext remains allowed as an explicit dev posture. Compose passes TLS env through and documents the requirement. |
| F5 | `VerifiedIngressPrincipal` had public fields + derived `Deserialize` — any code path could mint an "verified" principal | Confirmed defect (type boundary) | Fields private; sole constructor `pub(crate) from_transport_attestation` (ingress layer only). `transport_attested` flag is `#[serde(skip)]`; manual `Deserialize` always yields `transport_attested=false`, and `resolve_verified_ingress` fails closed on unattested principals. Journals/API payloads can carry the fingerprint as evidence but can never re-authorize. |

## Architecture

```
TCP accept (never blocks on handshake)
  └─ semaphore permit (bounded, try_acquire; drop on exhaustion)
      └─ per-conn task: TLS handshake (timeout, shutdown-aware)
          └─ mtls_peer_principal → VerifiedIngressPrincipal (attested)
              └─ handle_bmp_connection
                  ├─ Initiation → resolve_bmp_exporter_with_ingress
                  ├─ Resolved → register ActiveBmpSession (watch revoke channel)
                  └─ frame loop: select(read_frame, revoke) → durable disconnect

revalidation monitor (every RECHECK_INTERVAL_MS):
  try_trusted_bmp_bindings()  (ONE batched SQLite read per tick)
  for each active session: re-resolve observed envelope
    → still (binding, exporter, device)? keep : revoke_tx.send(true)
  store unreadable ×3 consecutive → revoke ALL tracked sessions
```

Revocation is detected within one recheck interval (default 5 s,
lab 1 s). No per-route SQLite queries; no in-memory-only notifications
(the routing service and management service are separate processes).

## Fail-closed semantics

- Store query failure: tolerated for up to 3 consecutive intervals
  (~15 s default), then ALL authorized sessions are revoked
  (`identity_store_unavailable`). A revoked-then-recreated binding has
  a new `binding_id` → old session's triple no longer matches → revoked.
- Credential rebound to a different device: resolution yields a
  different `device_id` → revoked.
- Binding gaining a `match_ingress_credential` pin while a
  claim-compatible session lives: `IdentityResolver::resolve` filters
  pinned bindings → session revoked.
- Revoked sessions cannot resume authority without a NEW connection
  completing resolution (which itself fails closed).

## Credential rotation procedure (sequential, safe today)

1. Issue replacement client certificate from the trusted CA.
2. Re-pin the binding: `upsert_exporter_binding` with the same
   `binding_id`, same canonical exporter/device, new
   `match_ingress_credential` fingerprint (admin API or
   `binding_admin.py repin` in the lab).
3. Within one recheck interval the old sessions are revoked; old
   credential reconnects resolve `conn:*` only.
4. Replacement credential connects → same `vexp:*` scope, same
   canonical exporter/device, new session epoch; RIB lifecycle
   continues (no canonical identity change).

Simultaneous old+new credential authorization (zero-downtime overlap)
is NOT supported: one binding pins exactly one fingerprint and the
(scope, protocol, exporter_id) uniqueness rule forbids a second
binding for the same exporter. A credential-list column is a possible
future schema v5; the sequential procedure above is the documented
operational path.

## Deployment requirements (production)

- `BONSAI_ROUTING_BMP_TLS_CERT`, `_KEY`, `_CLIENT_CA` — mounted
  read-only secret/cert material; all three required together.
- `BONSAI_ROUTING_BMP_IDENTITY_MODE=verified` (default).
- Non-loopback BMP bind REQUIRES mTLS or explicit
  `BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT=1` (dev only; every connection
  then stays `conn:*`-isolated — no authoritative state).
- `BONSAI_ROUTING_BMP_IDENTITY_MODE=claim_compat` REQUIRES explicit
  `BONSAI_ROUTING_ALLOW_CLAIM_COMPATIBLE=1` — never for production
  attribution.
- Credential-pinned bindings provisioned via the identity admin API
  (`match_ingress_credential`); routing service access stays read-only.
- `BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS` — revocation detection
  bound (default 5000; `0` disables revalidation — unsafe).

## Tests

`tests/bn021f_hardening.rs` — 9/9:

- stalled handshake does not block a valid client;
- handshake-limit saturation bounds excess connections and recovers
  after timeout;
- shutdown cancels pending handshakes and stops the listener;
- active binding revocation terminates the session with a durable
  `bmp_authorization_revoked` gap and blocks credential re-acquisition;
- revocation invalidates ALL sessions of a principal while leaving
  other exporters untouched;
- binding re-pin to a different device revokes the old session;
- certificate rotation preserves canonical exporter/device identity;
- deployment policy matrix (mTLS required off-loopback, claim_compat
  gated, missing TLS files fail fast, dev overrides);
- deserialized `VerifiedIngressPrincipal` can never resolve.

`tests/bn021f_authenticated_ingress.rs` — 9/9 (unchanged contract).

## Live NAT acceptance (R1–R5)

Lab: `lab/acceptance/bn021c-nat` rebuilt with the hardened binary;
all exporters still share translated BMP source `10.240.0.200`.
See `LIVE-RESULTS.md` and `api/` snapshots.

- R1 active binding revocation: `bind-frr-a` revoked via
  `binding_admin.py` → `vexp:exp-frr-a` gapped with
  `bmp_authorization_revoked` within ~1 s, last-known 4 routes kept;
  FRR-A reconnect isolated to `conn:*`; `vexp:exp-frr-b` unaffected.
- R2 multi-session revocation: FRR session + second same-cert session
  shared `vexp:exp-frr-a`; revocation terminated BOTH (second session
  observed closed by peer), single gap recorded, no sibling corruption.
- R3 rotation: binding re-pinned to a fresh `client-rot` certificate;
  old credential's session revoked; replacement credential resolved
  the SAME `vexp:exp-frr-a`/`dev-frr-a` scope.
- R4 TLS availability: 70 stalled TCP connections saturated all 64
  handshake permits — `BMP TLS handshake capacity exhausted; dropping
  connection` logged; established sessions continued; after the 10 s
  handshake timeout permits recovered and fresh valid handshakes
  succeeded.
- R5 restart: `docker kill -s KILL` + restart → replay restored
  `vexp:*` speakers with last-known routes; the
  `bmp_authorization_revoked` gap survived replay; the revoked
  credential still resolved `conn:*` only; no `exp:*` synthesis, no
  fabricated withdrawals.

## Remaining limitations

- Revocation bound = recheck interval (default 5 s); an in-flight
  frame batch can complete between signal and loop exit (bounded by
  frame read, not unbounded).
- Plaintext loopback BMP remains allowed without override (dev
  posture); non-loopback requires explicit opt-in.
- `conn:*` scopes created before revocation detection retire normally;
  verified scopes keep last-known state under the gap (BN-005).
- Exporters need native mTLS or an authenticated sidecar; FRR 10.6 BMP
  has no TLS — lab uses `bmp_tls_proxy.py`.
- Production acceptance (BN-021g) remains open: cross-protocol
  regression, multi-site, failover, alias migration.

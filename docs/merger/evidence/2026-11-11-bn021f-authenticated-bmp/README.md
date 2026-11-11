# BN-021f — Authenticated BMP Ingress and Authoritative Exporter Identity

Status: VERIFIED (implementation, tests, and live NAT acceptance done)
Date: 2026-11-11
Branch: `feat/bn021c-bmp-exporter-isolation` (extends PR #2)

## 1. Problem

BN-021c PR #2 review proved that BMP `sysName`/`sysDescr` are
self-reported claims: after the legitimate exporter disconnects, an
unrelated exporter presenting the same `sysName` through the same NAT
source acquires the canonical `exp:*` scope, the canonical `device_id`,
and write access to that exporter's authoritative RIB
(`../2026-11-11-bn021c-pr2-review/LIVE-RESULTS.md`, Scenario F-seq).
RFC 7854 §11 identifies exporter impersonation as the key risk of
unauthenticated BMP transport.

A second defect: the concurrent same-claim fallback isolated the second
connection into `conn:*` but preserved the resolved `device_id`, so an
unauthenticated claimant still *appeared* associated with an
authoritative device.

## 2. Architecture decision

**Mutual TLS terminated inside `bonsai-routing`.**

- The BMP listener optionally wraps each accepted TCP connection in a
  rustls server handshake that *requires* a client certificate chaining
  to a configured CA (`bmp_mtls`: cert/key/client-CA PEM paths, env
  `BONSAI_ROUTING_BMP_TLS_{CERT,KEY,CLIENT_CA}`).
- After the handshake, the verified principal is derived **only** from
  the peer certificate: `sha256:<hex>` of the DER leaf
  (`mtls_peer_principal`). It can never be influenced by BMP payload
  bytes, headers, or PROXY metadata.
- The principal feeds `IdentityResolver::resolve_verified_ingress`,
  which first filters trusted bindings to those whose
  `match_ingress_credential` equals the fingerprint, then applies the
  existing claim/contradiction evidence rules. Certificate validity
  alone is **not** authorization: a valid cert with no pinned binding
  resolves to an isolated `conn:*` scope with `device_id = None`.
- Alternatives considered: external TLS proxy (rejected as primary —
  delivering the authenticated identity to the resolver would need a
  new trusted side-channel; kept possible via a future
  `IngressAuthMethod`), PROXY protocol (rejected — spoofable),
  per-connection tokens inside BMP (rejected — not in RFC 7854,
  application-layer secret on an untrusted path).

### Identity namespaces

- `vexp:{scope}:{canonical}` — authoritative scope reachable ONLY via
  verified ingress. Distinct namespace fences historical claim-based
  `exp:*` journal records: replay never promotes old self-reported
  state into verified state, and new verified connections can never
  join a legacy `exp:*` speaker.
- `exp:{scope}:{canonical}` — legacy claim-compatible scope
  (BN-021c), retained read-only for journal compatibility; reachable
  only under the explicit unsafe opt-in.
- `conn:{uuid}` — per-connection isolation; never carries
  `device_id`.

### Modes

- `BmpIdentityMode::Verified` (default): canonical attribution requires
  a verified principal + credential-pinned binding. Plaintext or
  mTLS-without-binding connections are always `conn:*` isolated.
- `BmpIdentityMode::ClaimCompatible` (`BONSAI_ROUTING_BMP_IDENTITY_MODE
  =claim_compat`): explicit, clearly-unsafe opt-in retaining BN-021c
  claim-based `exp:*` promotion for migration/interop only.

## 3. Persistence (schema v4)

Additive migration: `exporter_bindings.match_ingress_credential TEXT`
(NULL = credential not pinned; such bindings are invisible to the
verified path). Idempotent (`user_version` 3→4, `ALTER TABLE … ADD
COLUMN` guarded by version check). Existing populated DBs migrate on
open; the routing service's read-only open works against both v3 and
v4 databases (v3 simply yields no credential-pinned bindings → all
verified connections isolate, fail closed). No private key material is
stored; journals and API responses never carry credential material.

## 4. Lab topology (real TCP + real TLS + real NAT)

`lab/acceptance/bn021c-nat/` extended:

- `bmp_tls_proxy.py` — stdlib-only mTLS client shim run inside each FRR
  container (FRR 10.6 BMP has no TLS support). FRR BMP →
  `127.0.0.1:11019` plaintext → mutual-TLS client (per-exporter cert,
  verifies Bonsai server cert against lab CA) → NAT → Bonsai.
- Per-run lab CA + server + 4 client certs generated under
  `runtime/tls/` (gitignored; keys never committed).
- `make_identity_db.py` → schema v4; bindings for frr-a/frr-b pin their
  cert fingerprints; frr-c and frr-imp have valid certs but NO binding.
- `frr-imp` (hostname `frr-a`, own unbound credential) created STOPPED
  for the sequential-impersonation test.

## 5. Test coverage

`tests/bn021f_authenticated_ingress.rs` — real TCP + real rustls
handshakes against `run_bmp_listener` with an mTLS acceptor:

| Test | Scenario | Expected |
|---|---|---|
| A | valid cert + binding | `vexp:*` scope, canonical device, resolved |
| B | sequential impersonation (same sysName, different cred) | `conn:*`, no device_id, A's state untouched |
| C | shared NAT source, distinct creds | fully separated `vexp:*` scopes |
| D | valid cred B claims A's sysName | contradiction → `conn:*`, no attribution |
| E | missing / rogue-CA / plaintext client | handshake failure, zero state |
| F | revoked binding | `conn:*`, no authority |
| G | legit reconnect, same cred | stable `vexp:*` identity, new epoch |
| H | two conns, same verified principal | shared `vexp:*` scope (FRR global+VRF) |
| I | SIGKILL + replay | `vexp:*` provenance survives; `conn:*` retired; no `exp:*` synthesis |

## 6. Results

- `tests/bn021f_authenticated_ingress.rs`: **9/9 pass** (tests A–I).
- Regression: bn021c 17/17, bn021b 13/13, bn005_closeout 11/11,
  rbmp-rib 42, rbmp-store, bn006 suites, r01_r04 7/7, r05_r06 2/2,
  bn022 5/5 — 0 failures. `cargo check -p bonsai --all-targets` clean.
- Live NAT acceptance: see `LIVE-RESULTS.md` — sequential and
  concurrent same-claim impersonation cannot acquire another
  exporter's `vexp:*` scope or `device_id`; legitimate reconnects and
  multi-connection sharing work; SIGKILL replay preserves verified
  provenance.
- Clippy: `cargo clippy -p bonsai --all-targets` reports warnings only
  plus ONE pre-existing `deny`-level `never_loop` error in untouched
  `src/http_server/device.rs` (present on `main`, unrelated to this
  change).

## 7. Remaining limitations

- Active-session revocation: bindings are re-read per NEW connection;
  an already-resolved `vexp:*` session keeps authority until reconnect
  (same deferred-lifecycle limitation as BN-021c P1-1). Bounded
  follow-up: periodic binding revalidation or admin session-kill.
- Certificate rotation requires updating `match_ingress_credential`
  (explicit binding update — by design, no implicit trust).
- `ClaimCompatible` mode remains unsafe-by-flag for migration only.
- No production-readiness claim from local/lab TLS tests.

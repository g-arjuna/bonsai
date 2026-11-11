# BN-021f hardening — live NAT results (2026-11-11)

Lab rebuilt via guarded `up.sh` (`BN021C_RESET=1`; prior runtime
archived under `runtime/_archive/`). Image `bn021c-routing:local`
rebuilt from the hardened binary. All BMP exporters share translated
source `10.240.0.200` through `bn021c-nat`.

Baseline after convergence (`api/speakers-pre-revocation.json`):

```
vexp:exp-frr-a | dev-frr-a | resolved | 4 routes
vexp:exp-frr-b | dev-frr-b | resolved | 4 routes
conn:<id>      | None      | provisional | 2 routes   (frr-c, unbound)
```

## R1 — active binding revocation

`binding_admin.py revoke bind-frr-a` at ~08:51:0xZ.

Post-revocation (`api/speakers-post-revocation.json`):

```
vexp:exp-frr-a | dev-frr-a | gap | bmp_authorization_revoked | 4 routes (last-known)
vexp:exp-frr-b | dev-frr-b | complete_for_observed_view | 4 routes
conn:5d0fb6e7… | None      | complete_for_observed_view | 4 routes (frr-a reconnect, isolated)
conn:96d5aafc… | None      | provisional | 2 routes (frr-c)
```

Service log: `revoking BMP session: authorization no longer valid
… binding_id=bind-frr-a credential=Some("sha256:d0b0…") status=Provisional`.
The revoked credential's FRR reconnect could not reacquire authority.

## R2 — multiple sessions, one principal

FRR-A's own BMP session plus a second mTLS session (`second_session.py`,
same `client-a` cert, sysName `frr-a`) — both resolved into
`vexp:exp-frr-a` (two `identity resolved … vexp:exp-frr-a` log lines,
no `conn:*` isolation for the sibling).

`revoke bind-frr-a` → both sessions terminated: the second session
exited (`closed by peer`), the shared scope recorded ONE
`bmp_authorization_revoked` gap; `vexp:exp-frr-b` untouched.

## R3 — certificate rotation

1. `activate bind-frr-a`, then `repin bind-frr-a` → fresh
   `client-rot` fingerprint (`sha256:9e5ae726…`).
2. Cert-A session revoked at next recheck (`bmp_authorization_revoked`).
3. `second_session.py` with `client-rot` + sysName `frr-a` →
   `vexp:exp-frr-a | dev-frr-a | exp-frr-a` live again (new epoch,
   gap cleared). Canonical identity preserved; old credential now
   resolves `conn:*` only.

## R4 — TLS availability

70 stalled TCP connections to `10.240.0.20:11019` (no TLS bytes)
from inside `frr-a`. Service log:

```
WARN BMP TLS handshake capacity exhausted; dropping connection
     speaker_ip=10.240.0.200 max_handshakes=64   (×15+)
```

Established sessions (`vexp:exp-frr-b`, rot session) continued
streaming throughout. After the 10 s handshake timeout released
permits, fresh valid handshakes succeeded (`vexp:exp-frr-a`
re-established; `vexp:exp-frr-b` recovered).

## R5 — SIGKILL restart/replay

`docker kill -s KILL bn021c-bonsai && docker start bn021c-bonsai`,
with `bind-frr-a` revoked before the kill.

Post-restart (`api/speakers-post-restart.json`):

```
vexp:exp-frr-a | dev-frr-a | gap | bmp_authorization_revoked | 4 routes
vexp:exp-frr-b | dev-frr-b | complete_for_observed_view | 4 routes
conn:*         | None      | provisional/complete | isolated
```

- The revocation gap replayed from `bmp.journal` (3 durable
  `bmp_authorization_revoked` records present).
- A fresh connection with the REVOKED `client-rot` credential
  resolved `conn:*` only — revocation survives restart.
- No `exp:*` synthesis, no fabricated withdrawals; last-known routes
  retained under explicit gaps (BN-005 intact).

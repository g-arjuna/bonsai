# BN-021c PR #2 hardening — live NAT re-verification (2026-11-11)

Lab rebuilt via `BN021C_RESET=1 lab/acceptance/bn021c-nat/up.sh` (prior
`runtime/` journals/DB archived under `runtime/_archive/` — not deleted).
Image `bn021c-routing:local` rebuilt from post-review `target/debug/
bonsai-routing`. All three FRR exporters behind SNAT source
`10.240.0.200` → bonsai `10.240.0.20:11019`.

## Baseline (post-rebuild)

`api/speakers-final.json` (final state) and observed sequence:

| step | observation |
|---|---|
| bring-up | `exp:exp-frr-a` (dev-frr-a, 4 routes), `exp:exp-frr-b` (dev-frr-b, 4), `conn:6734…` (frr-c, provisional, 2) — all `speaker_addr=10.240.0.200` |

## Concurrent same-claim (was Scenario F FAIL) — NOW ISOLATED

Recreated site-C exporter with `--hostname frr-a` while real frr-a was
connected (`docker rm -f bn021c-frr-c` + `docker run --hostname frr-a`
on sitec, frr-c config/AS65003).

Observed (`bonsai` log + API, ~05:49:32Z):

```
WARN Concurrent BMP connection claims an already-live exporter scope;
isolating to conn:* (same-claim connections cannot be proven to share
ownership) speaker_ip=10.240.0.200 exporter_instance=exp:exp-frr-a
device_id=Some("dev-frr-a")
```

Speakers after impostor connect:

- `exp:exp-frr-a` epoch 1, **4 routes, unchanged** — legit prefixes only
  (`10.99.0.0/24`, `203.0.113.0/24`).
- `conn:076e82d3…` epoch 4, 2 routes — impostor's `192.0.2.0/24` lives
  here. `identity_status=resolved`, `device_id=dev-frr-a` (claim-based
  attribution preserved, state isolated).

**Concurrent identical-claim merge: FIXED and lab-verified.**

## Sequential same-claim — STILL POSSIBLE (documented OPEN gate)

`docker stop bn021c-frr-a`, then `docker restart bn021c-frr-c`
(impostor, hostname `frr-a`; re-applied `ip route replace default via
10.240.3.2` — Docker restart drops the manual gateway route, a known
lab detail also noted in the 10-Nov README).

After reconnect (~05:52:36Z):

```
INFO BMP exporter identity resolved ... exporter_instance=exp:exp-frr-a
status="resolved" device_id=Some("dev-frr-a") binding_id=Some("bind-frr-a")
```

- `exp:exp-frr-a` epoch 5, gap cleared, routes = impostor's
  `192.0.2.0/24` via peers `0.0.0.0` and `10.240.0.50`.
- The impostor obtained the canonical scope AND `dev-frr-a`
  attribution — indistinguishable from a legitimate reconnect because
  identity evidence is entirely self-reported.

Then legit frr-a restarted and reconnected (epoch 6): it reclaimed
`exp:exp-frr-a` once the impostor container was removed. Note: the
impostor's `192.0.2.0/24` (peer `0.0.0.0`) remained as last-known
state — consistent with BN-005 "no fabricated withdrawals on EOF",
but it shows injected routes can persist under a hijacked scope until
explicit withdrawal/peer-down or administrative cleanup.

**Sequential identical-claim impersonation: CONFIRMED STILL OPEN.**
Requires authenticated ingress (BN-021f) — no bounded fix exists
without a non-self-reported ownership proof.

## Restore

`bn021c-frr-c` recreated with `--hostname frr-c`; final state in
`api/speakers-final.json`: `exp:exp-frr-a` epoch 6 (5 routes incl. the
noted residue), `exp:exp-frr-b` epoch 2, `conn:63b7…` (frr-c,
provisional). Lab left running.

# BN-014 complete live verification (10 Nov 2026)

Full BN-014 change-assurance cycle re-verified against **live** BMP data
after the routing-service deployment fix (stray host debug instance
removed; container now receives real BMP on `172.100.207.1:11019`).
This is the first run where baseline/candidate captures reflect actual
routing state rather than journal replay.

## Episode: CHG-BN014-LIVE (`manual:bn014-live-001`)

Speaker note: live BMP arrives under speaker `172.20.0.1` (docker
masquerade collapses frr1+frr2 — documented secondary finding), so the
change's `affected_devices` targets `172.20.0.1`. Peer/route counts are
therefore the merged view of both routers.

| Step | Result |
|---|---|
| Webhook ingest | 200, `success:true` |
| Baseline capture | 5 peers, 6 routes, `errors:[]` |
| Inject: frr2 `neighbor 10.100.1.0 shutdown` (admin-down eBGP to frr1) | confirmed on-device |
| Candidate capture (~25s later) | 5 peers, **4 routes** — live withdrawals visible |
| `verify-invariants` | **overall `fail`** — `no_new_peer_downs` FAIL: "down peers increased baseline=0 candidate=2" (both directions of the session reported down by the two speakers' views) |
| Diff (snapshots endpoint) | `status: diffed`, honest deltas |
| Restore: `no neighbor ... shutdown` | session re-Established |
| Candidate re-capture + verify | **overall `pass`** — all applicable invariants pass |

## What this proves (BN-014 a–d)

- **a)** Webhook → ChangeRequest → baseline/candidate capture pipeline
  works on live data.
- **b)** Independent invariants (`capture_complete`,
  `no_new_peer_downs`, `prefix_retention`,
  `expected_prefixes_present`) evaluate purely from captured snapshots
  and produced a **genuine fail** on a real peer-down event — not a
  synthetic or replayed result.
- **c)** Failure injection + recovery rehearsal works end-to-end with
  durable verdicts (`change_invariant_verdict` ConfigItems).
- **d)** Episode record: this document + persisted snapshots/verdicts
  constitute the change episode audit trail.

## Honest caveats

- Speaker `172.20.0.1` merges frr1+frr2 peer sets (docker-proxy source
  NAT). Peer *states* are still correct per-peer; per-router speaker
  attribution is degraded — follow-up: host-network mode or BMP speaker
  disambiguation (see `2026-11-10-bmp-peerdown-full-investigation`).
- Mid-outage candidate snapshot was overwritten by the post-restore
  candidate (same `css-…-candidate` id, latest-wins upsert — documented
  BN-014b behavior). The fail verdict was likewise overwritten by the
  recovery pass; the fail evidence is captured in this session's
  outputs and the routing-service log excerpt.
- Stale `172.100.207.x` speakers remain in container journal replay;
  they no longer receive live data.

## Evidence files

- `snapshots-final.json` — post-recovery snapshots + diff view
- `invariants-final.json` — persisted verdict records (latest = pass)
- `peers-live.json` — live peer view under speaker 172.20.0.1
- `routing-log-excerpt.txt` — `mtype=2` PeerDown frames +
  `BGP peer down LocalSystemClosed2` manager lines

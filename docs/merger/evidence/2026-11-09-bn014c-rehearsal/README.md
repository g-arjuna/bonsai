# BN-014c — change-window failure/recovery rehearsal (9 Nov 2026)

Slice 3 of BN-014. Rehearsed a controlled failure on the dedicated
containerlab NOS during an open change window and ran the BN-014a/014b
capture + invariant machinery end to end.

## Procedure (all live, packaged `bonsai:latest`)

1. `POST /api/webhooks/change-event` — change `manual:bn014c-rehearsal-001`
   (CHG-BN014C), affected device `172.100.207.12` (= clab
   `bonsai-bn003-r07vrf` **frr2**, AS 65002, BMP speaker → bonsai-routing).
2. `POST /api/changes/{id}/capture {"phase":"baseline"}` → 1 route
   (`10.10.1.0/24`), 2 peers up, `errors: []`.
3. **Inject:** `docker exec clab-bonsai-bn003-r07vrf-frr2 vtysh` →
   `router bgp 65002 / neighbor 10.100.1.0 shutdown` (admin-down the eBGP
   session to frr1). Confirmed `Idle (Admin)` via `show bgp summary`.
4. Waited ~20 s, captured `candidate`, ran `verify-invariants`.
5. **Observed gap (honest finding):** for ~6 minutes the BMP-fed view did
   NOT reflect the shutdown — peers still `{'up': 2}`, route still
   `10.10.1.0/24`, `/api/routing/v1/events` showed no new events (latest
   `occurred_at` remained 2026-11-08). `10.10.1.0/24` is locally
   originated by frr2 (`nexthop 0.0.0.0(frr2)`), so it correctly survives
   a session loss; but the peer state staying `up` means either FRR did
   not emit BMP Peer Down for the admin shutdown, or bonsai-routing did
   not process it. **This is a real observability gap to investigate —
   not papered over.**
6. **Restore:** `no neighbor 10.100.1.0 shutdown`; session re-established
   (`show bgp summary` → Established, see `frr2-bgp-summary.txt`).
7. Re-captured `candidate` (upsert overwrites the earlier candidate
   snapshot — same `css-…-candidate` id) and re-ran `verify-invariants`
   → `overall: pass`, all applicable invariants pass.

## What this proved

- The full change-assurance loop works live: webhook → ChangeRequest →
  baseline/candidate capture → persisted snapshots → independent
  invariant verdicts → recovery re-verification, all durable in the
  graph (`change_state_snapshot` + `change_invariant_verdict` ConfigItems).
- Injection plumbing works (vtysh admin-down confirmed on-device).
- **Gap:** admin neighbor shutdown did not surface in the BMP-fed
  routing view within the observed window — peer-down/withdrawal
  propagation needs investigation before claiming detection coverage
  for this fault class (candidate BN-014c follow-up or BN-016 telemetry
  gap).

## Evidence files

- `snapshots.json` — baseline + post-recovery candidate snapshots
- `invariants.json` — persisted verdict records
- `peers-after-recovery.json` — routing-service peer view post-restore
- `frr2-bgp-summary.txt` — on-device session state after restore

## Commands (repro)

```bash
# inject
 docker exec clab-bonsai-bn003-r07vrf-frr2 vtysh -c "conf t" \
   -c "router bgp 65002" -c "neighbor 10.100.1.0 shutdown"
# restore
 docker exec clab-bonsai-bn003-r07vrf-frr2 vtysh -c "conf t" \
   -c "router bgp 65002" -c "no neighbor 10.100.1.0 shutdown"
# API (admin JWT cookie)
 curl -b cookies -X POST localhost:3000/api/changes/manual:bn014c-rehearsal-001/capture \
   -H 'Content-Type: application/json' -d '{"phase":"candidate"}'
 curl -b cookies -X POST localhost:3000/api/changes/manual:bn014c-rehearsal-001/verify-invariants \
   -H 'Content-Type: application/json' -d '{}'
```

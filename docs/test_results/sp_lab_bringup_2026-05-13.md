# SP Lab Bring-Up Verification — Variant A (SR Linux)
> Date: 2026-05-13 | Sprint: CV5 Sprint 5 T5-3
> Topology: `lab/sp/sp-mpls-srte.clab.yml`
> Spec: `docs/operations/sp_lab_spec.md` (Variant A)
> Operator: fill in pass/fail/notes as you proceed.

---

## Pre-flight checklist

Before deploying, confirm these hold:

| Item | Command | Expected | Pass/Fail |
|------|---------|----------|-----------|
| Docker available | `docker ps` | no error | |
| bonsai-mgmt bridge absent | `docker network ls \| grep bonsai-mgmt` | not listed (new deploy) OR listed (idempotent) | |
| No stale SP containers | `docker ps --filter "name=clab-bonsai-sp"` | empty | |
| SR Linux image available | `docker images ghcr.io/nokia/srlinux` | ≥1 row | |
| FRR image available | `docker images frrouting/frr` | ≥1 row | |
| Bonsai running with BMP enabled | `curl -s localhost:3000/api/topology \| jq .` | 200 OK | |
| Bonsai BMP listener | `ss -tlnp \| grep 5000` | `0.0.0.0:5000` | |

---

## Deploy

```bash
cd /home/arjuna/Desktop/bonsai
sudo containerlab deploy -t lab/sp/sp-mpls-srte.clab.yml --reconfigure
```

Expected: 9 containers created (`srl-pe1`, `srl-pe2`, `srl-pe3`, `srl-rr1`, `srl-rr2`,
`frr-p1`, `frr-p2`, `frr-ce1`, `frr-ce2`) + gobgp-bgp-ls. Containerlab prints
`Nodes status: 10/10 started` (or similar).

---

## Check 1 — Node liveness (all 9 + gobgp)

```bash
docker ps --filter "name=clab-bonsai-sp" --format "table {{.Names}}\t{{.Status}}"
```

| Node | Status | Pass/Fail |
|------|--------|-----------|
| clab-bonsai-sp-srl-pe1 | Up | |
| clab-bonsai-sp-srl-pe2 | Up | |
| clab-bonsai-sp-srl-pe3 | Up | |
| clab-bonsai-sp-srl-rr1 | Up | |
| clab-bonsai-sp-srl-rr2 | Up | |
| clab-bonsai-sp-frr-p1 | Up | |
| clab-bonsai-sp-frr-p2 | Up | |
| clab-bonsai-sp-frr-ce1 | Up | |
| clab-bonsai-sp-frr-ce2 | Up | |
| clab-bonsai-sp-gobgp-bgp-ls | Up | |

---

## Check 2 — IS-IS adjacencies (expect 8 total)

```bash
# frr-p1: expect 3 neighbors (pe1, p2, rr1)
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show isis neighbor"

# frr-p2: expect 4 neighbors (p1, pe2, rr2, pe3)
docker exec clab-bonsai-sp-frr-p2 vtysh -c "show isis neighbor"
```

| Node | Expected adjacencies | Count | Pass/Fail |
|------|---------------------|-------|-----------|
| frr-p1 | 3 (pe1, p2, rr1) | | |
| frr-p2 | 4 (p1, pe2, rr2, pe3) | | |
| Total | 8 | | |

---

## Check 3 — SR-MPLS LFIB (expect ≥6 entries on each P node)

```bash
# Each P node should have MPLS forwarding entries for all 6 remote prefix SIDs
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show mpls table"
docker exec clab-bonsai-sp-frr-p2 vtysh -c "show mpls table"
```

| Node | Expected SIDs in LFIB | Count | Pass/Fail |
|------|-----------------------|-------|-----------|
| frr-p1 | ≥6 (SIDs 101–107 excluding local 104) | | |
| frr-p2 | ≥6 (SIDs 101–107 excluding local 105) | | |

---

## Check 4 — LDP sessions (expect 3 on frr-p1)

```bash
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show mpls ldp neighbor"
```

| Expected peers | State | Pass/Fail |
|---------------|-------|-----------|
| 10.2.0.1 (pe1) | Operational | |
| 10.2.0.5 (p2) | Operational | |
| 10.2.0.6 (rr1) | Operational | |

---

## Check 5 — iBGP VPN-IPv4 sessions on rr1 (expect 5)

```bash
docker exec clab-bonsai-sp-srl-rr1 sr_cli "show network-instance default protocols bgp neighbor"
```

| Expected neighbor | Role | State | Pass/Fail |
|------------------|------|-------|-----------|
| 10.2.0.1 (pe1) | RR client | Established | |
| 10.2.0.2 (pe2) | RR client | Established | |
| 10.2.0.3 (pe3) | RR client | Established | |
| 10.2.0.7 (rr2) | RR peer | Established | |
| 172.100.105.100 (gobgp) | BGP-LS eBGP | Established | |

---

## Check 6 — SR-TE policy on srl-pe1

```bash
docker exec clab-bonsai-sp-srl-pe1 sr_cli "show network-instance default segment-routing sr-policies"
```

| Policy | Color | Endpoint | State | Pass/Fail |
|--------|-------|----------|-------|-----------|
| bonsai-pe1-pe2 | 100 | 10.2.0.2 | active | |

Explicit path: SID 104 (p1) → SID 105 (p2) → endpoint 10.2.0.2

---

## Check 7 — L3VPN reachability: CE1 → CE2 (VRF-A)

```bash
# CE1 prefix: 172.20.1.0/24; CE2 prefix: 172.20.2.0/24
docker exec clab-bonsai-sp-frr-ce1 ping -c 3 172.20.2.1
docker exec clab-bonsai-sp-frr-ce2 ping -c 3 172.20.1.1
```

| Direction | Expected | Pass/Fail |
|-----------|----------|-----------|
| CE1 → CE2 | 0% loss | |
| CE2 → CE1 | 0% loss | |

---

## Check 8 — BMP feed arriving at bonsai

```bash
# Run immediately after deploy; expect Initiation + PeerUp messages within 30s
journalctl -u bonsai --since -2m | grep -i "bmp\|peer_up\|initiation"

# OR check bonsai API for BMP events
curl -s localhost:3000/api/events | head -20
```

| Expected | Pass/Fail | Notes |
|----------|-----------|-------|
| BMP Initiation from ≥1 node | | |
| BMP PeerUp from ≥3 PE sessions | | |
| BMP StatisticsReport within 60s | | |

---

## Check 9 — BGP-LS received at gobgp

```bash
docker exec clab-bonsai-sp-gobgp-bgp-ls gobgp global rib -a ls
```

Expected: ≥7 BGP-LS NLRIs (7 nodes + 8 link descriptors + 7 prefix SIDs).

| Item | Expected count | Actual | Pass/Fail |
|------|---------------|--------|-----------|
| Node NLRIs | 7 | | |
| Link NLRIs | ≥8 | | |
| Prefix NLRIs | ≥7 | | |

---

## Check 10 — Automated health script

```bash
bash scripts/check_lab.sh --topology sp | jq .
```

Expected: `"passed": true`, `"warnings": []`.

Record raw JSON output (or link to artefact file):

```
# paste output here
```

---

## Deviations and notes

> Record any spec deviations here. Minor deviations (wrong BMP syntax version,
> adjacency timing out on first deploy) are expected — document the fix.

| Item | Expected | Actual | Fix applied |
|------|----------|--------|-------------|
| | | | |

---

## Outcome

- [ ] All checks pass → SP lab declared operational. Move to chaos runner targeting SP topology.
- [ ] Partial pass (BMP/BGP-LS only failing) → Acceptable if IS-IS/BGP/SR-MPLS pass. File issues for BMP config.
- [ ] IS-IS or BGP failing → Block. Investigate before chaos.

**Final verdict**: ☐ PASS  ☐ PARTIAL  ☐ FAIL

**Start chaos**: `python scripts/chaos_runner.py chaos_plans/always_on_sp.yaml --fg`

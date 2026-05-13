# SP Lab Specification — CV5 T5-2

> Authored 2026-05-12. Drives bring-up decisions for CV5 Tier 5.
> References: `SP_LAB_VENDOR_RESEARCH.md` (T5-1), `lab/sp/sp-mpls-srte.clab.yml`, `lab/sp/sp-mpls-srte-xrd.clab.yml`.
>
> **Two lab variants are specified here:**
> - **Variant A (SR Linux PEs)** — immediately deployable, covers SR-MPLS + LDP + L3VPN + BMP. Does NOT cover SRv6 (SR Linux container limitation confirmed by T5-1 research).
> - **Variant B (XRd PEs)** — full SP feature surface including SRv6, PCEP, RSVP-TE. Requires Cisco XRd Control Plane image (Cisco CCO account). This is the **recommended production lab** per T5-1.
>
> **Bring-up rule**: spec must be reviewed before `containerlab deploy`. No hit-and-try.

---

## Variant A — Nokia SR Linux PE Lab (immediate use)

**Topology file**: `lab/sp/sp-mpls-srte.clab.yml`

### Node inventory

| Node | Kind | Role | Loopback | SR-MPLS SID |
|------|------|------|----------|-------------|
| srl-pe1 | Nokia SR Linux | PE | 10.2.0.1/32 | SID 101 (index 1) |
| srl-pe2 | Nokia SR Linux | PE | 10.2.0.2/32 | SID 102 (index 2) |
| srl-pe3 | Nokia SR Linux | PE | 10.2.0.3/32 | SID 103 (index 3) |
| srl-rr1 | Nokia SR Linux | RR (iBGP) | 10.2.0.6/32 | SID 106 (index 6) |
| srl-rr2 | Nokia SR Linux | RR (iBGP) | 10.2.0.7/32 | SID 107 (index 7) |
| frr-p1 | FRR 9.x | P (transit) | 10.2.0.4/32 | SID 104 (index 4) |
| frr-p2 | FRR 9.x | P (transit) | 10.2.0.5/32 | SID 105 (index 5) |
| frr-ce1 | FRR 9.x | CE | — | — |
| frr-ce2 | FRR 9.x | CE | — | — |
| gobgp-bgp-ls | gobgp | BGP-LS collector | 172.100.105.100/24 (mgmt) | — |

**SRGB**: base 100, range 900 (SIDs 100–999)

### Link map

| Link | Subnet | pe side | p side |
|------|--------|---------|--------|
| srl-pe1 e1-1 ↔ frr-p1 eth1 | 10.2.10.0/31 | .0 | .1 |
| frr-p1 eth2 ↔ frr-p2 eth1 | 10.2.10.2/31 | .2 | .3 |
| frr-p2 eth2 ↔ srl-pe2 e1-1 | 10.2.10.4/31 | .5 | .4 |
| srl-pe1 e1-2 ↔ srl-rr1 e1-1 | 10.2.10.6/31 | .6 | .7 |
| frr-p1 eth3 ↔ srl-rr1 e1-2 | 10.2.10.8/31 | .8 | .9 |
| frr-p2 eth3 ↔ srl-rr2 e1-1 | 10.2.10.10/31 | .10 | .11 |
| srl-pe2 e1-2 ↔ srl-rr2 e1-2 | 10.2.10.12/31 | .12 | .13 |
| srl-pe3 e1-1 ↔ frr-p2 eth4 | 10.2.10.14/31 | .14 | .15 |
| frr-ce1 eth1 ↔ srl-pe1 e1-3 | 10.2.10.16/31 | .16 (ce) | .17 (pe) |
| frr-ce2 eth1 ↔ srl-pe2 e1-3 | 10.2.10.18/31 | .18 (ce) | .19 (pe) |

### Expected steady state — Variant A

**IS-IS (Level 2, area 49.0001)**:
- 8 IS-IS adjacencies total (all point-to-point, level-2-only):
  - pe1 ↔ p1, pe1 ↔ rr1
  - p1 ↔ p2, p1 ↔ rr1
  - p2 ↔ pe2, p2 ↔ rr2, p2 ↔ pe3
  - pe2 ↔ rr2
- All 7 loopbacks (10.2.0.1–10.2.0.7) in IS-IS RIB on every node

**SR-MPLS**:
- 7 prefix SIDs (SID 101–107) advertised in IS-IS LSPs
- LFIB on each node contains 6 MPLS forwarding entries (all remote prefix SIDs)
- SR-TE policy `bonsai-pe1-pe2` on srl-pe1: explicit path via SID 104 → 105 → endpoint 10.2.0.2 (color 100)

**LDP**:
- LDP sessions: p2p on all backbone links (pe1-p1, p1-p2, p2-pe2, pe3-p2, pe1-rr1, p1-rr1, p2-rr2, pe2-rr2)
- LDP labels allocated for all IS-IS prefixes

**iBGP / L3VPN (AS 65200)**:
- RR cluster: rr1 + rr2. Each PE (pe1, pe2, pe3) is a route-reflector client
- 6 iBGP VPN-IPv4 sessions total (pe1→rr1, pe1→rr2, pe2→rr1, pe2→rr2, pe3→rr1, pe3→rr2)
- rr1 + rr2 peer with each other (iBGP between RRs)
- VRF-A (target:65200:100): pe1 + pe2 + pe3. CE1↔pe1 (AS 65300 eBGP), CE2↔pe2 (AS 65300 eBGP)
- VRF-B (target:65200:200): pe1 + pe3

**BMP**:
- pe1, pe2, pe3, rr1, rr2 all configured as BMP exporters
- BMP station address: bonsai host reachable via bonsai-mgmt bridge (172.100.105.1)
- Expected message types: Initiation, PeerUp (at BGP establishment), RouteMonitoring (VPN-IPv4 prefixes), StatisticsReport (every 30s per session)
- Expected PeerUp messages at steady state: ≥6 (one per PE→RR session pair)

**BGP-LS**:
- gobgp-bgp-ls sidecar peers with srl-rr1 over iBGP (address-family bgp-ls bgp-ls)
- BGP-LS topology visible at gobgp: 7 nodes, 8 links, 7 prefix SIDs
- Bonsai BGP-LS listener receives JSON lines from gobgp bridge script

**BFD**:
- BFD sessions on backbone p2p links (1s interval, both FRR and SR Linux sides)
- BFD for BGP: enabled on PE→RR sessions (failure-detection enable-bfd)

### Feature coverage — Variant A

| Feature | Covered | Notes |
|---------|---------|-------|
| SR-MPLS with prefix SIDs | ✅ | IS-IS + SRGB 100–999 |
| SR-TE explicit path policy | ✅ | pe1→p1→p2→pe2 (color 100) |
| LDP (coexisting with SR-MPLS) | ✅ | All backbone links |
| L3VPN (VPN-IPv4) | ✅ | VRF-A + VRF-B |
| iBGP with RR cluster | ✅ | rr1 + rr2 as reflectors |
| BGP-LU (labeled unicast) | ✅ | SR Linux 26.x supports it |
| BMP export (all PE + RR) | ✅ | RouteMonitoring + PeerUp + Stats |
| BGP-LS export (via gobgp) | ✅ | Topology + prefix SIDs |
| BFD | ✅ | All backbone links + BGP |
| **SRv6** | ❌ | SR Linux container does NOT support SRv6 |
| **RSVP-TE** | ❌ | SR Linux is SR-MPLS-first; no RSVP |
| **PCEP** | ❌ | Not available in SR Linux container |
| IS-IS Flex-Algo | ❌ | Not covered in this spec |

### Bring-up verification checklist — Variant A

```bash
# Deploy
sudo containerlab deploy -t lab/sp/sp-mpls-srte.clab.yml

# 1. IS-IS adjacencies — expect 8 total
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show isis neighbor"
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show mpls table"

# 2. SR-MPLS LFIB — expect 6 entries on frr-p1
docker exec clab-bonsai-sp-frr-p1 vtysh -c "show mpls table"

# 3. BGP sessions on rr1 — expect ≥6 VPN-IPv4 established sessions
docker exec clab-bonsai-sp-srl-rr1 sr_cli "show network-instance default protocols bgp neighbor"

# 4. BMP feed arriving at bonsai (check bonsai logs for BMP Initiation + PeerUp)
journalctl -u bonsai --since -5m | grep "bmp"

# 5. BGP-LS received at gobgp
docker exec clab-bonsai-sp-gobgp-bgp-ls gobgp global rib -a ls

# 6. SR-TE policy installed on pe1
docker exec clab-bonsai-sp-srl-pe1 sr_cli "show network-instance default segment-routing sr-policies"

# 7. L3VPN: ping CE1 from CE2 over VRF-A
docker exec clab-bonsai-sp-frr-ce1 ping -c 3 <ce2-vrf-a-prefix>
```

---

## Variant B — Cisco XRd Control Plane Lab (full SP feature surface)

**Topology file**: `lab/sp/sp-mpls-srte-xrd.clab.yml`

> **Status**: topology file authored (T5-2); **awaiting XRd image** from Cisco CCO.
> The XRd Control Plane image requires a Cisco account. Free CCO accounts work per public reports.
> Download: software.cisco.com → IOS XRd Control Plane → latest 7.x release.
> Image tag once loaded: `ios-xr/xrd-control-plane:7.x.x`

### Node inventory

| Node | Kind | Role | Loopback | SR-MPLS SID | SRv6 locator |
|------|------|------|----------|-------------|--------------|
| xrd-pe1 | cisco_xrd | PE | 10.2.0.1/32 | SID 101 | fc00:0:1::/48 |
| xrd-pe2 | cisco_xrd | PE | 10.2.0.2/32 | SID 102 | fc00:0:2::/48 |
| xrd-pe3 | cisco_xrd | PE | 10.2.0.3/32 | SID 103 | fc00:0:3::/48 |
| xrd-rr1 | cisco_xrd | RR (iBGP) | 10.2.0.6/32 | SID 106 | fc00:0:6::/48 |
| frr-p1 | linux (FRR) | P (transit) | 10.2.0.4/32 | SID 104 | — |
| frr-p2 | linux (FRR) | P (transit) | 10.2.0.5/32 | SID 105 | — |
| frr-ce1 | linux (FRR) | CE | — | — | — |
| frr-ce2 | linux (FRR) | CE | — | — | — |
| gobgp-bgp-ls | linux | BGP-LS collector | 172.100.106.100/24 (mgmt) | — | — |

**SRGB**: base 100, range 900. **SRv6 locator prefix**: fc00:0::/32.

### Expected steady state — Variant B

All Variant A features PLUS:

| Feature | Expected |
|---------|---------|
| SRv6 H.Encaps | End/End.DT4 functions per PE; locator fc00:0:x::/48 |
| SRv6 L3VPN | VRF-A over SRv6 transport (SRH encap at PE ingress) |
| RSVP-TE | Optional; 1 RSVP-TE LSP pe1→pe2 for comparison with SR-TE policy |
| PCEP | xrd-rr1 as PCE; pe1 as PCC; 1 delegated LSP |
| Flex-Algo | Algo 128 (low-latency) on all XRd nodes; IS-IS Flex-Algo advertisement |
| BGP-LS (XR-native) | XRd PEs + rr1 export LS NLRIs directly; richer than SR Linux |

### Bring-up rule for Variant B

**Do not deploy until**:
1. XRd image pulled and `docker images | grep xrd` shows the image
2. Host kernel sysctl tuned: `fs.inotify.max_user_instances=64000; fs.inotify.max_user_watches=64000`
3. `docs/operations/sp_lab_spec.md` (this doc) reviewed and confirmed

---

## Operational notes

### Management network
Both variants use `bonsai-mgmt` (T2-2 invariant):
- Variant A: 172.100.105.0/24
- Variant B: 172.100.106.0/24 (different subnet to allow co-existence during migration)

### Chaos targets for SP lab (T5-4, separate sprint)
Once either variant is up and passing its steady-state checklist:

| Fault type | Target | Expected bonsai detection |
|------------|--------|---------------------------|
| PE uplink failure (shutdown e1-1) | srl-pe1 / xrd-pe1 | IS-IS adjacency loss → BMP PeerDown → blast-radius BFD drop |
| P-P link failure | frr-p1 eth2 ↔ frr-p2 eth1 | IS-IS reroute, SR-TE path change |
| BGP RR session reset | rr1 | Mass PeerDown on all PE RR sessions |
| LDP session loss (p1→p2) | netem delay inject on link | LDP session timeout, fallback to SR-MPLS |
| SR-TE policy degradation | Remove color 100 route on pe1 | SR policy fallback to IGP best path |
| BMP exporter disconnect | Stop BMP daemon on pe2 | Bonsai BMP PeerDown for all pe2 sessions |
| IS-IS adjacency timeout (BFD kill) | netem loss 100% on backbone link | BFD → IS-IS adjacency → BMP PeerDown cascade |

### Resource budget
| Environment | Variant | RAM estimate | Fits OCI 24 GB? |
|------------|---------|-------------|-----------------|
| OCI cloud | A (SR Linux) | ~2.5 GB (9 nodes × 250 MB + gobgp + overhead) | ✅ Yes |
| OCI cloud | B (XRd) | ~11 GB (4 XRd × 1.5 GB + 4 FRR × 100 MB + overhead) | ✅ Yes |

---

*T5-2 output — 2026-05-12. Variant A is immediately deployable against the existing lab YAML. Variant B awaits XRd image. Chaos catalogue (T5-4) is Sprint 5 scope.*

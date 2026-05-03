# SP Lab — SR-MPLS + LDP + L3VPN

Nokia SR Linux + FRR topology exercising SP-scale features: IS-IS SR-MPLS backbone, LDP coexistence, L3VPN with iBGP route reflectors, SR-TE explicit path, and eBGP CE peering.

## Node inventory

| Node | NOS | Mgmt IPv4 | Loopback | SR SID |
|------|-----|-----------|----------|--------|
| srl-pe1 | Nokia SR Linux | 172.100.104.11 | 10.2.0.1/32 | 101 |
| srl-pe2 | Nokia SR Linux | 172.100.104.12 | 10.2.0.2/32 | 102 |
| srl-pe3 | Nokia SR Linux | 172.100.104.13 | 10.2.0.3/32 | 103 |
| srl-rr1 | Nokia SR Linux | 172.100.104.14 | 10.2.0.6/32 | 106 |
| srl-rr2 | Nokia SR Linux | 172.100.104.15 | 10.2.0.7/32 | 107 |
| frr-p1  | FRR 9.x        | 172.100.104.16 | 10.2.0.4/32 | 104 |
| frr-p2  | FRR 9.x        | 172.100.104.17 | 10.2.0.5/32 | 105 |
| frr-ce1 | FRR 9.x        | 172.100.104.18 | 10.2.0.8/32 | — |
| frr-ce2 | FRR 9.x        | 172.100.104.19 | 10.2.0.9/32 | — |

SRGB: 100–999 (SID = 100 + label-index)

## P2P Links

| Link | Subnet | Left | Right |
|------|--------|------|-------|
| pe1 e1-1 ↔ p1 eth1 | 10.2.10.0/31 | .0 | .1 |
| p1 eth2 ↔ p2 eth1 | 10.2.10.2/31 | .2 | .3 |
| p2 eth2 ↔ pe2 e1-1 | 10.2.10.4/31 | .4 | .5 |
| pe1 e1-2 ↔ rr1 e1-1 | 10.2.10.6/31 | .6 | .7 |
| p1 eth3 ↔ rr1 e1-2 | 10.2.10.8/31 | .8 | .9 |
| p2 eth3 ↔ rr2 e1-1 | 10.2.10.10/31 | .10 | .11 |
| pe2 e1-2 ↔ rr2 e1-2 | 10.2.10.12/31 | .12 | .13 |
| pe3 e1-1 ↔ p2 eth4 | 10.2.10.14/31 | .14 | .15 |
| ce1 eth1 ↔ pe1 e1-3 | 10.2.10.16/31 | .16 | .17 |
| ce2 eth1 ↔ pe2 e1-3 | 10.2.10.18/31 | .18 | .19 |

## Features

- **IS-IS Level 2** underlay, area 49.0001, all backbone nodes (SRL + FRR)
- **SR-MPLS**: prefix SIDs on all loopbacks (SRGB 100–999, indices 1–7)
- **LDP**: transport sessions on backbone p2p links (coexisting with SR-MPLS)
- **iBGP VPN-IPv4**: rr1+rr2 as RRs, pe1/pe2/pe3 as clients
- **L3VPN VRF-A**: pe1 (CE1), pe2 (CE2), pe3 (stub); RT target:65200:100
- **L3VPN VRF-B**: pe3 only (asymmetric coverage); RT target:65200:200
- **SR-TE policy** on pe1: color 100, endpoint pe2, explicit via p1(SID 104)→p2(SID 105)
- **BFD** 1 s on all backbone links
- **CE BGP**: frr-ce1/ce2 in AS 65300, eBGP to pe1/pe2 inside VRF-A

## Deploy

```bash
containerlab deploy -t lab/sp/sp-mpls-srte.clab.yml
```

## Verify

```bash
scripts/check_lab.sh --topology sp
```

## FRR config note

FRR containers use `binds` in the topology YAML to mount daemons and frr.conf before supervisord starts — no separate startup script required. The `frrouting/frr:latest` image reads `/etc/frr/daemons` at supervisord init time.

## Known limitations

- RSVP-TE not configured (FRR has limited RSVP support); SR-TE policy covers the explicit-path test use case
- FRR MPLS requires kernel MPLS modules (CONFIG_MPLS, CONFIG_MPLS_ROUTING); ContainerLab host must have these loaded
- `mpls zebra` CLI on FRR nodes required for LDP label allocation; already enabled when ldpd=yes in daemons file

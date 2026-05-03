# DC Lab — EVPN + SRv6 Spine-Leaf

3-tier Nokia SR Linux topology exercising every DC feature bonsai needs to detect and remediate.

## Addressing

| Node | Mgmt IPv4 | Loopback | SRv6 Locator |
|------|-----------|----------|--------------|
| srl-super1 | 172.100.103.11 | 10.1.0.1/32 | fc00:1:1::/48 |
| srl-super2 | 172.100.103.12 | 10.1.0.2/32 | fc00:1:2::/48 |
| srl-spine1 | 172.100.103.13 | 10.1.0.3/32 | fc00:1:3::/48 |
| srl-spine2 | 172.100.103.14 | 10.1.0.4/32 | fc00:1:4::/48 |
| srl-leaf1  | 172.100.103.15 | 10.1.0.5/32 | fc00:1:5::/48 |
| srl-leaf2  | 172.100.103.16 | 10.1.0.6/32 | fc00:1:6::/48 |
| srl-leaf3  | 172.100.103.17 | 10.1.0.7/32 | fc00:1:7::/48 |
| srl-leaf4  | 172.100.103.18 | 10.1.0.8/32 | fc00:1:8::/48 |

## P2P Links

| Link | Subnet | Left addr | Right addr |
|------|--------|-----------|------------|
| super1 e1-1 ↔ spine1 e1-1 | 10.1.10.0/31 | .0 | .1 |
| super1 e1-2 ↔ spine2 e1-1 | 10.1.10.2/31 | .2 | .3 |
| super2 e1-1 ↔ spine1 e1-2 | 10.1.10.4/31 | .4 | .5 |
| super2 e1-2 ↔ spine2 e1-2 | 10.1.10.6/31 | .6 | .7 |
| spine1 e1-3 ↔ leaf1 e1-1  | 10.1.11.0/31 | .0 | .1 |
| spine1 e1-4 ↔ leaf2 e1-1  | 10.1.11.2/31 | .2 | .3 |
| spine1 e1-5 ↔ leaf3 e1-1  | 10.1.11.4/31 | .4 | .5 |
| spine1 e1-6 ↔ leaf4 e1-1  | 10.1.11.6/31 | .6 | .7 |
| spine2 e1-3 ↔ leaf1 e1-2  | 10.1.11.8/31 | .8 | .9 |
| spine2 e1-4 ↔ leaf2 e1-2  | 10.1.11.10/31 | .10 | .11 |
| spine2 e1-5 ↔ leaf3 e1-2  | 10.1.11.12/31 | .12 | .13 |
| spine2 e1-6 ↔ leaf4 e1-2  | 10.1.11.14/31 | .14 | .15 |

## Features

- **IS-IS Level 2** underlay, area 49.0001, p2p links, IPv4+IPv6
- **iBGP EVPN** (AS 65100): super-spines are RRs; all others are clients
- **SRv6 micro-SID**: per-node locator fc00:1:N::/48; advertised via IS-IS
- **VXLAN VTEPs** on leaves: mac-vrf (bridged) + ip-vrf (routed)
  - Tenant-A: VNI 1000 (mac-vrf-a), VNI 100100 (ip-vrf-a), all leaves
  - Tenant-B: VNI 2000 (mac-vrf-b), VNI 100200 (ip-vrf-b), leaf3+leaf4
- **Anycast IRB gateway**: 192.168.100.1/24 (Tenant-A), 192.168.200.1/24 (Tenant-B)
- **BFD** 1 s intervals on all p2p uplinks (container platform minimum)

## Deploy

```bash
containerlab deploy -t lab/dc/dc-evpn-srv6.clab.yml
```

## Verify

```bash
scripts/check_lab.sh --topology dc
```

## Known limitations

- BFD timer 1 s (1 000 000 µs) — ContainerLab/Linux minimum; production targets 100 ms
- IS-IS multi-level (L1/L2 area split) not yet configured; all nodes in single L2 area 49.0001
- SRv6 requires SR Linux 24.x; verify with `docker pull ghcr.io/nokia/srlinux:latest`

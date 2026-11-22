# M6F-FRR live acceptance — 2026-11-22

Executed against the reusable topology `lab/acceptance/m6f-frr/`
(FRR 10.6.2, digest-pinned) with the production Bonsai
`routing_service` BMP listener (`bonsai:latest`, image
`sha256:21c83c85…`, container `bonsai-bonsai-routing-1`,
`BONSAI_ROUTING_BMP_ADDR=0.0.0.0:11019`).

Evidence class: `local_virtual_nos`. No credentials (lab BMP is
unauthenticated inside the task-owned bridge). No secrets present.

## Results

| Check | Outcome | Evidence |
|---|---|---|
| Deploy / BGP peering | PASS | `bgp-summary.txt`: frr-b Established, 1 prefix received |
| Route monitoring (announce) | PASS | `bmp-frame-summary.txt` mtype=0 route-monitoring UPDATEs; RIB `10.99.10.0/24` via 65002 (`bgp-rib.txt`) |
| Route withdrawal | PASS | `no network 10.99.10.0/24` on frr-b → prefix gone from frr-a RIB; BMP mtype=0 UPDATEs with withdrawn NLRI observed in listener log |
| Route re-announce | PASS | `network` restored → prefix re-received (`*> 10.99.10.0/24`), peer Established |
| Peer-down / peer-up events | PASS | `bmp-manager.log`: `BGP peer down … RemoteSystemClosed2` (docker restart), `LocalSystemClosed2` (startup reconvergence), `BGP peer up … peer_as=65002` |
| BMP session reconnect | PASS | `BMP speaker connected speaker=172.100.110.11` ×2 across destroy/redeploy |
| Exporter identity | PASS | speaker IP 172.100.110.11 = frr-a mgmt; peer header carried FRR software string `FRRouting 10.6.2_git`, hostname `frr-a` |
| BGP-LS | UNSUPPORTED | `address-family ?` on bgpd 10.6.2 exposes only ipv4/ipv6/l2vpn — no BGP-LS AFI/SAFI; `show bgp ls` unknown command |
| gNMI | UNSUPPORTED | FRR exposes no OpenConfig gNMI endpoint; daemon gRPC API is not gNMI — not probed as gNMI |

## Operational findings

- `docker restart` on a containerlab `linux`-kind node DESTROYS the
  clab link interfaces (eth1 disappears; only eth0/mgmt remains) —
  links are injected at deploy time, not persisted in the container
  network config. Peer-down testing must use `containerlab
  destroy` + `deploy`, or kill a daemon inside the node.
- FRR `write memory` inside the container rewrites the bind-mounted
  `frr.conf` as container-root — it clobbered the repo file
  (mode/owner changed). Restored from git; do NOT run `write memory`
  in this topology. Runtime `configure terminal` changes suffice for
  ephemeral tests.
- vtysh prints cosmetic `Unknown command[4]: zebra=yes` lines when
  reading the `daemons` file — harmless; daemons start normally.

## Files

- `frr-version.txt` — `FRRouting 10.6.2_git (frr-a)` build string
- `image.txt` — pinned image reference + resolved image ID
- `bgp-summary.txt`, `bgp-rib.txt` — frr-a BGP state/RIB
- `bmp-manager.log` — sanitized Bonsai routing_service BMP manager
  events (speaker IPs, peer up/down, connect) — no payload dumps
- `bmp-frame-summary.txt` — BMP message-type histogram
  (0=route-monitoring, 1=route-mirroring/loc-rib, 2=peer-down,
  3=peer-up, 4=initiation per RFC 7854)

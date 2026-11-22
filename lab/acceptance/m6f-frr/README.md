# M6F-FRR — FRRouting local acceptance

Reusable, version-pinned FRR topology for routing/BMP/BGP-LS acceptance.
FRR is a **protocol platform**, not a gNMI platform: its northbound
daemon gRPC API is not OpenConfig gNMI and is classified
`UNSUPPORTED`/`NOT_EXECUTED` for the gNMI matrix rows — never simulated.

## Image

`quay.io/frrouting/frr@sha256:89e5e1ec8b17c5dffaf57dbac58a29ebeb3731a84e5bc09dad1662adff2ba7f5`
(tag `10.6.2`, inspected 2026-11-22). Single pinned image; no source
build, no retagging of owner images.

## Topology

- `frr-a` AS65001 — BMP exporter (`-M bgpd_bmp`), connects to the host
  mgmt bridge gateway `172.100.110.1:11019`.
- `frr-b` AS65002 — originates only `10.99.10.0/24` (connected /24 on
  `lo`). No default route, nothing toward host/internet.
- Transit: `eth1` /31 `10.100.110.0/31`.
- Management: `m6f-frr-mgmt` `172.100.110.0/24` (task-owned network;
  distinct from all other labs).

## Protocol-appropriate checks

| Check | Method |
|---|---|
| Peer-up / peer-down | `show bgp ipv4 unicast summary` on frr-a before/after frr-b restart |
| Route monitoring | BMP route-monitoring UPDATE for `10.99.10.0/24` at the Bonsai `routing_service` BMP listener |
| Route withdrawal | BMP UPDATE with withdrawn NLRI after `frr-b` config removes the network (vtysh) |
| Multiple peers | add `frr-c` node only when a scenario requires it |
| Restart/reconnect | `docker restart` frr-a; BMP session re-establishment within `min-retry`/`max-retry` bounds |
| Exporter identity | BMP peer header peer-type / distinguisher maps to frr-a, not synthesized |
| BGP-LS | FRR 10.6 `bgpd` link-state AFI/SAFI support must be verified on the live image before claiming PASS; otherwise `UNSUPPORTED`/`NOT_EXECUTED` |
| gNMI | `NOT_EXECUTED` — no OpenConfig gNMI endpoint exists on FRR |

## Running

```sh
containerlab deploy -t topology.clab.yml
# Bonsai side: run the routing_service BMP listener bound on the host
# (BONSAI_ROUTING_BMP_ADDR=0.0.0.0:11019 ./target/debug/bonsai-routing
#  or the compose stack's routing container with the port published).
containerlab destroy -t topology.clab.yml --cleanup
```

Evidence goes to `docs/merger/evidence/<UTC-date>-m6f-frr/` with the
same PASS/FAIL/PARTIAL/UNSUPPORTED/NOT_EXECUTED discipline as the gNMI
vendors. Credentials: none required (lab BMP is unauthenticated inside
the task-owned bridge; mTLS variants are covered by bn021c/bn021f).

Executed 2026-11-22 (`docs/merger/evidence/2026-11-22-m6f-frr/`):
peer-up, route announce/withdraw/re-announce, peer-down
(RemoteSystemClosed2) and BMP reconnect all PASS against the live
`routing_service` listener; BGP-LS UNSUPPORTED on the 10.6.2 image
build (no BGP-LS address-family); gNMI UNSUPPORTED.

Operational caveats (learned live):

- `docker restart` on a clab `linux` node removes the injected link
  interfaces — use `containerlab destroy` + `deploy` for peer/node
  restart scenarios.
- Never run FRR `write memory` here: it rewrites the bind-mounted
  `frr.conf` as container-root and clobbers the repo file. Use
  `configure terminal` for ephemeral changes only.

## Prior executed evidence (same protocol path)

- `docs/merger/evidence/2026-11-08-bn003-genuine-vrf-bmp/` — real FRR
  10.6.2 BMP stream captured and verified (`genuine-bmp-stream.bin`).
- `docs/merger/evidence/2026-11-10-bn021c-nat-acceptance/` — FRR 10.6.1
  BMP through mTLS proxy into Bonsai routing_service.
- `docs/merger/evidence/2026-11-11-bn021f-authenticated-bmp/` —
  authenticated BMP ingress acceptance.
- `docs/merger/evidence/2026-11-09-bmp-peerdown-investigation/` and
  `2026-11-10-bmp-peerdown-full-investigation/` — peer-down semantics.

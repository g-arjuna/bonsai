# BN-003 genuine router VRF BMP export — wire proof PASSED — 8 November 2026

Base commit `8017090` + uncommitted working tree. Supersedes the blocked
attempt in `2026-11-08-bn003-vrf-lab` (root cause: FRR does not create Linux
VRFs; the lab expected zebra to provision BLUE — it cannot and should not).

## What changed vs the failed attempt

Research (`frr_research.txt`, FRR 10.6.2 source + upstream topotest
`test_bgp_bmp_2.py`) established:

1. **FRR does not create Linux VRFs** — the kernel VRF must be provisioned
   as root *before* FRR starts. The repro package does this via containerlab
   node `cmd:` → `bootstrap.sh` (waits for eth1/eth2, `ip link add BLUE type
   vrf table 1001`, `ip link set dev eth2 master BLUE`, dummy `bn003pfx`
   interface with `10.99.1.1/24` on frr2).
2. **RD must be exported explicitly**: `rd vpn export 65000:100` inside
   `router bgp 65001 vrf BLUE` → `address-family ipv4 unicast`. FRR's
   `bgp_bmp.c` copies this export RD into the BMP per-peer distinguisher.
3. **Route must arrive via eBGP** — frr2 originates `10.99.1.0/24` as a real
   connected route inside BLUE; frr1 receives the UPDATE (peer type 1,
   RD-instance). Loc-RIB (peer type 3) is not accepted as proof.
4. Image bumped to `quay.io/frrouting/frr:10.6.2` (maintained patch; BMP
   code identical to 10.6.1).

## Lab (deployed, left running)

`lab/fast-iteration/bn003-frr-bmp-vrf/` — topology `frr-bmp-vrf.clab.yml`,
lab name `bonsai-bn003-vrf`, mgmt `172.100.208.0/24`, privileged nodes,
`bootstrap.sh` pre-FRR VRF provisioning, `verify_bmp.py` wire probe.

## Verification results

- `verify_bmp.py` (real TCP listener on `0.0.0.0:11029`):
  **"BN-003 WIRE PROOF PASSED"** — BMP Peer Up msg=3 peer_type=1
  RD=65000:100, Route Monitoring msg=0 peer_type=1 with UPDATE NLRI
  `10.99.1.0/24`; 5 native frames captured to `genuine-bmp-stream.bin`.
- `show vrf` → `vrf BLUE id 3 table 1001`; `show bgp vrf BLUE ipv4 unicast
  summary` → neighbor 10.100.2.1 Established, PfxRcd 1;
  `show bgp vrf BLUE ... 10.99.1.0/24` → valid best path via eBGP.
- Wire bytes confirm distinguisher `00 00 fd e8 00 00 00 64` = 65000:100.

## Evidence files

- `genuine-bmp-stream.bin` sha256 `59d7a562b4bfe09403b6be8547fe77aac07a5ada9993c88f4428e134526d8ce9`
- `verify_bmp.log` sha256 `c25b51817e29c687926658175d51ba5387c93f4d1c895ed2cfe673bce0b38fe7`

## Not done (BN-003 remainder)

- Bonsai-side ingestion: point the routing service BMP listener at
  `172.100.208.1:11029` (or restart lab after Bonsai binds), then verify
  VRF-scoped RIB population via the acceptance API/journey.
- Deployed browser/graph restart journey still open.
- `verify_bmp.py` exited after capture; lab nodes still running.

## Bonsai-side ingestion — VERIFIED (same day, second phase)

Routing service (`target/debug/bonsai-routing`) bound BMP listener to
`172.100.208.1:11029`, HTTP `127.0.0.1:18081`, journals under
`/tmp/bn003-journal/`, token `bn003-lab-token`. frr1 restarted to force a
fresh BMP session.

API results (captured JSON in this directory):

- `/internal/routing/v1/peers`: **RdInstance peer 65000:100, addr
  10.100.2.1, state up**, AdjRibIn pre+post-policy counts; LocRib peer also
  present (peer_type=3, correctly distinguished).
- `/internal/routing/v1/routes`: `10.99.1.0/24` in all three views —
  LocRib, AdjRibInPrePolicy, AdjRibInPostPolicy — each keyed
  `peer_distinguisher: "65000:100"`, `peer_type: RdInstance`/`LocRib`,
  `is_best`, next_hop 10.100.2.1, exporter identity
  `172.100.208.11`/`session_epoch 1` preserved end to end.
- `/internal/routing/v1/speakers`: speaker `172.100.208.11` sys_name `frr1`
  sys_descr `FRRouting 10.6.2_git`, 2 peers up, 3 routes; one
  `transport_gap` (bmp_tcp_eof) recorded from the deliberate frr1 restart —
  `completeness_state: gap` honestly reflects it (no fabricated state).

This is the first end-to-end proof that a **real router's** VRF-scoped BMP
stream populates Bonsai's VRF-keyed RIB — replacing the synthetic
`build_rd_bmp_frames` fixture for this property.

JSON artifact SHA-256:
- bonsai-health.json `b83abfb26e0fd13b39c3fcb80244247b46938d0ce04be8726dae34ac4898bb44`
- bonsai-peers.json `b7d76df1e529671f7f3ef726448134fb9f310b9d62e987403fd2730505acb7ae`
- bonsai-routes.json `4aa01e26afbe57ca18ff58f64db80fa0cfa130c53c2562e9493e8bb66d5dbd4c`
- bonsai-speakers.json `687ffb036243ed2deef6f559fa8f8bad40968b3402db89fd1e8e6af8f73b146c`

Still open: deployed browser/graph restart journey; acceptance-test
integration (wire genuine stream into
`bn003_live_routing_browser_acceptance.rs` instead of fixture injection).

## r07-addressing variant lab (acceptance-test enabler)

`lab/fast-iteration/frr-bmp-r07-vrf.clab.yml` + `configs-r07vrf/` — same
172.100.207.x addressing and FRR 10.6.1 image the BN-003 acceptance test
uses, with kernel-VRF bootstrap + `rd vpn export 65000:100` + eBGP route.
Deployed as `bonsai-bn003-r07vrf` (left running).

Verified: `show vrf` BLUE active table 1001; `show bgp vrf BLUE ipv4 unicast
summary` → neighbor 10.100.2.1 Established PfxRcd 1; TWO BMP TCP sessions
ESTABLISHED to 172.100.207.1:11019 (global instance + VRF instance target).

Not yet wire-verified on this lab: `verify_bmp.py` reads only the first
accepted connection, so the RD-instance frames on the second connection
weren't decoded; a tcpdump byte-scan was inconclusive. Next: multi-connection
probe or bonsai-routing ingestion on 172.100.207.1:11019 to confirm RD
frames on r07 addressing, then test surgery.

## r07 lab wire proof — CONFIRMED via bonsai-routing (same day, third phase)

Second `bonsai-routing` instance bound `172.100.207.1:11019` (HTTP
`127.0.0.1:18082`, journals `/tmp/bn003-journal-r07/`). `/peers` shows:
`RdInstance 65000:100 10.100.2.1 up` (AdjRibIn pre+post=1), `GlobalInstance
0:0 10.100.1.1`, two LocRib peers correctly distinguished. The r07-addressing
lab emits genuine RD-instance BMP — the acceptance test's lab topology can
replace `build_rd_bmp_frames` fixture injection.

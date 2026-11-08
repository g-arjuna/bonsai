# BN-003 acceptance gate — genuine VRF stream, browser journeys PASSED — 8 November 2026

Base commit `d1df12e` + uncommitted working tree. Supersedes the fixture-based
run in `2026-10-13-bn003`: the `build_rd_bmp_frames` synthetic injection is
removed entirely — every BMP byte ingested came off the wire from FRR 10.6.1.

## What changed

- `tests/bn003_live_routing_browser_acceptance.rs` now deploys
  `lab/fast-iteration/frr-bmp-r07-vrf.clab.yml` (`configs-r07vrf/`, kernel VRF
  BLUE pre-provisioned by `bootstrap.sh`, `rd vpn export 65000:100`, real eBGP
  route `10.99.1.0/24` from frr2 on eth2/10.100.2.0/31). The shared
  `frr-bmp-r07.clab.yml` used by `tests/r07_frr_containerlab_verification.rs`
  is untouched.
- frr1 exports **two concurrent BMP sessions** (global instance + VRF BLUE
  instance) plus frr2's global session — 3 TCP connections on :11019. A unified
  accept manager assigns session epochs: concurrent connections from one
  speaker IP share the epoch; a fresh epoch opens only after the speaker's
  last connection closes (mirrors the `run_bmp_listener` fix below).
- `/bmp/disconnect` now disables BOTH frr1 BMP targets (`bonsai` +
  `bonsai-vrf`); `/bmp/reconnect` re-enables both and waits for the first
  frame on the fresh epoch.
- Positive wire proof: live capture must contain the RD 65000:100
  distinguisher (`00 00 fd e8 00 00 00 64`) — 14 occurrences found.

## Product fix uncovered by genuine multi-instance BMP

`run_bmp_listener` (`src/routing_service/mod.rs`) previously opened a new
speaker session epoch **per TCP connection**. FRR opens one BMP session per
BGP instance, so the second frr1 connection's epoch fenced the first
connection's End-of-RIB as stale — VRF peers stayed `syncing` forever
(reproduced live: `completeness_state: syncing`, RdInstance 65000:100 peers
with empty `eor_families`). Fixed: concurrently live connections from one
exporter IP share the speaker epoch; epoch fencing still applies to genuine
reconnects. Deployed proof below.

## Executed checks

- `cargo test --test bn003_live_routing_browser_acceptance
  test_bn003_live_routing_browser_acceptance -- --nocapture`
  → **1 passed** (30.4s): containerlab deploy, 3 genuine BMP sessions,
  genuine VRF stream verified in RIB (RdInstance 65000:100 peer up,
  10.99.1.0/24 present), **12/12 Playwright Chromium journeys passed**
  (401/403 auth, login+JWT cookie, 3 RIB views, RD isolation 0:0 vs 65000:100,
  modify/withdraw/re-announce, TCP-EOF gap preservation, epoch reconnect,
  prefix history, empty/unsupported states, journal restart, logout replay
  denial).
- Wire proof assert: 14 occurrences of RD 65000:100 in
  `frr_live_network_capture.bin` (6959 bytes,
  sha256 `6859f47e2e887636285979be06b55cb65a03c0162b36be7d74711ee97b126337`).
- `cargo build --bin bonsai-routing` → dev profile OK.

## Deployed journey (running lab + bonsai-routing process)

Lab `bonsai-bn003-r07vrf` redeployed; new `bonsai-routing` bound
`172.100.207.1:11019` (HTTP `127.0.0.1:18082`, journals
`/tmp/bn003-journal-r07/`).

- Before fix (old binary, same lab): `completeness_state: syncing`,
  RdInstance 65000:100 peers `syncing` with `eor_families: {}`.
- After fix: `overall: complete_for_observed_view`, epoch 3, 4 peers up
  (GlobalInstance 0:0 10.100.1.1, RdInstance 65000:100 10.100.2.1, both
  LocRib peers), 3 routes. JSON: `deployed_*.json`.
- Process restart: killed PID, restarted same binary/journals →
  `complete_for_observed_view`, durable_sequence 793, routes preserved,
  new session epoch 4. Graph-projection lag 0 asserted in browser journey 3.

## Files

- `manifest.json` — gate manifest (status Verified, 12 journeys, provenance).
- `frr_live_network_capture.bin` + `.sha256`, `captured_bmp_stream.bin` —
  pure wire capture, no synthetic frames.
- `frr1/frr2_show_*.txt` — router state at teardown.
- `00_…09_*.png` — journey screenshots.
- `deployed_{speakers,peers,routes,routes_vrf,health}.json` — live API proof
  of the epoch fix + restart recovery.
- `sha256sums.txt`.

## Honest limits

- Epoch sharing applies to *concurrently live* connections; if one of several
  sessions dies while siblings live, its reconnect reuses the speaker epoch
  (EOR still clears the gap; routes withdrawn during the gap rely on the
  fresh RIB dump — same-epoch entries are not retired by EOR). Not exercised
  by this gate; noted as a known semantic edge.
- The acceptance test's in-process web router is the same
  `build_public_routing_web_router` used by the deployed app; BN-005 covers
  container replacement separately.
- sccache note: `target/.rustc_info.json` had cached a stale probe failure;
  deleted to unblock builds (documented in AGENTS.md).

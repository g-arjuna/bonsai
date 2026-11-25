# BN-024-04 native FRR interface acquisition evidence

Date: 2026-11-25. Classification: genuine authorized lab observation,
read-only; no customer data and no credentials.

## Live capture and production-path dispatch

- Device/container: `clab-bonsai-bn003-vrf-frr2` at `172.100.208.12`.
- Canonical device identity used in evidence subjects:
  `containerlab:clab-bonsai-bn003-vrf-frr2`.
- Transport: verified SSH, password supplied only through
  `BONSAI_CAPTURE_PASSWORD`; agent/key scanning and ambient user known_hosts
  are disabled; unknown or changed host keys are rejected.
- Fixed commands: `vtysh -c 'show version json'` (release evidence only) and
  `vtysh -c 'show interface json'` (the approved interface acquisition command).
- SSH host key: `ssh-ed25519` `SHA256:w/HyErTm6v1GknV7Mcae7K5xUMVnIVDvlFE5TfSoT1E`.
- Release evidence: FRRouting `10.6.1_git`, hostname `frr2`.
- Required declared immutable artifact qualification:
  `quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b`.
- Interface output: 2,796 bytes, SHA-256
  `b61b12df7142a9fcfacb4718eb9fc2b3210cd91a68122a87254dd093855c489a`.
- Dispatch: `frr-10.6.1-structured-interfaces`, qualification `Qualified`,
  normalizer `frr-iface-json-2026.11-r2`, mapping `bn024-iface-map-3`.
- Result: 4/4 validated `interface.state` typed facts (`eth0`, `eth1`,
  `eth2`, `lo`), 4/4 retained, `shadow_only=true`, no authoritative projection.
- Final genuine Rust evidence run: both env-gated live tests passed — the
  inventory/profile/normalizer acquisition test (0.46 s execution) and the
  production-router collect+read test (7.91 s execution). Direct helper capture
  completed in under one shell second.

Artifacts:

- `direct-capture-envelope.json` — direct fixed-command helper envelope.
- `live-shadow-observation.json` — retained-shape shadow observation emitted by
  the Rust inventory/profile/normalizer path.
- `test-live-final.log` — final genuine Rust live test output.
- `unknown-host-key.stderr.txt`, `wrong-host-key.stderr.txt`,
  `wrong-password.stderr.txt` — genuine negative SSH transport evidence; each
  failed nonzero with zero stdout bytes.

## Lab adaptation and credential cleanup

The pre-existing FRR container did not run sshd. A narrow task-local
`bonsai_obs` account was created inside that lab container, added to the FRR
groups needed for read-only `vtysh`, and `sshd` was started with root login
disabled, password authentication limited to that account, `MaxAuthTries=3`
and `LoginGraceTime=10`. Final verification generated a random ephemeral
password, used it only in process memory/environment, and rotated the account
to a different random unknown password in the same script trap. No password or
private key is present in these artifacts. No lab container was torn down or
rebuilt, and no network-device write/command interface was added.

## Focused verification summary

`test-focused-final11.log` records the final focused ci-profile run:

- production-router integration: 3 passed, 1 ignored live-gated test;
- `frr_interface` lib selection: 24 passed, 1 ignored live-gated test;
- SQLite v5→current migration preserving rows and nullable `software_build`: passed;
- managed-device `software_build` declared-metadata API conversion: passed;
- wall time 1,103.95 s, peak rustc RSS approximately 5.56 GB.

The 24-test selection includes the six `bn02404_repro` regressions and the
bounded acquisition-helper tests. It demonstrates, among other requirements:

- all rejected/invalid decoded records produce `no_usable_records`, not an
  empty-table success;
- three duplicate kernel ifindices never resolve to a valid relationship;
- a truncated address family beyond the retained boundary remains unknown,
  not known-empty;
- malformed capture-time metadata is rejected;
- lower-link parent evidence is distinct from master/LAG membership;
- explicit/numeric disagreement and contradictory slave/type evidence remain
  unresolved rather than becoming topology;
- simultaneous parent and master evidence remains distinct;
- oversized input/output and entity/address budgets are explicit refusals;
- helper deadline expiry terminates and reaps the child; oversized helper
  stdout is an explicit refusal.

Focused performance in that run: genuine fixture average 962.8 µs per
observation over 100 iterations (1,038.6 observations/s); synthetic scale
275.63 µs/interface at 128 interfaces and 152.42 µs/interface at 1024.
This is a ci-profile unoptimized test measurement, not a production capacity
claim.

## Python helper and browser boundary

`python-helper-tests.log`: 8/8 passed. These are mostly mock-channel/unit
tests covering fixed commands, independent stdout/stderr draining, output
limits, command failure, missing credential and deadline closure. They are not
claims of full SSH transport qualification; the genuine transport evidence is
the live positive/negative SSH run above.

`ui-build.log`: production UI build passed. `browser-native-frr-interfaces.log`:
4/4 Chromium tests passed. These tests drive the real built UI but use browser
route fixtures shaped like the authorized API contract; they are component/
browser acceptance, not independent end-to-end backend qualification. They cover
truthful stale/shadow presentation, denied viewer affordance, explicit failed
acquisition state, and device-switch generation fencing.

## Canonical gate result

The first canonical run on published SHA
`a7a50feef3ab6fbafe56933fa3e89103ec1b0377` failed closed at L2 after 664
passed / 1 failed because the two new degraded typed-fact payload states were
present in Rust but absent from the committed JSON schema/state fixture. That
failure is preserved in `gates-l0-l3-first-failure-a7a50fe.log`; L3 was
NOT_EXECUTED by fail-fast. The fixture/schema correction was committed as
`d203e0000ee93c3439f925bb7cc99addeba640e2` without weakening the assertion.
`typed-facts-python.log` shows the schema drift suite actually executing
(7/7 passed) after provisioning the pre-existing test venv with `jsonschema`.

Canonical gates were rerun once on exact production SHA
`d203e0000ee93c3439f925bb7cc99addeba640e2` with the trusted build lane,
prebuilt Ladybug, sccache/mold, `CARGO_BUILD_JOBS=2` and system Chromium.
Result: **L0 PASS, L1 PASS, L2 PASS, L3 PASS** in 2,064.23 s wall time;
peak RSS recorded by `/usr/bin/time` was 6,804,388 KiB. L2 executed 1,298
passed Rust tests across the bonsai library, RustyBMP crates and all 35
required integration targets (10 ignored remain ignored, not counted as
passes). L3 executed 71 passed Rust/browser-acceptance tests across all 13
targets, plus 60/60 root Python tests and 318 passed SDK Python tests with
1 skipped and 1 deselected (neither counted as passed). The Python skip was
verified with `-rs` as the deliberate no-torch compatibility path being
skipped because torch is installed (`python-gate-suites-rs.log`). Full output:
`gates-l0-l3-d203e00.log`. L4 is **NOT_EXECUTED**; this single-NOS bounded
live lab evidence is not the multi-NOS L4 matrix.

During the canonical browser/workflow tests, several historical benchmark
JSON artifacts were regenerated in the working tree as test side effects.
They were restored byte-for-byte from `HEAD` before publication and are not
part of this evidence change.

# B01 implementation and B02 reproductions

11 October 2026 (Asia/Kolkata). Source baselines remain Bonsai `20903963155c27b35a8c511df229d71b848f7aa0` and RustyBMP `e2a1189392db520b25a376795e1bfae72eca5b55`.

RustyBMP changes are isolated in `/home/arjuna/rustybmp-b01`, branch `integration/b01-baseline`. The original `/home/arjuna/rustybmp` worktree and its existing documentation/scenario edits are preserved. Cargo's existing target cache is shared to avoid recompiling native dependencies unnecessarily; verification commands compile against the isolated source. Nothing was committed or pushed.

## Changes

- RustyBMP now selects Rust 1.95.0 through a toolchain file and CI. Workspace `rust-version` is raised to the tested 1.95 policy. The unchanged lockfile includes `time 0.3.49` requiring 1.88; its former 1.85 CI compiler cannot satisfy that requirement. No dependency versions were updated.
- Required Python tests use `scripts/run_required_tests.py`. Assertion failures, skipped test reports, collection-time dependency skips and zero collected tests cannot produce a passing required check. Local optional suites can still use pytest directly.
- RustyBMP CI runs actual `tests/protocol/` and `tests/ml/` directories, removing the nonexistent `bmppy/tests` invocation. Wiring failures are required. API tests have a disposable test backend with bounded readiness, test mode and read-only SQL fixtures; skips fail the required runner. Ruff/mypy/scikit-learn are explicitly installed for the existing checks/tests.
- Buildx explicitly loads the image for smoke/API tests. The verified image is saved/uploaded, then downloaded/loaded in the Playwright job. Backend cleanup runs with `if: always()`. Readiness uses `scripts/wait_http.sh` instead of fixed sleeps. This follows [Docker's load input](https://github.com/docker/build-push-action#inputs) and [GitHub's artifact transfer mechanism](https://docs.github.com/en/actions/tutorials/store-and-share-data).
- Docker's builder declares C/C++/make and curl headers needed by the pinned native dependency build. The runtime declares its C++/zlib libraries. Fetch/build commands use the lockfile; the dependency-cache step no longer hides compilation errors. A disposable `config/ci.toml` uses in-memory storage. The duplicate `[store]` table in the example configuration is repaired.
- Explicitly requested ignored FRR tests now fail when prerequisites are absent. They remain optional/ignored during ordinary workspace runs. Their old FRR 9.1 scenario is not a verified current lab and was not deployed.
- Bonsai's main CI and native x86 artifact build explicitly select 1.95.0 and install native compiler/protobuf/sccache/mold prerequisites. Main CI's Python/npm audits propagate errors instead of discarding them. The existing zstd linker workaround is unchanged; ARM/cross and other workflows still require separate verification.

## Discovery and executed verification

Before B02 regression additions:

| Target | Discovered tests | Passed | Ignored |
|---|---:|---:|---:|
| `rbmp-core` library | 51 | 51 | 0 |
| `rbmp-core` explicit integration target | 18 | 16 | 2 |
| `rbmp-rib` | 37 | 37 | 0 |
| `rbmp-enrichment` | 55 | 55 | 0 |
| `rbmp-mrt` | 15 | 15 | 0 |
| `rustybmp` binary | 52 | 52 | 0 |
| `rbmp-store` | 1 | 1 | 0 |
| Other binary/library/doc targets | 0 | 0 | 0 |
| **Workspace total** | **229** | **227** | **2** |

Commands ran from `/home/arjuna/rustybmp-b01`:

```bash
cargo +1.95.0 fetch --locked
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 test -p rbmp-core -p rbmp-rib --locked -- --list
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 test -p rbmp-core -p rbmp-rib --locked
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 test --workspace --locked -- --list
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 test --workspace --locked
PYTHONDONTWRITEBYTECODE=1 /home/arjuna/rustybmp/.venv/bin/python scripts/run_required_tests.py tests/protocol/ tests/ml/ -p no:cacheprovider -q
bash scripts/check_wiring.sh
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 build -p rbmp-server --bin rustybmp --locked
```

Python protocol/ML: **101 passed** (34 protocol, 67 ML). Wiring: **15/15 passed**. A freshly built host backend, bound to loopback on port 17878 with a temporary vault and in-memory DB, passed **24 API tests**, no skips. Its process was stopped and its temporary state removed after verification. Starting from a cwd without SQL fixtures initially produced 24 skips; the required runner rejected that run. Running with fixture access resolved it and informed the CI mount.

Failure propagation was checked with temporary deliberately failing Rust/Python assertions, a skipped Python test, no tests, a collection-time missing import mixed with a passing test, and the real API suite pointed at an unavailable backend. Rust returns 101, required Python failures/skips return 1, and no tests returns 5. The temporary assertion files were removed. A stub unavailable Docker command verified that both explicitly requested FRR tests fail before any router/container is launched. A missing HTTP backend also fails bounded readiness.

All three edited workflows pass actionlint 1.7.7 (`-shellcheck= -pyflakes=`; those optional external analyzers were not run). Bash syntax, TOML parsing, patch applicability and Git whitespace checks pass. This is local validation; no GitHub Actions job was dispatched.

## B02 preparation — deliberately failing real-parser evidence

`rbmp-core` now has an explicit `bmp_rfc_contract` target at `tests/regressions/bmp_rfc_contract.rs`. It runs **11 tests: 3 pass, 8 fail** against current source. The failures independently reproduce:

- Initiation TLVs 0/1/2 and termination TLVs 0/1 mapped incorrectly.
- Local Instance peer type 2 incorrectly treated as outbound with O clear.
- Loc-RIB filtering checks the wrong flag bit and accepts a reserved bit as filtering.
- Zero/short/truncated declared BMP lengths and incorrect embedded BGP length accepted.

Passing cases cover a six-byte common header with the required Initiation TLVs, outbound O handling across peer types 0–2, and rejection of all ten original invalid generated fixtures. Original fixture bytes remain unchanged and are referenced as negative inputs. Expected values derive from [RFC 7854](https://www.rfc-editor.org/rfc/rfc7854.html), [RFC 8671](https://www.rfc-editor.org/rfc/rfc8671.html) and [RFC 9069](https://www.rfc-editor.org/rfc/rfc9069.html), not the existing generator.

RFC 7854 section 4.3 requires sysDescr and sysName in Initiation. The old integration test's empty six-byte Initiation is a record of existing parser acceptance, not a valid sender conformance vector. The new positive vector includes both required TLVs; malformed-length tests retain that valid body so missing TLVs cannot accidentally explain rejection.

```bash
CARGO_TARGET_DIR=/home/arjuna/rustybmp/target cargo +1.95.0 test -p rbmp-core --test bmp_rfc_contract --locked
```

This exits 101 intentionally because the defects remain unfixed. These tests are not ignored or marked as expected failures. Current workspace discovery includes these 11 additional cases; the earlier 227-pass baseline is **not** a claim that the current full suite passes.

An unapplied [Bonsai unit-test patch](evidence/2026-10-11/bonsai-b02-reproductions.patch) prepares three corresponding initiation/direction regressions. `git apply --check` passes, but these Bonsai cases have not been compiled/executed. No Bonsai parser or RustyBMP parser implementation was changed.

## B01 Docker build, API gate & cross-job image transfer verification (completed)

- Built release image `rustybmp:b01` (`sha256:fce0778a7034d6b1ec1c7aba7330091d1eb853b70c6e9346d178c7a775d740bd`, `134,863,318` bytes).
- Started container `rustybmp-b01-verify` on `127.0.0.1:17879` with `config/ci.toml` and read-only SQL fixtures (`tests/fixtures/sql:/app/tests/fixtures/sql:ro`).
- Verified `/health` via `scripts/wait_http.sh` (`HTTP 200`, `{"status":"ok","version":"0.1.0"}`).
- Ran `scripts/run_required_tests.py tests/api/`: **24 passed in 2.13s, 0 skipped**.
- Tagged `rustybmp:ci`, ran `docker save -o rustybmp-ci.tar rustybmp:ci`, removed `rustybmp:ci`, and verified `docker load -i rustybmp-ci.tar`.
- Evidence saved in [b01-docker-verification.log](evidence/2026-10-11/b01-docker-verification.log).

## B02 implementation — RFC 7854/8671/9069/4271/4760 parser & fixture repairs (completed)

- **RustyBMP (`/home/arjuna/rustybmp-b01`)**:
  - Repaired `PeerFlags::filtered` (`0x80`), `RibType::from_peer` (`O` flag `0x10` for `PeerType::LocalInstance`), 6-byte BMP common header validation, Loc-RIB contextual `F` flag (`0x80` without IPv6 `0x08` collision), Initiation TLVs (`0=String, 1=sysDescr, 2=sysName` + unknown TLV preservation via `parse_initiation_tlvs`), Termination TLVs (`0=String, 1=Reason(u16)`), and BGP OPEN/UPDATE header length checks.
  - Archived the 10 original malformed 7-byte fixtures in `tests/fixtures/bmp/negative_legacy_7byte/` with `manifest.json` SHA-256 provenance and regenerated 10 valid 6-byte fixtures in `tests/fixtures/bmp/*.bin`.
  - Independent byte inspection (`inspect_fixtures.py`): all 10 fixtures (`length_matches: true`) and inline `ROUTE_MONITOR_PDU` (`75/75`, BGP `27/27`) pass ([rustybmp-fixture-inspection-after.json](evidence/2026-10-11/rustybmp-fixture-inspection-after.json)).
  - Required Python suite: **102 passed in 13.16s**; Rust workspace: **241 passed, 0 failed, 2 ignored** ([b02-rustybmp-reproductions-after.log](evidence/2026-10-11/b02-rustybmp-reproductions-after.log)).
- **Bonsai (`/home/arjuna/Desktop/bonsai`)**:
  - Recorded pre-fix B02 failures (`1 passed; 5 failed`) in [b02-bonsai-reproductions-before.log](evidence/2026-10-11/b02-bonsai-reproductions-before.log).
  - Repaired `src/streaming/bmp.rs` (`parse_initiation`, `parse_termination`, `parse_common_peer_header`, `classify_rib_type`, `parse_route_monitoring`) and verified `tests/bmp_rfc_contract.rs` ([b02-bonsai-reproductions-after.log](evidence/2026-10-11/b02-bonsai-reproductions-after.log)).

## B03 implementation — framing bounds, timeouts, public BGP parsers & TLS hardening (completed)

- Added `validate_bmp_header` and bounded `read_bmp_frame` (header + body read timeouts, pre-allocation frame cap) in both RustyBMP (`rbmp-core`, `rbmp-server`) and Bonsai (`src/streaming/bmp.rs`).
- Capped `StatsReport` pre-allocation capacity and hardened public BGP parsers (`attributes.rs`, `capabilities.rs`, `nlri.rs`) against truncated inputs.
- Hardened `collector_protocol.rs`, `proxy.rs`, and `bin/collector.rs` with bounded frame reads and timeouts.
- Fixed `tls.rs` to register CA certs into `RootCertStore`, enforce `WebPkiClientVerifier` when `client_auth_required = true`, and pass `cfg.bmp.max_connections`.
- Verified `bmp_rfc_contract` (`16 passed` in RustyBMP, `7 passed` in Bonsai) and `rbmp-server` `receiver::tests` (`4 passed`).

## Remaining gates and next task

Both repositories have substantial pre-existing formatting drift (`cargo +1.95.0 fmt --all -- --check`) and RustyBMP has 19 pre-existing `clippy` diagnostics in `rbmp-core`. Assertions and lint gates were not weakened, and no broad formatting rewrite was mixed into protocol diffs.

Next bounded implementation is **Milestone M1 (B04 -> B05 -> B06 -> B08)**: typed route/peer/view/event identities (`B04`), session-aware ASN/Add-Path/EOR decoding (`B05`), durable journal & DuckDB commit checking (`B06`), and truthful feature gating (`B08`), before importing the crates into Bonsai (`B10`).


# Bonsai / RustyBMP baseline — 11 October 2026

This report preserves the initial read-only snapshot. Subsequent B01 changes and fresh compiled test results are recorded in [merger/B01_REPORT.md](merger/B01_REPORT.md); consult [merger/STATUS.md](merger/STATUS.md) for current state.

Scope: read the new handoff, inspect both current repositories and determine the first actions. Measurements below were executed on this Ubuntu host. No lab/device configuration, application source, dependency versions, global Docker context or Git history was changed. Documentation and inspection evidence were added inside Bonsai. This is an environment/source baseline, not a release certification.

## Repository state

| Repository | Location | Branch | HEAD | Compared with audit |
|---|---|---|---|---|
| Bonsai | `/home/arjuna/Desktop/bonsai` | `main` | `20903963155c27b35a8c511df229d71b848f7aa0` | Exact match |
| RustyBMP | `/home/arjuna/rustybmp` | `main` | `e2a1189392db520b25a376795e1bfae72eca5b55` | Exact match |

Commands: `git status --short`, `git rev-parse HEAD`, `git branch --show-current`, `git log -6 --oneline` (Bonsai), `git show HEAD:crates/rbmp-core/Cargo.toml` (RustyBMP). No remote freshness check was performed; these are local revisions.

Bonsai's pre-existing modified files: `python/bonsai_ml/syslog_embedding_worker.py`, `python/collector_engine.py`, `src/http_server/ml_jobs.rs`, `ui-bonpy/package.json` (24 insertions, 7 deletions). Untracked: root handoff and `ui-bonpy/.svelte-kit/`. Changes concern embedding response/text handling, logging, graph query fields/filtering and a local launch command. These were inspected and left untouched. Root `AGENTS.md` and `.codex` are empty; no additional `AGENTS.md` was found in either source tree.

RustyBMP's pre-existing modified files: `docs/CODEX_TESTING.md`, `docs/NEXT_SESSION_PROMPT.md`, `docs/TESTING_PROGRESS.md`, `results_and_decisions.md`. Untracked: `tests/scenarios/05_multi_pair_cross_vendor_frr_xrd/`, `ui/playwright-report/`, `ui/test-results/`. Those documents contain earlier test/lab results; they were read as historical reports, not rerun evidence. Preserve this work before creating a clean verification checkout.

## Host and tools

| Command | Observed result |
|---|---|
| `uname -a` | Linux `sai`, kernel `7.0.0-34-generic`, x86_64 |
| `lsb_release -ds` | Ubuntu 24.04.5 LTS |
| `nproc` | 8 |
| `free -h` | 46 GiB RAM, 33 GiB available; 8 GiB swap, unused at inspection |
| `df -h .` | 468 GiB filesystem; 302 GiB available (32% used) |
| `rustc --version` | 1.95.0 (`59807616e`, 2026-04-14) |
| `cargo --version` | 1.95.0 (`f2d3ce0bd`, 2026-03-21) |
| `rustup show` | Installed `stable` and `1.95.0`; Bonsai selects 1.95.0 through `rust-toolchain.toml`; x86_64 Linux target installed |
| `node --version` / `npm --version` | v20.20.2 / 10.8.2 |
| `python3 --version` | 3.12.3 |
| `clang --version` | Ubuntu clang 18.1.3 |
| `cmake --version` | 3.28.3 |
| `protoc --version` | libprotoc 25.3 |
| `pkg-config --version` | 1.8.1 |
| `sccache --version` | 0.15.0 |
| `mold --version` | 2.30.0 |
| `containerlab version` | 0.79.0, commit `5ae50094a` |
| `docker compose version` | v5.3.1 |

`command -v clang cmake protoc pkg-config sccache mold docker containerlab` resolves all tools. Protoc is `/home/arjuna/.local/bin/protoc`; sccache is `/home/arjuna/.cargo/bin/sccache`; Docker CLI is `/home/arjuna/.rd/bin/docker`; other native tools/containerlab are under `/usr/bin`.

Both repositories have Python 3.12.3 `.venv` environments. `python3 -m pytest --version` fails because system Python has no pytest; `/home/arjuna/Desktop/bonsai/.venv/bin/python -m pytest --version` succeeds (9.0.3). RustyBMP's `.venv` successfully executes its protocol suite. No package installation is required merely to use these existing environments.

## Docker endpoints, labs and ports

Initial sandboxed Docker/containerlab inspection was denied by socket restrictions; read-only inspection was retried with approval. Unsandboxed `docker version` still fails: the default `rancher-desktop` context points to unavailable `/home/arjuna/.rd/docker.sock`. This is an endpoint mismatch, not proof that all Docker is unavailable.

`docker --host unix:///var/run/docker.sock version` succeeds: client 29.6.2-rd, system Docker Engine 29.8.1, API 1.56, containerd 2.3.6, runc 1.5.1. Explicit `--host` was used without changing global context.

Read-only commands executed with approval:

```bash
containerlab inspect --all
docker --host unix:///var/run/docker.sock version
docker compose version
docker --host unix:///var/run/docker.sock ps -a --format '{{.Names}}\t{{.Image}}\t{{.Status}}'
docker --host unix:///var/run/docker.sock images --digests --format '{{.Repository}}:{{.Tag}}\t{{.Digest}}\t{{.ID}}\t{{.Size}}'
ss -tlnp
```

Containerlab lists `bonsai-srl`, topology `lab/fast-iteration/3node-srl.clab.yml`: `srl1`, `srl2`, `srl3` all exited, no assigned addresses. System Docker has healthy Kibana, Grafana, Splunk, Elasticsearch and Prometheus containers; NetBox/worker/PostgreSQL/Redis containers are stopped. No RustyBMP FRR/XRd lab is listed as deployed at this inspection. No existing container was started/stopped.

Relevant listening TCP ports: 3001, 5601, 8088, 8100, 9093, 9200; local sccache 4226, local DNS/printing and editor ports. 3000, 50051, 7878 and 11019 do not appear in this snapshot. Recheck before starting services; this snapshot does not reserve ports.

## Locally available NOS images

| Image tag | Digest or local image ID | Size |
|---|---|---|
| `quay.io/frrouting/frr:10.6.1` | `sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b` | 194 MB |
| `frrouting/frr:v8.4.1` | `sha256:0f8c174d95add7916101077d4716822552c758b8ff3d2dcb55104f6534202e3e` | 158 MB |
| `frrouting/frr:latest` | `sha256:990e83490108b686fd6df3b1cafa6bdbb2714acb00eedb9a89693946f46f45ce` | 151 MB |
| `ios-xr/xrd-control-plane:24.4.2` | Local ID `b3bd8e8ea489`; no repository digest reported | 1.3 GB |
| `ghcr.io/nokia/srlinux:latest` | `sha256:ad3d4a533674cf2e2e7321fef38806a6c66e3d56a3e8693c45fc2b4206078dd6` | 2.28 GB |
| `ceos64:4.36.0F` | Local ID `fc203c55adcb`; no repository digest reported | 2.61 GB |

Existing Bonsai/parser images are also available. `rustybmp:ci` and `frrouting/frr:9.1` are absent from this listing. The ignored Rust FRR test still expects `frrouting/frr:9.1`; the documented Python lab uses the available 10.6.1 image. Availability does not prove BMP support or license/redistribution permission. No images were pulled, run, exported or uploaded.

## Test and build baseline

| Check actually executed | Result | Interpretation |
|---|---|---|
| `cargo metadata --no-deps --offline --format-version 1` in both roots | Exit 0 | Manifest targets discovered; no compilation performed |
| `cargo fmt --all -- --check` in Bonsai | Exit 1 | Existing formatting drift: 19,619 output lines, 1,233 diff blocks; no formatting applied |
| `RUSTUP_TOOLCHAIN=1.95.0 CARGO_TARGET_DIR=/home/arjuna/Desktop/bonsai/target/routing-baseline cargo test -p rbmp-core --locked --offline -- --list` from RustyBMP | Cargo exit 101 | `anes v0.1.6` uncached; executable test list not obtained. No fresh Rust count/pass claimed |
| RustyBMP `.venv/bin/python -m pytest -p no:cacheprovider tests/protocol/test_bmp_pdu.py --collect-only -q` | Exit 0; 19 collected | Only BMP fixture/helper tests |
| Same file with `-q` | Exit 0; 19 passed in 0.04 s | Tests accept their own incorrect header convention |
| `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=. .venv/bin/python -m pytest -p no:cacheprovider tests/protocol/ --collect-only -q` from RustyBMP | Exit 0; 34 collected | 19 BMP + 15 convergence tests |
| Same directory with `-q` | Exit 0; 34 passed in 1.04 s | No skips; not a BMP conformance claim |
| `python3 docs/merger/evidence/2026-10-11/inspect_fixtures.py /home/arjuna/rustybmp` | Exit 0; 10/10 header-length mismatches | Independent byte inspection, not parser execution |

An exploratory whole-protocol collection from the Bonsai cwd failed importing `bmppy`; running from the RustyBMP root with explicit `PYTHONPATH=.` resolved it. Use the recorded invocation. Do not convert collection errors into skips. No dependency downloads were attempted after the offline cache miss because this session's scope is analysis; fetching/building is the first B01 verification action.

Target inventories: [Bonsai](merger/evidence/2026-10-11/bonsai-targets.json), [RustyBMP](merger/evidence/2026-10-11/rustybmp-targets.json). Python names: [collected list](merger/evidence/2026-10-11/python-protocol-collected.txt). [Rust blocker](merger/evidence/2026-10-11/rust-test-list-blocker.txt), [format excerpt](merger/evidence/2026-10-11/bonsai-format-summary.txt). Full temporary format output: `/tmp/bonsai-fmt-baseline.log`.

Not executed: fresh compiled Rust tests, workspace Clippy/build, frontend builds/tests, live-router interoperability, crash/replay tests, load tests or deliberately failing CI assertions. Existing cached test binaries were found but were not treated as current-source verification.

## Handoff assessment and first actions

The product direction and correctness-first milestones are appropriate. Source inspection supports the highlighted Bonsai initiation mapping, view classification and peer-transition ordering defects. The ten fixture headers and inline BMP/BGP length discrepancy reproduce exactly. BMP's six-byte header and TLV assignments come from [RFC 7854](https://www.rfc-editor.org/rfc/rfc7854.html#section-4.1); directional O semantics come from [RFC 8671](https://www.rfc-editor.org/rfc/rfc8671.html#section-4); Loc-RIB flags are contextual under [RFC 9069](https://www.rfc-editor.org/rfc/rfc9069.html#section-4.2).

F13 needs correction: RustyBMP already wires the root integration module through `rbmp-core`'s explicit `[[test]]` target at the audited SHA. However, `api_smoke` and `retention_sweep` are omitted from that module; some MCP tests check constructed JSON rather than a running implementation, and the ignored FRR test returns success when its prerequisite image is unavailable. Discovery and behavioural coverage are separate questions.

RustyBMP CI still hides Python failures (`|| echo`), treats wiring checks as advisory, and consumes `rustybmp:ci` in a different job without transferring/building it there. Its Buildx step also lacks an explicit local load. Bonsai CI does not explicitly install the sccache/mold required by repository config. These are source findings; no GitHub Actions run was executed this session.

First implementation: B01 test/CI verification and failure propagation, then B02 independent parser regressions and wire repairs, then B03 framing safety. See [BACKLOG.md](merger/BACKLOG.md) for bounded acceptance criteria and [STATUS.md](merger/STATUS.md) for current state. Keep identity and durability ahead of imports/UI. Preserve both dirty source trees and use an isolated writable checkout for RustyBMP changes.

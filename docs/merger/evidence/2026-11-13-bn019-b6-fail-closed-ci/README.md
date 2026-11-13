# BN-019-B6 — Fail-closed CI and baseline test reliability

Date: 2026-11-13. Branch: `fix/bn019-b6-fail-closed-ci`.

Core invariant: **a green Bonsai validation result must correspond to tests
that genuinely executed and passed.**

## 1. False-green paths found and removed

`Makefile` (all on `main` before this change):

| Target | Defect | Fix |
|--------|--------|-----|
| `test` | `cargo nextest run ... || cargo test ...` — a real nextest failure silently fell back to a second runner | nextest probe is explicit: missing nextest is reported, `cargo test` is the single runner; test failure exits nonzero |
| `test-integration` / `test-all` | `cargo test --test '*' ... || echo "No integration test files found"` — real integration failures were converted into informational success | `|| echo` removed; cargo's exit code propagates |
| `test-python` | `python -m pytest tests/ -v || true` — every Python failure ignored | `|| true` removed |
| `test-ui` | `npm run test:smoke 2>/dev/null || echo "No UI smoke tests configured yet"` — missing script reported as success | target now prints the real UI acceptance path (`tests/ui_driver` via cargo harness / `just verify-release`) instead of faking a pass |
| `lint` | Python lint failures could be masked depending on linter availability | linter absence or failure fails the target |

Remaining `|| true` occurrences in the Makefile are legitimate best-effort
operations (sccache stats display, `cp` into `dist/`, `check-deps`
informational output) and were deliberately preserved.

`justfile` recipes (`check-fast`, `test-protocol`, `test-replay`,
`verify-release`) already fail fast; they are unchanged. New `gate`/`gate-all`
recipes delegate to the canonical script.

## 2. Canonical tiered gates

`scripts/ci/gates.sh` is the single definition consumed by local runs
(`just gate <tier>`), developers, and `.github/workflows/ci.yml`.

- **L0** source checks — rustfmt `--check` scoped to files changed vs
  `GATE_BASE` (default `origin/main`), failing only on *new* drift; files
  already unformatted at the base ref are warned about, not failed (the
  repo carries pre-existing repo-wide fmt drift, e.g. `src/graph/mod.rs`,
  which is not this gate's job to reformat). Python `compileall`,
  `cargo metadata --no-deps`.
- **L1** compile/static — `cargo check --workspace --all-targets
  --profile ci --offline`, `ui` `npm ci && npm run build`, `bonsai_sdk`
  import check.
- **L2** required behavioral — `cargo test -p bonsai --lib`,
  `cargo test -p rbmp-core -p rbmp-rib -p rbmp-store`, and 21 named
  self-contained integration targets (bmp_rfc_contract, config_example,
  ownership_*, b07/b08/b09, bn001/002/004/005_closeout, bn021b–bn021g,
  bn022). One `cargo test` invocation compiles all binaries once, then runs
  them — compile-once, no per-target relink.
- **L3** cross-component — 13 heavier integration targets including the two
  headless-browser acceptance suites (`bn015_u1_inventory_browser`,
  `bn016_u1_topology_browser`, Playwright + Chromium against the real
  router), plus both Python suites (`tests/test_*.py`, `python/tests` —
  the SDK suite auto-skips its `integration` marker).
- **L4** lab/deployment — containerlab FRR (`bn003`, `r07`), env-gated
  `bn006_collection_wiring`, live-acceptance Python scripts, soak. The
  script prints the explicit list and does **not** execute; a green L0–L3
  never implies L4 passed.

Targets excluded from CI and why: `bn003` (containerlab FRR topology),
`bn006_collection_wiring` (requires `BONSAI_BN006_*` live env), `r07`
(containerlab), `bn005_container_mount`/`bn006_*_live_acceptance.py`
(Docker/live devices). These are L4.

## 3. Baseline failures — root causes and fixes

Last reported baseline (5 failures at `9b80cd4` era; 3 remained after PR
#15):

1. **`archive::tests::hourly_writer_limits_one_hour_four_targets_to_four_files`**
   — test arithmetic overflow: `flush` inferred as a small int type;
   `flush * 1_000_000_000` overflowed in debug builds. Fixed by explicit
   `i64` timestamp arithmetic in the fixture. Test-only defect; product
   code unchanged. `src/archive.rs`.

2. **`graph::tests::subscription_status_write_preserves_device_metadata`**
   — real product defect: `write_subscription_status_blocking` MATCHed the
   `HAS_SUBSCRIPTION_STATUS` edge on the raw `host:port` address while
   Device nodes are keyed by `strip_port(address)` — for `host:port`
   devices the edge silently never formed (zero rows). Fixed by
   normalizing through the same canonical address contract used by every
   other graph writer. `src/graph/mod.rs`.

3. **`service_discovery::tests::service_pattern_matching`** — real coverage
   gap: patterns accepted `api_gateway`/`api-gateway`/`apigateway` but not
   the space-separated `"API Gateway - Frontend"` form that real interface
   descriptions use. Fixed by widening the separator class to `[ _-]?` and
   adding positive (space form) + negative (`mgmt-oob` must not match)
   regression assertions. Matching remains evidence-backed — no invented
   service relationships. `src/service_discovery.rs`.

4. **`api_registry_persists_and_emits_changes`** — resolved by PR #15
   (isolated per-test runtime dirs); verified passing, not assumed.

5. **`site_sync` baseline** — resolved by PR #15 site-hierarchy work;
   verified in the 363/363 lib run.

6. **`remediation::trust::tests::persistence_round_trip`** — flaky under
   load, found by the first `gates.sh l2` run (load avg >50 from concurrent
   builds): the test slept a fixed 50 ms racing `TrustStore::persist`'s
   fire-and-forget background write. Fixed deterministically: `TrustStore`
   now tracks spawned `JoinHandle`s and exposes
   `flush_pending_writes()`; the test joins the writer instead of sleeping.
   Async persistence semantics (Q-10) unchanged. `src/remediation/trust.rs`.

Result: `cargo test -p bonsai --lib --offline` → **363 passed, 0 failed**
(64.77 s test execution after compile). See `docs/build-times/runs.csv`.

## 3b. First gate run caught real breakage

The first `gates.sh l2` execution failed at compile time: PR #15 added five
inventory fields to `TargetConfig`, and three integration test files
(`bn021e_snmp_source_identity`, `bn021e_syslog_source_identity`,
`bn021g_collector_trust_foundation`) still used exhaustive struct literals —
E0063 on `main`. These targets had never been compiled by the narrower
test commands used during PR #15 acceptance. Fixed by adding the new fields
to the literals; `cargo check -p bonsai --tests` now passes for **all**
test targets. This is the fail-closed gate working as designed: a target
that cannot compile cannot report green.

## 3c. L3 gate run caught two more masked baseline defects

7. **`a10_a11_a12_operator_and_scenario_pack::a10_operator_adjudication_...`**
   — stale test vs deliberate hardening: internal routing endpoints now
   fail closed via `check_internal_token` (401 without
   `internal_service_token` or the explicit insecure-dev env escape). The
   test used `RoutingServiceConfig::default()` and sent no token. Fixed by
   configuring a test token and sending `x-bonsai-routing-token`, matching
   the pattern sibling tests (bn021c/bn021f/r05_r06) already use.

8. **`b16_b17_b18_b19_m3_release_gates` b17/b19 digest tests** — stale
   oracle serialization: bn021c exporter-isolation changed
   `compute_rib_digest`'s canonical row format (added
   `exporter_instance`, `distinguisher`, `rib_type`, `path_id` fields) but
   the test-side `OracleRib::digest()` kept emitting the pre-change row
   format, so actual-vs-oracle SHA-256 digests could never match. Route
   *counts* still matched — product behavior was correct; only the
   comparison serialization drifted. Fixed by extracting
   `compute_rib_canonical_rows()` (pub) and aligning the oracle row format
   to the canonical contract.

9. **Python SDK suite — 17 masked failures** (`python -m pytest tests/`
   under `cd python`), all previously hidden by Makefile `|| true`:
   - `test_catalog_and_executor` — **real product defect**: two library
     files share `detection_rule_id: bgp_session_down`
     (`bgp_session_down.yaml` multi-vendor, `frr_bgp_session_down.yaml`),
     and `PlaybookCatalog._by_rule` kept only the last doc — nokia_srl
     playbooks were unreachable. Fixed: catalog merges playbook lists per
     rule_id. `python/bonsai_sdk/playbooks/catalog.py`.
   - `test_f11_rule_fixes` (6), `test_t0_fixes` (1) — stale test doubles:
     detection rules now resolve vendor state mappings conservatively
     (unknown vendor ⇒ no detection, deliberate F11 hardening); fakes
     returned `""`/MagicMock vendors. Fixed fakes to return `nokia_srl`;
     t0 test now asserts shared-extractor equality with the rule's vendor
     enrichment excluded (and verified separately).
   - `test_coverage_gap_fill` — environment-dependent: `_use_docker_transport()`
     returns true whenever a docker binary exists; patched to the SSH path
     for determinism.
   - `test_gnn_scaffolding` (2) — stale constants: feature list grew 23→36
     and `DEFAULT_NUM_HEADS` is 8 (set at scaffolding commit); updated.
   - `test_validate_playbooks` (2) — catalog legitimately grew 9→12 docs;
     `OspfNeighbor` is now a real schema label so the negative test needed
     a genuinely unknown label.

Result: `208 passed, 1 skipped, 1 deselected`.

## 3d. Final gate results (ci profile, CARGO_TARGET_DIR=/tmp/ci-target-probe)

| Gate | Result | Wall time |
|------|--------|-----------|
| `gates.sh l0` | PASS | ~3 s |
| `gates.sh l2` | PASS — lib 363/363 + rbmp suites + 21 integration targets, 0 failures | ~22 min incl. bonsai lib recompile |
| `gates.sh l3` | PASS — 13 integration targets incl. bn015/bn016 Playwright/Chromium browser acceptance + root pytest 16 + SDK pytest 208 | ~8 min (binaries warm) |
| `gates.sh l4` | not executed (lab infra required) | — |

Compile-once evidence: one `cargo test` invocation per tier builds all
selected `--test` binaries once, then runs them; L3 reused L2's artifacts.
ci-profile test binaries are ~34–90 MB vs ~850 MB dev-profile — the
`ci` profile (debug=0, incremental=false) is the right CI choice and is
the default in `gates.sh`.

## 4. CI workflow

`.github/workflows/ci.yml` — four jobs on `pull_request` + `push: main`:

| Job | Runner | Contents |
|-----|--------|----------|
| `source-checks` (L0) | ubuntu-latest | gates.sh l0, `GATE_BASE=origin/<base>` |
| `python-tests` | ubuntu-latest | venv + `pip install -e python[dev]`; root `tests/` and `python/tests` pytest |
| `ui-build` | ubuntu-latest | `npm ci && npm run build`; uploads `ui-dist` artifact |
| `rust-gates` (L1–L3) | **self-hosted** `[self-hosted, linux, x64, bonsai-ci]` | toolchain prerequisite check (fail closed), sccache action, downloads `ui-dist`, `playwright install chromium`, `gates.sh l1 l2 l3` |

Runner rationale: a full Bonsai test build needs >100 GB `target/` and
parallel ~1 GB test-binary links exceed GitHub-hosted 16 GB RAM / 14 GB SSD.
The rust job is self-hosted by design; until such a runner is registered it
stays **queued**, which is the correct fail-closed state. Hosted jobs cover
L0, Python and the UI build so partial signal exists without the heavy
runner.

Security: no production credentials; tests use tempdirs, synthetic certs,
mocked NetBox, loopback listeners. No Docker image is built or released by
this workflow (docs-only changes cannot trigger releases). No branch
protection was modified.

Suggested required merge-gate check names once a self-hosted runner
exists: `source-checks`, `python-tests`, `ui-build`, `rust-gates`.

## 5. Failure-injection verification (Phase J)

Local, reversible injections — none committed:

| Injection | Command | Result |
|-----------|---------|--------|
| Missing tool | `env PATH=/usr/bin:/bin scripts/ci/gates.sh l0` | exit 1, `GATE FAIL: required tool missing: cargo` |
| Missing test target | `cargo test -p bonsai --test definitely_missing_target --profile ci --offline` | exit 101, cargo lists available targets |
| Failing Python test | temporary `tests/test_zz_inject_fail.py` (`assert False`) + pytest | exit 1, `1 failed` |
| Failing Rust test | temporary `tests/zz_inject_fail.rs` (`assert_eq!(2+2,5)`) + `cargo test --test zz_inject_fail` | see runs.csv / below |
| Intentional UI failure | covered structurally: `gates.sh` has no `|| true`; Playwright nonzero propagates through the cargo harness (`assert!(status.success())`) — same mechanism proven by bn015/bn016 harnesses | n/a locally |

GitHub Actions runs: **not yet executed** — no self-hosted runner is
registered in this environment, and hosted runners cannot complete the Rust
gates. CI is configured but unverified end-to-end; the workflow file is
syntax-consistent and every command it runs was exercised locally through
`gates.sh`.

## 6. Build efficiency (Phase H)

- `cargo nextest` is **not installed** locally; CI uses pinned
  `cargo test --profile ci` (profile added: `inherits=dev`,
  `incremental=false`, `debug=0`).
- Compile-once: one `cargo test` invocation per tier compiles all selected
  `--test` binaries, then runs them; L2→L3 reuse the same `target/ci`
  fingerprints and sccache.
- `.cargo/config.toml` already mandates `sccache` wrapper + mold; CI sets
  `RUSTFLAGS=-C link-arg=-fuse-ld=mold` and the sccache GHA cache action.
- Measurements: see `docs/build-times/runs.csv` (ci-profile cold build,
  lib suite, injection builds).

## 7. Remaining limitations

- Self-hosted runner provisioning is an infra task outside this PR.
- L4 lab acceptance (containerlab, live NOS, soak) remains manual and is
  explicitly not implied by CI.
- Pre-existing repo-wide rustfmt drift remains (e.g. `src/graph/mod.rs`);
  L0 gates only *new* drift on changed files.
- `cargo test --profile ci` vs `dev` produces different fingerprints;
  measurements are not directly comparable to warm dev numbers.

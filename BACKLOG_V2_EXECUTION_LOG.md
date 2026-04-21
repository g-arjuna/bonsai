# Backlog v2 Execution Log

Living companion to `BONSAI_CONSOLIDATED_BACKLOG_V2.md`.

Purpose:
- keep a shareable, append-only record of backlog execution
- capture the crux of each fix, not just the final status
- record validation evidence and any remaining blockers

## 2026-04-20

### Completed

#### T0-6-cont - shared extractor migration
- Moved BGP, interface, and BFD rule feature extraction onto `extract_features_for_event`.
- Kept `topology.py` poll-based because it does not consume event-driven features.
- Added regression coverage ensuring rule and ML detectors produce matching feature payloads for the same BGP event.
- Validation: focused WSL pytest coverage in `python/tests/test_t0_fixes.py`.

#### T0-7 - ADR debt
- Appended ADR entries covering:
  - event bus seam
  - debounce scope/default
  - retention tie-breaking semantics
  - verification field aliasing
  - shared extractor split
  - typed defaults via `typing.get_type_hints()`

#### T0-8 - retention tie-breaking
- Reworked retention count-cap deletion to delete exact oldest IDs instead of deleting all rows at or before a cutoff timestamp.
- Validation: Rust unit test for same-timestamp collision case plus release test pass.

#### T1-1c - Parquet archive consumer
- Added `src/archive.rs` as an event-bus subscriber writing device/hour-partitioned Parquet batches.
- Added `[archive]` config, archive spawn in `main.rs`, and `scripts/archive_stats.py`.
- Validation:
  - release build and targeted Rust tests passed
  - live-lab smoke run produced Parquet files under an archive output directory

#### T1-3a / T1-3b - dynamic registry + subscriber lifecycle
- Added `ApiRegistry` in `src/registry.rs` with durable local state in `bonsai-registry.json`.
- Seeded the runtime registry from `bonsai.toml` so existing file-configured devices still come up on boot.
- Added managed-device gRPC CRUD in `proto/bonsai_service.proto` and `src/api.rs`:
  - `ListManagedDevices`
  - `AddDevice`
  - `UpdateDevice`
  - `RemoveDevice`
- Extended `TargetConfig` with `role` and `site` so onboarding metadata can flow into later discovery/profile work.
- Refactored `main.rs` so subscribers are controlled from registry state:
  - initial snapshot starts on boot
  - `Added` starts a subscriber
  - `Updated` restarts a subscriber with new settings
  - `Removed` stops a subscriber without restarting Bonsai
- Fixed subscriber cancellation so a healthy telemetry stream reacts to shutdown/remove signals immediately instead of waiting for the stream to fail.
- Validation:
  - `cargo build --release`
  - `cargo test --release api_registry_persists_and_emits_changes -- --nocapture`
  - Windows runtime smoke: Bonsai booted, `/api/readiness` returned `200`, and `bonsai-registry.json` was created

#### T1-3c - DiscoverDevice RPC
- Added `src/discovery.rs` as the gNMI Capabilities probe/report layer.
- Added `DiscoverDevice` to `proto/bonsai_service.proto` and wired it through `src/api.rs`.
- Discovery accepts address, TLS settings, role hint, and credential env var names only; plaintext credentials are not accepted on the API surface.
- Discovery connects to the candidate target, calls Capabilities, reports advertised models, detects vendor/encoding, emits warnings for missing interface/BGP/LLDP models, and returns a built-in profile recommendation.
- The built-in recommendation is intentionally a bridge to `T1-3d`; path profiles still need to move into `config/path_profiles/`.
- Validation:
  - `cargo test --release discovery -- --nocapture`
  - `cargo build --release`
- Tooling note:
  - Python generated stubs under `python/generated/` were regenerated with `scripts/regenerate_python_stubs.ps1` using the verified Windows Python 3.13 + `grpc_tools`.

#### Local workflow hardening
- Added durable helper scripts:
  - `scripts/check_dev_env.ps1` verifies Windows Python, `grpc_tools`, real ripgrep, Cargo, WSL `.venv`, `clab`, and Bonsai readiness.
  - `scripts/search_repo.ps1` bypasses the unreliable Chocolatey `rg.exe` shim and calls the real ripgrep binary.
  - `scripts/regenerate_python_stubs.ps1` regenerates committed Python gRPC stubs without relying on ambiguous `python` / `python3` PATH resolution.
  - `scripts/start_bonsai_windows.ps1` starts the Windows Bonsai binary, waits for `/api/readiness`, records PID/log paths, and normalizes duplicate `Path`/`PATH` environment entries before launch.
  - `scripts/stop_bonsai_windows.ps1` stops the recorded Windows Bonsai process.
- Updated `README.md`, `docs/DEVELOPMENT.md`, and `AGENTS.md` with the Windows/WSL boundary:
  - Windows runs Rust build/test/clippy and the Bonsai core/API/UI process.
  - WSL runs ContainerLab, `clab`, `netem`, chaos plans, and lab-side Python tooling.
  - Proto stub generation uses the Windows helper when WSL is not available.
- Validation:
  - `scripts/search_repo.ps1` found `DiscoverDevice` without hitting the shim.
  - `scripts/regenerate_python_stubs.ps1` completed and generated Python stubs containing `DiscoverDevice`.
  - `scripts/check_dev_env.ps1` confirmed Windows Python 3.13, `grpc_tools`, real ripgrep, Cargo, WSL Python 3.12, WSL `.venv`, and `/usr/bin/clab`.
  - `scripts/start_bonsai_windows.ps1` launched Bonsai and reached HTTP readiness in the sandbox smoke; persistent use should be from a normal Windows PowerShell because the Codex sandbox may clean child processes after a tool call.

#### T1-3d - path profile templates
- Added the initial YAML profile set under `config/path_profiles/`:
  - `dc_leaf_minimal.yaml`
  - `dc_spine_standard.yaml`
  - `sp_pe_full.yaml`
  - `sp_p_core.yaml`
- Added `serde_yaml` parsing in `src/discovery.rs` so `DiscoverDevice` recommendations are now driven by templates instead of hardcoded path construction.
- Each profile path declares required model gates (`required_models` / `required_any_models`), mode, origin, sampling interval, optionality, and rationale.
- Discovery filters each template path against the device's advertised Capabilities models and emits warnings for dropped unsupported paths.
- Kept the previous built-in recommendation logic as a safety fallback if profile files are missing or malformed.
- Validation:
  - `cargo test --release discovery -- --nocapture`
  - `cargo build --release`

#### T1-3e - runtime path verification feedback loop
- Added `src/subscription_status.rs` as the 30-second subscription verifier.
- Subscribers now publish the actual gNMI paths they subscribe to after Capabilities detection and successful Subscribe RPC setup.
- The verifier watches the telemetry bus and writes `SubscriptionStatus` graph nodes for each active path:
  - `pending` when the path is subscribed
  - `observed` when matching telemetry arrives
  - `subscribed_but_silent` when no matching telemetry arrives within 30 seconds
- Added graph schema:
  - `SubscriptionStatus`
  - `HAS_SUBSCRIPTION_STATUS`
- Matching is event-family aware so interface counters, interface oper-state, BGP, BFD, and LLDP updates mark the right subscription class observed even when vendors return child paths or native module prefixes.
- Graph status writes preserve existing `Device.vendor` and `Device.hostname` instead of overwriting them with empty status-writer defaults.
- Validation:
  - `cargo test --release subscription_status -- --nocapture`
  - `cargo test --release subscription_status_write_preserves_device_metadata -- --nocapture`
  - `cargo build --release`

#### T1-3f / T4-1-lite - operator onboarding UI and HTTP facade
- Added HTTP onboarding endpoints on the existing Axum server:
  - `GET /api/onboarding/devices`
  - `POST /api/onboarding/discover`
  - `POST /api/onboarding/devices`
  - `POST /api/onboarding/devices/remove`
- The HTTP facade wraps the existing Rust registry/discovery path rather than adding gRPC-web or a separate frontend transport.
- The UI now has an `Onboarding` workspace:
  - env-var-name-only credential inputs
  - Capabilities discovery report
  - recommended profile summary
  - save/update into runtime registry
  - remove from runtime registry
  - per-device `SubscriptionStatus` display
- Existing `bonsai.toml`-seeded inline lab credentials are preserved when a device is edited from the UI, so the UI does not accidentally wipe credentials it intentionally does not display.
- Validation:
  - `npm run build` in `ui/`
  - `cargo build --release`
  - live HTTP discovery against SR Linux leaf returned `nokia_srl`, `JSON_IETF`, one recommended profile, and 362 advertised models
  - live save/update of `172.100.102.12:57400` succeeded and later reported five `observed` subscription paths
  - registry add/remove smoke for a non-lab placeholder succeeded

#### T1-2a - distributed collector runtime seam
- Added runtime modes in `bonsai.toml`:
  - `all` preserves the existing single-process behavior
  - `core` runs graph/API/UI and accepts collector telemetry
  - `collector` runs local gNMI subscribers and forwards decoded telemetry to core
- Added `TelemetryIngest` as a client-streaming gRPC RPC in `proto/bonsai_service.proto`.
- Added `src/ingest.rs` with:
  - telemetry-to-protobuf conversion
  - protobuf-to-`TelemetryUpdate` conversion
  - collector forwarder loop with reconnect behavior
- `BonsaiService` now republishes accepted ingest updates onto the core event bus, so remote collector telemetry reaches the existing graph writer/status/archive consumers through the same path as local gNMI.
- Regenerated Python gRPC stubs with `scripts/regenerate_python_stubs.ps1`.
- Left T1-2 zstd compression as an explicit follow-up: enabling tonic's zstd feature on this Windows/MSVC build conflicts with LadybugDB's bundled `zstd.lib` and fails with duplicate zstd symbols.
- Validation:
  - `cargo test --release telemetry_ingest_proto_round_trips_json_payload -- --nocapture`
  - `cargo test --release runtime_mode -- --nocapture`

#### T2-4 - playbook validation script
- Added `scripts/validate_playbooks.py` to validate:
  - preconditions
  - placeholder fields
  - verification labels against graph schema
- Added focused tests in `python/tests/test_validate_playbooks.py`.

### In progress

#### T1-3 - dynamic device onboarding
- Complete through `T1-3e`.
- Remaining adjacent work is CLI/UI convenience:
  - `bonsai device add`
  - `bonsai device remove`
  - `bonsai device list`

#### T1-2 - distributed collector architecture
- First slice complete: one-binary runtime modes plus `TelemetryIngest` stream.
- Remaining:
  - gRPC stream compression without conflicting with LadybugDB's bundled zstd on Windows
  - disk-backed collector queue for core outage resilience
  - mTLS between collector and core
  - live two-process validation against the lab

#### T3-2-cont - chaos plans
- Added:
  - `chaos_plans/baseline_mix.yaml`
  - `chaos_plans/bgp_heavy.yaml`
  - `chaos_plans/gradual_only.yaml`
  - `chaos_plans/README.md`
- Refactored chaos execution to a WSL-first workflow because the live ContainerLab lab and `clab` run inside WSL.
- Added helper scripts:
  - `scripts/start_baseline_chaos.sh`
  - `scripts/check_baseline_chaos.sh`
- Started a 1-hour live `baseline_mix` run in the background from WSL.
- Monitoring:
  - thread heartbeat automation checks status periodically
  - status script reports PID, CSV path, row count, and log tail
- Close-out:
  - the live 1-hour `baseline_mix` run completed cleanly on 2026-04-20
  - final CSV: `chaos_runs/20260420T133343Z/injections.csv`
  - total recorded injections: 17
  - review confirmed matching inject/heal timestamps for each row
  - the heartbeat automation was deleted after the run completed

#### T3-3 / T2-5 / T2-3 - readiness + hygiene cluster
- Added `python/bonsai_sdk/training_readiness.py` as the shared home for:
  - minimum bars
  - remediation cutoff
  - anomaly/remediation dataframe validation
  - graph readiness reporting
- Added `scripts/check_training_readiness.py`.
- Updated:
  - `python/train_anomaly.py`
  - `python/train_remediation.py`
  - `python/bonsai_sdk/training.py`
  - `python/export_training.py`
- Data-hygiene cutoff:
  - based on commit `4a5cd707b7e59aa77d3f08a0bffb7a0c3ec72189`
  - documented in `DECISIONS.md`
  - Model C training now filters by post-cutoff `attempted_at_ns`
- Validation:
  - focused WSL pytest run passed: `10 passed`
  - WSL `py_compile` passed for new readiness and training files
- Close-out:
  - added `/api/readiness` on the Bonsai HTTP server so readiness can be queried locally from Windows without a Python `grpc` install
  - updated `scripts/check_training_readiness.py` to auto-fallback from gRPC to HTTP
  - live validation against the running Bonsai instance completed on 2026-04-20
  - added `RemediationTrustMark` graph nodes plus startup backfill so remediation trust is explicit in the graph itself
  - switched Model C export/readiness queries to trusted remediations instead of relying only on a timestamp filter convention
- Remaining:
  - operationally accumulate more trusted `success` outcomes so Model C reaches the training threshold

### Environment / workflow updates
- Standardized Python work on a repo-local `.venv/` created inside WSL.
- Added `docs/DEVELOPMENT.md`.
- Updated README / Phase 4 / agent docs to state:
  - Python and lab tooling run from WSL
  - Rust stays on Windows `--release` workflow for this machine

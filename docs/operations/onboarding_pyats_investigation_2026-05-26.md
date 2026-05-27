# Onboarding And PyATS Investigation

Date: 2026-05-26

## Scope

This document captures the issues observed while restarting onboarding from a clean EV1-style baseline on the Ubuntu ops box, with special focus on:

- clean-slate startup behavior
- device onboarding
- PyATS bootstrap automation
- signal-lab SR Linux compatibility

The intent is to preserve exactly what failed, what was fixed, what remains open, and what the current safest workaround is.

## Environment

- Repository: `bonsai`
- Commit under test: `efd0655`
- Primary clean-start config: `bonsai.toml`
- Signal-lab config previously used for validation: `docker/configs/signal-test.toml`
- Lab addresses exercised during this investigation:
  - `172.100.109.14:57400` (`srl-leaf1`)
  - signal-lab subnet `172.100.109.0/24`

## High-Level Timeline

1. The repo was rebased to current `origin/main`.
2. EV1 build and testing guide review was completed.
3. A clean runtime reset was performed:
   - `runtime/` removed
   - existing `bonsai-credentials` moved aside
   - `bonsai.toml` restored from `bonsai.toml.example`
4. Clean-slate Phase 1 startup was revalidated successfully.
5. Phase 2 onboarding was attempted first through the automated PyATS bootstrap path, then through the discovery/manual path.

## Clean-Slate Startup Issues

### 1. `runtime/` directory missing after reset

Symptom:

- Bonsai failed immediately on startup with:
  - `graph open failed`
  - `Cannot open file /home/arjuna/Desktop/bonsai/runtime/bonsai.db: No such file or directory`

Root cause:

- `bonsai.toml.example` points `graph_path` at `runtime/bonsai.db`, but after deleting `runtime/`, the parent directory no longer existed.

Resolution:

- Recreated `runtime/`
- Restarted Bonsai

Result:

- Clean-slate startup completed successfully.
- Verified:
  - `GET /health` returned `ok`
  - `GET /api/onboarding/devices` returned `{"devices":[]}`
  - `GET /api/topology` returned an empty graph baseline

### 2. Background startup was unreliable for diagnosis

Symptom:

- `nohup ./target/release/bonsai --config bonsai.toml` sometimes exited early with no useful log output.
- Foreground startup consistently showed the real startup path and reached `startup phase="ready"`.

Impact:

- During this investigation, foreground runs were the only reliable way to capture actual failures after reset or rebuilds.

Operational note:

- On this box, when Bonsai appears to "die silently" after a rebuild, prefer a foreground run to capture the real cause before changing code.

## Onboarding And Bootstrap Issues

### 3. Bootstrap agent expected a missing credential resolve API

Symptom:

- `POST /api/devices/bootstrap` failed even though the credential alias existed.
- The bootstrap agent reported that credential resolution failed and received UI HTML instead of credential JSON.

Root cause:

- `python/bootstrap_agent.py` calls:
  - `GET /api/credentials/{alias}/resolve`
- The server did not expose that route.

Relevant files:

- `python/bootstrap_agent.py`
- `src/http_server/managed_devices.rs`
- `src/http_server/mod.rs`

Fix applied:

- Added `resolve_credential_handler` in `src/http_server/managed_devices.rs`
- Added route wiring in `src/http_server/mod.rs`

Result:

- `GET /api/credentials/srl-lab/resolve` now returns:
  - alias
  - username
  - password

### 4. Bootstrap handler launched the wrong Python interpreter

Symptom:

- `POST /api/devices/bootstrap` failed with:
  - `genie is not installed`

Root cause:

- `src/http_server/managed_devices.rs` launched `python3`
- Phase 0 Python setup had been done in the repo `.venv`, not the system interpreter

Fix applied:

- Updated bootstrap command construction in `src/http_server/managed_devices.rs`
- New behavior:
  - prefer `.venv/bin/python` when present
  - fall back to `python3` otherwise

Result:

- The bootstrap path now uses the project Python environment correctly.

### 5. PyATS / Genie dependencies were not present in the project venv

Symptom:

- Even after switching to `.venv`, `genie` and `pyats` were missing.

Action taken:

- Installed into `.venv`:
  - `pyats[full]`
  - `genie`

Result:

- Bootstrap agent can now import PyATS and Genie from the repo venv.

Operational caveat:

- The install warned about a dependency mismatch involving `oci-cli` and `click`, but the PyATS/Genie install itself completed successfully.

### 6. Bootstrap agent treated `host:port` as an SSH hostname

Symptom:

- Direct agent run initially failed with:
  - `translate_host('172.100.109.14:57400') raised gaierror`

Root cause:

- The agent was using the full gNMI address string (`172.100.109.14:57400`) as the SSH host.
- In this lab:
  - `57400` is the gNMI port
  - SSH is on standard port `22`

Fix applied:

- Updated `python/bootstrap_agent.py` to strip the port for SSH/PyATS transport setup while preserving the original address for Bonsai registration and graph seeding.

Result:

- Agent now attempts SSH to `172.100.109.14` correctly.

### 7. Bootstrap agent still targeted an outdated device registration API

Symptom:

- The bootstrap agent was written against an older server contract and attempted to register devices via `/api/devices`.

Root cause:

- Current onboarding API in the server is:
  - `POST /api/onboarding/devices`
- The agent still used:
  - `POST /api/devices`

Fix applied:

- Updated `python/bootstrap_agent.py` to register via `/api/onboarding/devices`
- Included `credential_alias` in the registration payload

Result:

- Bootstrap agent now matches the current onboarding API shape more closely.

## SR Linux / PyATS Tooling Issue

After the API and interpreter mismatches were fixed, the remaining automated-bootstrap failure became much narrower.

### 8. Raw SSH works, but PyATS session bring-up still fails on SR Linux

What was verified:

- TCP reachability to SR Linux SSH works:
  - `nc -vz 172.100.109.14 22` succeeded
- Direct SSH login works:
  - `ssh admin@172.100.109.14` succeeded
- gNMI discovery works when TLS inputs are supplied:
  - correct `ca_cert_path`
  - correct `tls_domain`

Current PyATS failure:

- Direct bootstrap-agent run now fails with:
  - `SSH connect failed: failed to connect to 172.100.109.14:57400`
  - `Failed while bringing device to "any" state`

Important interpretation:

- This is no longer a network problem.
- This is no longer a credential problem.
- This is no longer a missing dependency problem.
- This is now a PyATS/Unicon device-session compatibility problem.

Most likely cause:

- `python/bootstrap_agent.py` maps `nokia_srl` to Genie OS `iosxr`
- SR Linux is not IOS XR
- PyATS/Unicon is likely failing during prompt detection / state machine convergence because the wrong plugin behavior is being used for the SR Linux CLI session

Why this matters:

- The agent can open TCP/SSH at the system level
- But the PyATS session abstraction cannot stabilize the SR Linux shell into a usable learned-device state

This is the main unresolved tooling issue.

## Discovery / Manual Onboarding Findings

### 9. Manual discovery initially failed without lab TLS inputs

Symptom:

- `POST /api/onboarding/discover` against `172.100.109.14:57400` failed with a gNMI transport error.

Root cause:

- Signal-lab SR Linux nodes require:
  - correct CA cert
  - correct TLS domain

Successful request shape:

- `address`: `172.100.109.14:57400`
- `credential_alias`: `srl-lab`
- `ca_cert_path`: `lab/signal-test-lab/clab-bonsai-signal-test/.tls/ca/ca.pem`
- `tls_domain`: `clab-bonsai-signal-test-srl-leaf1`
- `role_hint`: `leaf`

Result:

- Discovery succeeded
- Vendor detected: `nokia_srl`
- Valid path recommendations were returned

Conclusion:

- Manual onboarding / discovery path is viable for SR Linux in this lab, provided the correct TLS metadata is supplied.

## Current Best Working Path

As of this investigation, the safest path for SR Linux onboarding is:

1. Start from the clean Phase 1 Bonsai baseline.
2. Add credential alias:
   - `srl-lab`
3. Run `POST /api/onboarding/discover` with:
   - lab CA cert
   - lab TLS domain
4. Use manual onboarding / selected path application rather than PyATS bootstrap

Reason:

- gNMI discovery is working
- PyATS CLI automation is not yet SR Linux-compatible in the current agent

## Code Changes Made During This Investigation

These changes were applied while narrowing the onboarding failures:

- `src/http_server/managed_devices.rs`
  - added credential resolve handler
  - updated bootstrap handler to prefer `.venv/bin/python`
- `src/http_server/mod.rs`
  - added route for `/api/credentials/{alias}/resolve`
- `python/bootstrap_agent.py`
  - prefer host-only address for SSH transport
  - include credential alias in bootstrap result
  - register through `/api/onboarding/devices`

Related but separate local work also exists in the tree:

- `src/http_server/observability.rs`
- `ui/src/routes/DbManagement.svelte`
- `ui/src/routes/Explorer.svelte`

Those are not part of the onboarding/PyATS diagnosis itself, but they were active in the same working session.

## Open Items — RESOLVED (2026-05-26)

All open items have been fixed in the same session.

### 1. ✅ SR Linux PyATS/Unicon plugin — paramiko-native path added

**Root cause confirmed**: There is NO PyATS/Unicon plugin for SR Linux. The `nokia_srl → iosxr` mapping was fundamentally wrong — SRL's CLI prompt (`A:srl-leaf1#`, `--{ running }--`) is completely different from IOS-XR, so Unicon's state machine could never converge.

**Fix applied**: `bootstrap_device()` now detects `nokia_srl` / `nokia_srlinux` vendor and routes to `_bootstrap_srl()`, a paramiko-based implementation using `sr_cli -d` / `--format json` commands. This is the same SSH pattern proven in `inject_fault.py`.

The SRL-native path collects:
- Hostname via `/system/name/host-name`
- Interfaces via `/interface *` (JSON)
- BGP neighbors via `/network-instance default protocols bgp neighbor *` (JSON)
- LLDP neighbors via `/system lldp interface *` (JSON)
- Platform/chassis details via `/platform chassis`
- OS version via `/system information version`

### 2. ✅ Bootstrap agent error propagation — venv + stderr fix

- Rust bootstrap handler now prefers `.venv/bin/python` over `python3`
- Both single-device and bulk bootstrap handlers updated

### 3. ✅ Address port stripping

- `_strip_port()` helper strips `:57400` (gNMI port) from addresses before SSH
- PyATS testbed `ip` field now uses `ssh_host` (stripped) instead of raw `address`

### 4. ✅ Device registration API alignment

- `_register_device()` now posts to `/api/onboarding/devices` (current server API)
- Falls back to `/api/devices` if the onboarding endpoint is not available

### 5. ✅ Sidecar SRL guard

- `docker/sidecars/pyats/app.py` `/learn` endpoint rejects `nokia_srlinux`/`nokia_srl` with a clear error directing to the paramiko path
- Removed incorrect `nokia_srlinux → sros` Genie OS mapping

## Current Status

Both bootstrap paths should now work on Ubuntu:
- **SRL nodes**: paramiko-native via `_bootstrap_srl()` — no PyATS dependency for these
- **All other vendors**: PyATS/Genie via the existing `device.learn()` path with port-stripped SSH host

## Fresh-Reset Retest Against `origin/main` (2026-05-27)

This follow-up retest was run after:

- hard-resetting the repository to `origin/main`
- removing all uncommitted local files
- rebuilding Bonsai from scratch
- restarting EV1 from the updated `docs/EV1_UBUNTU_TESTING_GUIDE.md`

The intent was to verify the guide as-written and document any remaining drift,
breakage, or backup steps needed to keep the test run moving.

### What matched the updated guide

- The updated EV1 guide is substantially better aligned than the older version:
  - it now explicitly requires hard cleanup of stale Bonsai processes
  - it uses the correct fast-iteration SRL lab addresses:
    - `172.100.100.11`
    - `172.100.100.12`
    - `172.100.100.13`
  - it documents CA extraction and TLS validation before Bonsai startup
- On this host, `clab` worked without `sudo`.
- The fresh lab CA was recreated at:
  - `lab/fast-iteration/clab-bonsai-srl/.tls/ca/ca.pem`
- The live SRL gNMI certificate for `172.100.100.11:57400` presented:
  - CN: `srl1.bonsai-srl.io`
  - SANs:
    - `srl1`
    - `clab-bonsai-srl-srl1`
    - `srl1.bonsai-srl.io`
    - `172.100.100.11`
- A new `bonsai.toml` was created with three static `[[target]]` entries using:
  - `ca_cert = "runtime/tls/clab-ca.pem"`
  - `tls_domain = "srl1" / "srl2" / "srl3"`

### Deviation 1 — UI was missing after clean reset

Symptom:

- Bonsai health was reachable on `:3000`
- but `GET /` returned `404 Not Found`
- the web UI was not visible

Root cause:

- `ui/dist`
- `ui-bonpy/dist`

had both been wiped by the clean reset and had not yet been rebuilt.

Recovery step used:

- `cd ui && npm ci && npm run build`
- `cd ui-bonpy && npm ci && npm run build`

Result:

- `GET /` returned `200 OK`
- the Bonsai web UI became available again

Operational note:

- The EV1 guide should explicitly call out that the web UI will remain blank / 404 after a
  full clean reset until both frontend bundles are rebuilt.

### Deviation 2 — system `python3` was not sufficient after reset

Symptom:

- the first fresh-run bootstrap attempt failed immediately
- direct dependency check showed:
  - `python3` could not import `paramiko`

Root cause:

- the clean reset removed the previous local Python environment
- Bonsai bootstrap depends on the repo `.venv`, not the stripped system Python

Recovery step used:

- created a fresh `.venv`
- installed the EV1 Python dependencies into it
- verified:
  - `.venv/bin/python -c 'import paramiko, requests, yaml'`

Result:

- the SRL bootstrap path could run again through `.venv`

### Deviation 3 — bootstrap created a duplicate non-TLS managed device

Observed behavior:

- bootstrapping `172.100.100.11` succeeded and returned SR Linux data:
  - BGP neighbors
  - LLDP neighbors
  - platform/model
  - OS version
- but bootstrap also registered a second managed-device entry at bare address:
  - `172.100.100.11`

The duplicate device had:

- no `:57400` port
- no `ca_cert`
- no `tls_domain`

Observed symptom:

- Bonsai tried to subscribe to the duplicate with:
  - `tls=false`
- logs showed:
  - `Capabilities RPC failed`
  - `Subscribe RPC failed`

Recovery step used:

- removed the duplicate via:
  - `POST /api/onboarding/devices/remove`
  - body: `{"address":"172.100.100.11"}`

Result:

- only the intended `:57400` targets remained

Operational note:

- This is still a meaningful guide/runtime mismatch:
  - the guide assumes “bootstrap then proceed”
  - the current bootstrap path can mutate managed-device state in a way that creates a
    second, non-TLS entry unless cleaned up

### Deviation 4 — guide references a profile route that is not present

Guide expectation:

- assign profiles via:
  - `PATCH /api/devices/{address}/profile`

Current `origin/main` behavior:

- that route is not present in the active router
- the supported flow is:
  - `GET /api/devices/{address}/recommendations`
  - `POST /api/devices/{address}/selected-paths`

Recovery step used:

- treated recommendations as the profile output
- applied the concrete path list via `selected-paths`

Operational note:

- the EV1 guide should be updated to match the actual API surface of this build

### Deviation 5 — recommended SRL bundle still fails, minimal native bundle works

Fresh-run test result on `172.100.100.11:57400`:

- gNMI readiness on the correct `:57400` target was:
  - `service_status: "reachable"`
  - TLS configured correctly
- recommendations returned the expected spine bundle
- applying the recommended bundle led to:
  - successful gNMI Capabilities
  - `tls=true`
  - repeated `Subscribe RPC failed`

Observed details:

- selected bundle size was 9 paths
- the bundle included SR Linux native paths plus OpenConfig paths
- device state after apply showed:
  - `selected_paths` populated
  - `subscription_statuses: []`
- operations showed:
  - `observed_subscriptions: 0`

Backup step used to keep EV1 moving:

- replaced the failing bundle with the known-good minimal SR Linux native path set:
  - `interface[name=*]/statistics`
  - `interface[name=*]/oper-state`
  - `network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]`
  - `system/lldp/interface[name=*]/neighbor[id=*]`

Result:

- all 4 paths moved to `status: observed`
- `observed_subscriptions` increased to `4`
- telemetry writes resumed

Operational conclusion from the fresh-reset retest:

- The updated EV1 guide is directionally correct and much closer to reality than before.
- The remaining friction points are now much clearer and should be treated as first-class
  guide deviations:
  - UI rebuild is required after a full clean reset
  - `.venv` recreation is required after a full clean reset
  - bootstrap can create a stray non-TLS managed-device entry
  - `/api/devices/{address}/profile` is not the active API route
  - the guide-aligned SRL recommendation bundle still fails at Subscribe time
- The reliable fallback that keeps testing moving is still:
  - remove stray non-TLS managed-device entries
  - keep the intended `:57400` targets
  - apply the minimal SR Linux native path bundle first

### Follow-through on all three fast-iteration nodes

The same fallback flow was then applied across the full 3-node fast-iteration lab:

- `172.100.100.11` bootstrap succeeded, but created a duplicate plain-IP managed device
- `172.100.100.12` bootstrap succeeded, but created a duplicate plain-IP managed device
- `172.100.100.13` bootstrap succeeded, but created a duplicate plain-IP managed device

For each node, the duplicate bare-address entry had to be removed with:

- `POST /api/onboarding/devices/remove`

After cleanup, the correct TLS-backed targets remained:

- `172.100.100.11:57400`
- `172.100.100.12:57400`
- `172.100.100.13:57400`

The same minimal SR Linux native path set was then applied to all three.

Observed final result:

- all three devices reported the four native paths as `status: observed`
- `GET /api/operations` reported:
  - `device_count: 3`
  - `enabled_device_count: 3`
  - `observed_subscriptions: 12`

This is the current best-known working EV1 telemetry baseline for the fast-iteration SRL lab on fresh `origin/main`.

### Additional deviation — interface-down burst after telemetry comes up

Once the minimal native subscriptions became active across the nodes, Bonsai emitted a large
burst of `interface_down` detections for many unused SR Linux interfaces.

Observed operations snapshot:

- `detection_events: 186`
- `state_change_events: 186`
- rule distribution heavily dominated by:
  - `interface_down`

Interpretation:

- The gNMI stream is working
- but the initial state sync still surfaces many admin/oper-down lab interfaces as detections
- this is noisy for EV1 validation because it obscures the “did telemetry come up?” signal

Operational note:

- For EV1 bring-up, treat the first large `interface_down` burst as a known noisy side effect
  of initial SRL telemetry sync on these lab nodes unless/until the interface-state suppression
  path is tightened further.

### Deviation 6 — Phase 4 syslog receiver was not actually available

Guide expectation:

- Bonsai should be listening on UDP `:5514`
- a local `nc -u` injection should appear via:
  - `GET /api/events/history?type=syslog&limit=5`

Observed behavior on fresh `origin/main`:

- `ss -tulnp` showed no listener on `5514`
- injecting a test syslog line to `127.0.0.1:5514` did not fail locally, but no syslog event
  appeared in event history
- `GET /api/events/history?type=syslog&limit=5` returned recent gNMI events rather than a
  filtered syslog view

Operational conclusion:

- Phase 4 could not be validated as written on this build
- there are two separate mismatches to track:
  - the receiver port is not bound
  - the `type=syslog` history filter does not behave as the guide expects

Backup action taken:

- stopped trying to drive additional syslog-only validation once it was clear the receiver
  was not active
- continued testing using the working gNMI telemetry baseline so EV1 validation could keep moving

### Deviation 7 — Phase 5 SNMP trap receiver was also absent

Guide expectation:

- Bonsai should be listening on UDP `:9162`
- a synthetic trap should appear via:
  - `GET /api/events/history?type=snmp&limit=5`

Observed behavior:

- `ss -tulnp` showed no listener on `9162`

Operational conclusion:

- Phase 5 could not be exercised on this live build because the expected trap receiver was not up

### Deviation 8 — sidecar ports and BonPy assumptions do not match this host/build

Guide expectation:

- sidecar health on `:9200`
- Prometheus metrics on `:9201`
- BonPy UI at `/bonpy/`

Observed behavior:

- `GET /api/sidecar/status` returned:
  - `{"sidecars":[]}`
- `ss -tulnp` showed only `:9200` open, not `:9201`
- the process behind `:9200` was not Bonsai sidecar health; querying `/health` there returned
  an Elasticsearch-style `404 index_not_found_exception`
- `GET /bonpy/` returned `404 Not Found`

Additional packaging note:

- after the rebuild, `ui-bonpy` produced assets under:
  - `ui-bonpy/dist-bonpy/`
- there was no:
  - `ui-bonpy/dist/index.html`

Operational conclusion:

- the Phase 9 sidecar assumptions in the EV1 guide do not line up with this runtime
- BonPy is also not wired into the live UI route in the way the guide expects

### Deviation 9 — ML schedule and job APIs differ from the guide

Guide expectation:

- `GET /api/ml/schedules` should show 7 schedules including:
  - `graph_snapshot`
  - `gnn_inference`
  - `syslog_embedding`
  - `config_embedding`
- `POST /api/ml/jobs` should accept:
  - `{"job_id":"graph_snapshot","trigger":"manual"}`

Observed behavior on fresh `origin/main`:

- `GET /api/ml/schedules` returned only 3 schedules:
  - `anomaly_export`
  - `gnn_snapshot`
  - `remediation_export`
- `POST /api/ml/jobs` rejected the guide payload with:
  - `missing field job_type`
- the accepted payload shape is:
  - `{"job_type":"gnn_snapshot","trigger":"manual","config_json":"{}"}`

Additional observation:

- posting a manual ML job with the accepted `job_type` payload returned a new job id
- but `GET /api/ml/jobs?limit=5` still returned an empty job list immediately afterward

Operational conclusion:

- the live ML API contract has diverged from the guide in both:
  - schedule inventory
  - manual trigger payload shape
- there may also be a secondary inconsistency between ML job creation and ML job listing on this build

Current practical EV1 status after these later-phase checks:

- confirmed working:
  - clean 3-node SRL lab bring-up
  - vault credential storage
  - SRL bootstrap
  - duplicate managed-device cleanup
  - minimal SR Linux native gNMI subscriptions
  - live gNMI event and detection creation
- blocked or mismatched on this build:
  - syslog receiver validation
  - SNMP trap validation
  - sidecar health/registration validation
  - BonPy UI route validation
  - guide-aligned ML schedule/job flow validation

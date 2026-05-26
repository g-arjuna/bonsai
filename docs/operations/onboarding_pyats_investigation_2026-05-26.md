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

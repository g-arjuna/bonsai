# Sprint 4 E2E Test Summary

**Date**: 2026-05-03
**Operator**: Arjuna Ganesan
**Bonsai version**: a46544d
**Lab topology**: lab/fast-iteration/bonsai-phase4 (ContainerLab 0.74.3)
**Lab nodes**: srl-spine1, srl-leaf1, srl-leaf2, xrd-pe1

## Overall Result: PASS (3/3 completed tests)

| Test ID | Name | Result | Notes |
|---------|------|--------|-------|
| T2-2 | ContainerLab integration | **PASS** | 4 devices, telemetry, fault inject + detection |
| T2-7 (Prometheus) | Output adapter — Prometheus remote-write | **PASS** | 202 bonsai_* metric series |
| T2-7 (Splunk) | Output adapter — Splunk HEC | SKIP | Image available; deferred — no active faults during window |
| T2-7 (Elastic) | Output adapter — Elasticsearch | SKIP | Image available; deferred — no active faults during window |
| T2-8 | Path profile validation | **PASS** | 4/4 devices on all path checks |
| T2-3 | NetBox enricher | SKIP | Deferred — correct image/compose setup needed |
| T2-4/5 | ServiceNow enricher | SKIP | Deferred — PDI access not yet provisioned |

---

## T2-2: ContainerLab Integration (PASS)

**Result file**: `e2e_containerlab/20260502-pass.md`

- 4 devices discovered and connected in topology
- Interface counter telemetry flowing (all 4 devices, non-zero octets)
- Path profile recommendations returned per device
- Fault injection: `ethernet-1/1` admin-state disable on srl-leaf1
- Detection event fired within 1 second
- Fault healed: `ethernet-1/1` admin-state enable restored

**Fixes applied to `scripts/e2e_containerlab_test.sh` during live run**:
- Health check endpoint: `/health` → `/api/topology`
- Topology file extension: `.yaml` → `.yml`
- Interface counter check: `/api/topology/state` (non-existent) → `/api/devices/{addr}` with `in_octets > 0`
- Fault injection: `clab exec -- sr_cli` → `docker exec -i <container> sr_cli` with heredoc in candidate mode using `commit now`

---

## T2-8: Path Profile Validation (PASS)

**Result file**: `e2e_path_validation/20260502-pass.md`

| Check | Result |
|-------|--------|
| Total devices | 4 |
| With subscription paths | 4/4 |
| With interface counter data | 4/4 |
| With BGP telemetry | 4/4 |
| With LLDP telemetry | 4/4 |
| Critical health | 0 |

**Fixes applied to `scripts/e2e_path_validation_test.sh` during live run**:
- Replaced GET `/api/onboarding/discover?address=...` (POST-only, requires credentials) with `/api/devices/{addr}` for in-band monitoring validation
- Added per-device checks: subscription_statuses, interface counters, BGP sessions, LLDP links, health state

---

## T2-7: Output Adapters (Prometheus PASS; Splunk/Elastic deferred)

**Result file**: `e2e_output_adapters/20260503-prometheus-pass.md`

- Prometheus remote-write receiver started on port 9099
- Adapter registered via POST `/api/adapters` with `{"config": {...}}`
- bonsai restarted (WAL replay ~289s; timeout set to 360s)
- 202 `bonsai_*` metric series visible in Prometheus within 60s
- Cleanup: adapter removed, container stopped

**Fixes applied to `scripts/e2e_output_adapters_test.sh` during live run**:
- API path: `/api/output-adapters` → `/api/adapters`
- Payload: `{"name":..., "type":...}` → `{"config": {"name":..., "adapter_type":...}}`
- Remove endpoint: `DELETE /api/adapters/{name}` → `POST /api/adapters/remove`
- Port: 9090 → 9099 (conflict with bonsai metrics exporter)
- Prometheus container flags: added `--config.file`, `--storage.tsdb.path`, `--web.enable-remote-write-receiver`
- bonsai restart helpers added with LD_LIBRARY_PATH auto-detection and 360s WAL startup timeout

**Splunk / Elastic status**: Docker images pulled (`splunk/splunk:latest` 4.88GB, `elasticsearch:8.12.0` 1.36GB). Tests deferred to next session — no active detection events were firing during the test window which limits Splunk/Elastic validation.

---

## Infrastructure Notes

- **lbug shared library**: bonsai requires `LD_LIBRARY_PATH=target/release/build/lbug-*/out/build/src` for `liblbug.so.0`
- **WAL startup time**: 289s replay for 15MB WAL accumulated during multi-hour lab session; scripts use 360s timeout
- **inotify limit**: XRd requires `fs.inotify.max_user_instances=64000` (kernel default 128 is insufficient)
- **SRL gNMI**: Requires `clab deploy --reconfigure` (not `docker start`) to apply clab-profile that enables port 57400

---

## Deferred Tests

### T2-3: NetBox Enricher
- **Status**: Docker images available (`postgres:16-alpine`, `redis:7-alpine`)
- **Blocker**: Correct NetBox image — `networktocode/netbox:v4.2-3.1.1` does not exist on Docker Hub; use `netboxcommunity/netbox` with proper compose from netbox-community/netbox-docker
- **Script**: `scripts/e2e_netbox_enricher_test.sh` (written, minor fixes needed: `/health` → `/api/topology`, `/api/enrichers` → `/api/enrichment`)

### T2-4/5: ServiceNow Enricher
- **Status**: Script written
- **Blocker**: ServiceNow PDI (Personal Developer Instance) not yet provisioned
- **Next step**: Provision free PDI at developer.servicenow.com

### T2-7 Splunk/Elastic
- **Status**: Images pulled, scripts written and validated (API paths confirmed correct)
- **Next step**: Run with active fault injection to generate detection events for forwarding

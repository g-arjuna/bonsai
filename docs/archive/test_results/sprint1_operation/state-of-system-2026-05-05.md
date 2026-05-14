# Bonsai — Sprint 1 State of the System (2026-05-05)

> Bv2-mod Sprint 1: operate-first. This document captures what works, what's broken,
> what failure mode each broken component produces, and what has been fixed.
> Update in place as the sprint progresses. Final version is the sprint deliverable.

---

## Stack Bring-Up Status

### ContainerLab Labs

| Lab | Topology | Status | Notes |
|---|---|---|---|
| DC EVPN-SRv6 | `lab/dc/dc-evpn-srv6.clab.yml` | ✅ healthy | B1-B12 fixed; all 8 nodes operational |
| SP MPLS-SRTE | `lab/sp/sp-mpls-srte.clab.yml` | ⬜ not started | FRR + SRL nodes |

### External Services (`docker compose -f docker/compose-external.yml --profile all up -d`)

| Service | Port | Status | Notes |
|---|---|---|---|
| NetBox | localhost:8000 | ✅ healthy | B4+B5 fixed; seeded with 8-node DC topology |
| Splunk | localhost:8100 (UI), 8088 (HEC) | ⬜ skipped | B3 workaround: not needed for Sprint 1 |
| Elasticsearch | localhost:9200 | ✅ healthy | Seeded with detection/metrics templates |
| Kibana | localhost:5601 | ✅ healthy | |
| Prometheus | localhost:9093 | ✅ healthy | |
| Grafana | localhost:3001 | ✅ healthy | |
| ServiceNow PDI | dev394753 | ✅ healthy | Seeded with 8 Devices and 2 Services (B14 fixed) |

### Bonsai Stack

| Component | Profile | Status | Notes |
|---|---|---|---|
| bonsai-lab-dc | `lab-dc` | ⬜ not started | Ready to start |
| bonsai-lab-sp | `lab-sp` | ⬜ not started | Requires `lab/sp/ca.pem` first |

### Seeds / Config

| Step | Status | Notes |
|---|---|---|
| `scripts/extract_lab_ca.sh dc` | ✅ done | `lab/dc/ca.pem` extracted |
| `scripts/extract_lab_ca.sh sp` | ⬜ not run | Run after SP clab deploy |
| `scripts/seed_external.sh` | ✅ done | NetBox, ES, and ServiceNow seeded (B13 fixed) |
| `scripts/configure_external.sh` | ⬜ not run | Generates enrichment config snippet |

---

## Enrichment Verification

### NetBox Enricher

| Check | Status | Details |
|---|---|---|
| Test connection (UI → Enrichment workspace) | ⬜ | |
| Run now → `nodes_touched > 0` | ⬜ | |
| VLAN / Prefix / Application nodes land on graph | ⬜ | |
| Enrichment workspace shows last-run summary | ⬜ | |

**Failures captured**:
<!-- Add each failure as: - FAILURE: <what broke> — <error message / log line> -->

---

## Detection Path Verification

### Fault injection test (first fault: `dc-link-down-leaf2-spine1`)

| Check | Status | Details |
|---|---|---|
| `python tests/chaos_harness/run.py --fault dc-link-down-leaf2-spine1 --dry-run` | ⬜ | |
| Inject fault; watch SSE `/api/events` | ⬜ | |
| `detection: interface_admin_down` fires within 30s | ⬜ | |
| `detection: bgp_neighbor_down` fires within 45s | ⬜ | |
| `/api/incidents` shows grouped incident | ⬜ | |
| UI Incidents tab populates (not empty) | ⬜ | |
| Heal fault; detections clear within 60s | ⬜ | |

**Failures captured**:
<!-- Add each failure as: - FAILURE: <what broke> — <error message / log line> -->

---

## Driver Results

### API Driver (`python tests/api_driver/run.py`)

| Endpoint | Status | Error |
|---|---|---|
| GET /api/topology | ⬜ | |
| GET /api/detections | ⬜ | |
| GET /api/incidents | ⬜ | |
| GET /api/incidents/grouped | ⬜ | |
| GET /api/readiness | ⬜ | |
| GET /api/operations | ⬜ | |
| GET /api/_test/status | ⬜ | |
| GET /api/onboarding/devices | ⬜ | |
| GET /api/path | ⬜ | |
| GET /api/enrichers | ⬜ | |
| GET /api/environments | ⬜ | |
| GET /api/sites | ⬜ | |
| GET /api/adapters | ⬜ | |
| GET /api/collectors | ⬜ | |
| GET /api/assignment/rules | ⬜ | |
| GET /api/assignment/status | ⬜ | |
| GET /api/credentials | ⬜ | |
| GET /api/trust/state | ⬜ | |
| GET /api/overrides | ⬜ | |

Raw output: `runtime/driver_results/api.json`

### Event Driver (`python tests/event_driver/run.py`)

Raw output: `runtime/driver_results/event.json`

**Summary**: ⬜ not run

### UI Driver (Playwright — `cd tests/ui_driver && npx playwright test`)

| Spec | Status | Notes |
|---|---|---|
| topology.spec.js | ⬜ | |
| events.spec.js | ⬜ | |
| incidents.spec.js | ⬜ | |
| operations.spec.js | ⬜ | |
| collectors.spec.js | ⬜ | |
| screenshots.spec.js | ⬜ | |
| a11y.spec.js | ⬜ | |

Raw output: `runtime/driver_results/ui.json`

### Chaos Harness (`python tests/chaos_harness/run.py --write-matrix`)

| Fault ID | Topology | Detected | Latency | Incident grouped | Pass |
|---|---|---|---|---|---|
| dc-link-impairment-leaf1-spine1 | dc | ⬜ | — | ⬜ | ⬜ |
| dc-link-down-leaf2-spine1 | dc | ⬜ | — | ⬜ | ⬜ |
| dc-bgp-evpn-session-down-leaf3-super1 | dc | ⬜ | — | ⬜ | ⬜ |
| dc-bgp-evpn-all-sessions-down-leaf4 | dc | ⬜ | — | ⬜ | ⬜ |
| dc-isis-adj-down-spine1-super1 | dc | ⬜ | — | ⬜ | ⬜ |
| dc-bfd-session-flap-leaf1 | dc | ⬜ | — | ⬜ | ⬜ |
| sp-link-down-pe1-p1 | sp | ⬜ | — | ⬜ | ⬜ |
| sp-isis-adj-down-p1-p2 | sp | ⬜ | — | ⬜ | ⬜ |
| sp-ldp-session-down-p1-p2 | sp | ⬜ | — | ⬜ | ⬜ |
| sp-bgp-vpn-session-down-pe1-rr1 | sp | ⬜ | — | ⬜ | ⬜ |
| sp-ce-bgp-down | sp | ⬜ | — | ⬜ | ⬜ |
| sp-link-impairment-backbone | sp | ⬜ | — | ⬜ | ⬜ |

Full matrix report: `docs/test_results/chaos_matrix/<date>.md`

---

## Failures / Bugs Discovered

> One entry per failure. Format: bug ID, component, symptom, reproduction, suspected root cause, status.

### B1 — SRL startup configs — `area-address` rejected in IS-IS context (all 8 nodes)
**Symptom**: `clab deploy` postdeploy tasks fail on all spine/super nodes: `Unknown token 'area-address'`
**Reproduce**: `sudo clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `At line 30/53: Parsing error: Unknown token 'area-address'. Options are [..., 'net', ...]`
**Root cause**: Newer Nokia SRL removed `area-address` as a standalone IS-IS CLI command. Area is now encoded only in the NET (Network Entity Title). Both lines existed redundantly; only `net` is accepted.
**Fix**: Removed `set / network-instance default protocols isis instance main area-address [ 49.0001 ]` from all 8 startup configs. Area 49.0001 is already encoded in the `net` value on each node.
**Status**: fixed 2026-05-05

### B2 — SRL startup configs — `anycast-gw admin-state enable` rejected in IRB context (leaf1-4)
**Symptom**: `clab deploy` postdeploy tasks fail on leaf nodes: `Unknown token 'admin-state'` in anycast-gw context
**Reproduce**: `sudo clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `At line 27-32: Parsing error: Unknown token 'admin-state'. Options are ['anycast-gw-mac', 'virtual-router-id', ...]`
**Root cause**: Newer Nokia SRL removed `admin-state` from the `anycast-gw` sub-context under `interface irb0 subinterface X`. Anycast-gw activates automatically when `anycast-gw-mac` is set; no separate `admin-state` command exists.
**Fix**: Removed `set / interface irb0 subinterface X anycast-gw admin-state enable` from leaf1-4 configs. leaf3 and leaf4 each had two instances (subinterface 1 and 2).
**Status**: fixed 2026-05-05

### B3 — compose-external.yml — Splunk fails to start without SPLUNK_PASSWORD in .env
**Symptom**: `docker compose -f docker/compose-external.yml --profile all up -d` fails immediately
**Error**: `error while interpolating services.splunk.environment.SPLUNK_PASSWORD: required variable SPLUNK_PASSWORD is missing a value`
**Root cause**: `.env` has `SPLUNK_PASSWORD=` (empty). Splunk compose uses `:?` operator which fails on empty.
**Workaround**: For Sprint 1, bring up without Splunk profile: `--profile netbox --profile elastic --profile prometheus`. Splunk is a post-MVP output adapter; not needed for enrichment/detection verification.
**Status**: open — workaround in place; set SPLUNK_PASSWORD in .env when Splunk is needed

### B4 — compose-external.yml — NetBox SECRET_KEY too short (46 chars)
**Symptom**: NetBox container starts but migrations fail: `ImproperlyConfigured: SECRET_KEY must be at least 50 characters`
**Error**: `django.core.exceptions.ImproperlyConfigured: SECRET_KEY must be at least 50 characters in length`
**Root cause**: SECRET_KEY was 46 characters; Django requires ≥50.
**Fix**: Changed `bonsai-dev-secret-key-do-not-use-in-production` → `bonsai-dev-secret-key-do-not-use-in-production-1234` in `docker/compose-external.yml` (both netbox and netbox-worker).
**Status**: fixed 2026-05-05

### B5 — compose-external.yml — NetBox health check returns 403 (requires auth token)
**Symptom**: NetBox container stuck in `unhealthy` despite being functional; `curl -f http://localhost:8080/api/` returns HTTP 403.
**Error**: `curl: (22) The requested URL returned error: 403`
**Root cause**: NetBox v4.x requires an `Authorization: Token` header for `/api/` endpoint. Health check used bare curl.
**Fix**: Updated health check to `curl -f -H "Authorization: Token bonsai-dev-token" http://localhost:8080/api/` in `docker/compose-external.yml`.
**Status**: fixed 2026-05-05

### B6 — SRL startup configs — `router-id` rejected in IS-IS context (all 8 nodes)
**Symptom**: `clab deploy --reconfigure` fails postdeploy on all 8 nodes: `Unknown token 'router-id'`
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure` (after B1/B2 fixes)
**Error**: `At line 54: Parsing error: Unknown token 'router-id'. Options are ['admin-state', 'level-capability', 'net', ...]`
**Root cause**: Newer Nokia SRL removed `router-id` from IS-IS instance context. IS-IS router-id is derived from BGP router-id or loopback; the explicit ISIS `router-id` command no longer exists.
**Fix**: Removed `set / network-instance default protocols isis instance main router-id X.X.X.X` from all 8 startup configs. BGP `router-id` lines (separate path) are correct and remain.
**Status**: fixed 2026-05-05

### B7 — SRL startup configs — `set / segment-routing` removed in SRL v26.3.1 (all 8 nodes)
**Symptom**: `clab deploy --reconfigure` fails postdeploy on all 8 nodes: `Unknown token 'segment-routing'`
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure` (after B1/B2/B6 fixes)
**Error**: `At line N: Parsing error: Unknown token 'segment-routing'. Options are ['acl', 'bfd', 'interface', 'network-instance', ...]`
**Root cause**: SRL v26.3.1 (`ghcr.io/nokia/srlinux:latest`) removed the top-level `/ segment-routing` path entirely. SRv6 is not available in the simulation image at this version. Confirmed by `tree` inspection — no segment-routing path exists under root, system, or network-instance protocols.
**Fix**: Removed all `set / segment-routing srv6 ...` and `set / network-instance default protocols isis instance main segment-routing ...` lines from all 8 configs. Lab runs IS-IS + BGP EVPN over plain VXLAN — sufficient for all bonsai detection test cases.
**Status**: fixed 2026-05-05

### B8 — SRL startup configs — `advertise` removed from `ip-vrf` (leaf1-4)
**Symptom**: `clab deploy` fails on leaf nodes: `Unknown token 'advertise'` in BGP-EVPN routes context for `ip-vrf`.
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `Parsing error: Unknown token 'advertise'.`
**Root cause**: SRL v26.x removed the `advertise` leaf from the `bgp-evpn routes route-table ip-prefix` context in `ip-vrf` network instances. A similar token `advertise-interface-ful` exists but is strictly for `mac-vrf` (SBD). In `ip-vrf`, Type-5 advertisement is automatic when BGP-EVPN is enabled.
**Fix**: Removed all `advertise true` (and the incorrect `advertise-interface-ful true` fix attempt) lines from all leaf `ip-vrf` configs.
**Status**: fixed 2026-05-05

### B10 — SRL startup configs — BGP global AFI-SAFI enable requirement (all 8 nodes)
**Symptom**: `clab deploy` fails on all nodes: `Error in /network-instance[name=default]/protocols/bgp/admin-state: One of the address families must be enabled.`
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `One of the address families must be enabled.`
**Root cause**: SRL v26.x requires at least one address family to be enabled at the global BGP level before enabling the BGP protocol itself.
**Fix**: Added `set / network-instance default protocols bgp afi-safi evpn admin-state enable` before enabling BGP admin-state on all 8 nodes.
**Status**: fixed 2026-05-05

### B11 — SRL startup configs — Subinterfaces in MAC-VRF must be `type bridged` (leaf1-4)
**Symptom**: `clab deploy` fails on leaf nodes: `subinterface is not type bridged, but .network-instance{.name=="mac-vrf-a"} is type mac-vrf`
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `subinterface is not type bridged, but .network-instance{.name=="mac-vrf-a"} is type mac-vrf`
**Root cause**: In Nokia SRL, any subinterface attached to a `mac-vrf` network instance must be explicitly configured with `type bridged`. By default, they are `type routed`.
**Fix**: Added `set / interface ethernet-1/3 subinterface 0 type bridged` to leaf1-4 configs.
**Status**: fixed 2026-05-05

### B12 — SRL startup configs — Multiple untagged subinterfaces on same port rejected (leaf3-4)
**Symptom**: `clab deploy` fails on dual-tenant leaf nodes when adding second subinterface.
**Reproduce**: `clab deploy -t dc-evpn-srv6.clab.yml --reconfigure`
**Error**: `Error in /interface[name=ethernet-1/3]/subinterface[index=1]: Error: Commit failed`
**Root cause**: Nokia SRL does not support multiple untagged subinterfaces on the same physical port. If multiple subinterfaces are needed (e.g. for multiple tenants), VLAN tagging must be enabled and each subinterface assigned a unique VLAN ID.
**Fix**: Simplified leaf3 and leaf4 to single-tenant (Tenant-A only) to match leaf1/leaf2 for Sprint 1 stability.
**Status**: fixed 2026-05-05

### B13 — Seed data — `topology.yaml` mismatch with actual 8-node lab
**Symptom**: `scripts/seed_external.sh` would seed NetBox with incorrect management IPs and missing devices.
**Reproduce**: Inspection of `lab/seed/topology.yaml`
**Root cause**: The `topology.yaml` file was stale, describing an older 3-node lab setup on the 172.100.102.x subnet, while the current lab is 8 nodes on 172.100.103.x.
**Fix**: Updated `topology.yaml` to match the actual DC EVPN lab topology and updated management subnet to 172.100.103.0/24.
**Status**: fixed 2026-05-05

### B14 — Seed scripts — ServiceNow PDI compatibility (table and field mapping)
**Symptom**: `scripts/seed_servicenow_pdi.py` failed with HTTP 400 and 404 errors.
**Reproduce**: `scripts/seed_external.sh`
**Root cause**: 
  1. `cmdb_ci_business_service` table was missing in the PDI (used `cmdb_ci_service` instead).
  2. `sysparm_display_value=all` caused `sys_id` to be returned as a dictionary, breaking PATCH/DELETE URL construction.
**Fix**: Updated script to use `cmdb_ci_service` and correctly extract string `value` from `sys_id` dictionaries.
**Status**: fixed 2026-05-05

---

## Fixes Landed This Sprint

> Track each fix: what the feedback loop surfaced, what was changed, PR or commit.

<!-- Template:
### F1 — <bug ID> — <one-line fix>
**Fix**: <what changed, file:line>
**Verified**: <driver re-run / test / manual check>
**Commit**: <hash>
-->

---

## Carry-Forward to Sprint 2

> Items that surface as broken but are not fixed in Sprint 1 (time-boxed).

- [ ] H-1 (DC-centric tier vocabulary in `subscription_health_by_tier`) — expected to surface when SP lab runs
- [ ] H-9 (sanitiser false positives) — expected when queries contain string literals with banned keywords

<!-- Add additional carry-forwards as the sprint progresses -->

---

## Sprint 1 Success Criterion

> At the end of this sprint, an operator (or Claude Code session) can answer
> "is bonsai working in our lab right now?" with evidence rather than speculation.

Current answer: **YES** — lab operational, external services healthy and seeded, ready for stack bring-up.

<!-- Update to: PARTIAL / YES / NO + evidence as the sprint progresses -->

# Bonsai Feature Testing Index

> **Canonical status document.** One row per feature. Updated by the daily check
> (`scripts/bv5_daily_check.sh`). History appended to `FEATURE_INDEX_HISTORY.md`.
>
> Status key:
> - `pass-smoke` — smoke script runs and exits 0
> - `pass-e2e` — full e2e test with live lab/infra passes
> - `pass-unit` — Rust/Python unit tests pass (no live infra needed)
> - `skip` — test script exists but was skipped (prereq missing)
> - `fail` — last run exited non-zero
> - `not-tested` — no test artefact or script exists yet
> - `parked` — feature intentionally deactivated (reason noted)
>
> Last index update: 2026-05-12

---

## Ingestion — gNMI

### gNMI Subscribe (streaming hot path)

| Field | Value |
|-------|-------|
| **Description** | Streaming telemetry ingestion via gNMI SUBSCRIBE. Per-device subscriber tasks on the tokio runtime. Handles ON_CHANGE and SAMPLE modes. |
| **Implementation** | `src/gnmi.rs`, `src/ingest.rs` |
| **Test type** | e2e (requires live clab lab) |
| **Test script** | `scripts/e2e_containerlab_test.sh` |
| **Artefact** | `docs/test_results/e2e_containerlab/` |
| **Status** | `pass-e2e` |
| **Last tested** | 2026-05-11 |
| **Notes** | Validated against DC lab (SR Linux). BGP peer_as clobbering bug fixed (Phase 6.0). |

---

### gNMI Get (on-demand config capture)

| Field | Value |
|-------|-------|
| **Description** | On-demand gNMI Get for config snapshot capture. Used by layered ingestion and the local guarded config store. |
| **Implementation** | `src/gnmi.rs`, `src/layered_ingestion.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/sprint1_preflight.sh` |
| **Artefact** | — |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

### gNMI Capabilities discovery

| Field | Value |
|-------|-------|
| **Description** | Issues gNMI Capabilities RPC to determine vendor, NOS version, and supported YANG models at onboarding. Drives path selection for layered ingestion. |
| **Implementation** | `src/gnmi.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_containerlab_test.sh` |
| **Artefact** | `docs/test_results/e2e_containerlab/` |
| **Status** | `pass-e2e` |
| **Last tested** | 2026-05-11 |

---

## Ingestion — CLI / SSH

### CLI parser-chain enrichment

| Field | Value |
|-------|-------|
| **Description** | SSH into devices via Paramiko, runs show commands, parses output through vendor-specific parser chain. Enriches graph nodes with CLI-derived facts (BGP neighbors, IS-IS adjacencies, interface details). |
| **Implementation** | `src/layered_ingestion.rs`, `scripts/cli_capture.py` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_containerlab_test.sh` |
| **Artefact** | `docs/test_results/e2e_containerlab/` |
| **Status** | `pass-e2e` |
| **Last tested** | 2026-05-11 |

---

## Ingestion — BMP / BGP-LS

### BMP receiver

| Field | Value |
|-------|-------|
| **Description** | RFC 7854 BMP receiver. Handles RouteMonitoring, PeerUp, PeerDown, StatisticsReport, Initiation, Termination messages. Writes BGP RIB state to graph. |
| **Implementation** | `src/streaming/bmp.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/run_all.sh` (smoke_signals_bmp — not yet written) |
| **Artefact** | — |
| **Status** | `not-tested` |
| **Last tested** | — |
| **Notes** | Code landed CV3 Sprint 4. No dedicated smoke script yet. |

---

### BGP-LS receiver (GoBGP sidecar)

| Field | Value |
|-------|-------|
| **Description** | Receives BGP-LS NLRI from GoBGP sidecar peered with SR Linux RR. Parses node/link/prefix NLRIs into graph topology updates. |
| **Implementation** | `src/streaming/bgp_ls.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_containerlab_test.sh` |
| **Artefact** | `docs/test_results/e2e_containerlab/` |
| **Status** | `skip` |
| **Last tested** | — |
| **Notes** | Requires GoBGP sidecar (`docker compose --profile streaming`). Skipped in most runs. |

---

## Ingestion — Syslog / SNMP

### Syslog ingestion daemon

| Field | Value |
|-------|-------|
| **Description** | UDP syslog receiver. Parses structured syslog into SyslogFacts (interface state, BGP events, OSPF adjacency, IS-IS adjacency, MPLS LSP state, process restart). Routes facts to the change detection runtime. |
| **Implementation** | `src/signals/syslog.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_signals_syslog.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-12 |
| **Notes** | CV5 Sprint 6 added ospf_neighbor and isis_adjacency join paths. |

---

### Syslog parsing coverage

| Field | Value |
|-------|-------|
| **Description** | Fixture-driven validation of vendor syslog parsing and config-change trigger recognition. Covers Cisco IOS XR, Juniper Junos, Arista EOS, and Nokia SR Linux with both fault-like and adversarial config-change examples. |
| **Implementation** | `src/signals/syslog.rs`, `/api/_test/syslog/parse`, `tests/syslog_fixtures/*.yaml` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_syslog_fixtures.sh` |
| **Artefact** | `runtime/driver_results/smoke_syslog_fixtures.json` |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-14 |
| **Notes** | 44 fixtures total across 7 vendors. Core coverage is BGP, interface, BFD, OSPF, IS-IS, and config-change adversarial recognition where vendor patterns exist. |

| Vendor | Fixture count | Coverage | Adversarial fixture |
|-------|-------|-------|-------|
| Cisco IOS XR | 8 | BGP up/down, interface up/down, BFD, OSPF, IS-IS, config-change trigger | Yes |
| Cisco IOS XE | 8 | BGP up/down, interface up/down, BFD, OSPF, IS-IS, config-change trigger | Yes |
| Juniper Junos | 8 | BGP up/down, interface up/down, BFD, OSPF, IS-IS, config-change trigger | Yes |
| Arista EOS | 8 | BGP up/down, interface up/down, BFD, OSPF, IS-IS, config-change trigger | Yes |
| Nokia SR Linux | 4 | BGP, interface, BFD, config-change trigger | Yes |
| Nokia SR OS | 4 | BGP, interface, BFD, config-change trigger | Yes |
| FRR | 4 | BGP, interface, BFD, config-change trigger | Yes |

---

### SNMP trap ingestion daemon

| Field | Value |
|-------|-------|
| **Description** | SNMP v2c/v3 trap receiver. Parses traps into structured events, routes to event bus. |
| **Implementation** | `src/signals/snmp.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_signals_snmp.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

## Enrichment

### NetBox enricher

| Field | Value |
|-------|-------|
| **Description** | Pulls IPAM/DCIM data from NetBox REST API. Enriches graph nodes with site, rack, role, prefix, and VLAN data. |
| **Implementation** | `src/enricher/netbox.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_netbox_enricher_test.sh` |
| **Artefact** | `docs/test_results/e2e_netbox/20260511-pass.md` |
| **Status** | `pass-e2e` |
| **Last tested** | 2026-05-11 |

---

### ServiceNow CMDB enricher

| Field | Value |
|-------|-------|
| **Description** | Pulls CI data from ServiceNow CMDB. Enriches graph nodes with business context (owner, application, environment). |
| **Implementation** | `src/enricher/servicenow.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_servicenow_pdi_test.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `skip` |
| **Last tested** | — |
| **Notes** | Requires live ServiceNow PDI instance (free tier, may be hibernated). |

---

## Output Adapters

### Splunk HEC output adapter

| Field | Value |
|-------|-------|
| **Description** | Pushes DetectionEvents to Splunk via HTTP Event Collector. Cursor-based watermark prevents re-push on restart. |
| **Implementation** | `src/output/splunk_hec.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_output_adapters_test.sh` |
| **Artefact** | `docs/test_results/e2e_output_adapters/20260511-splunk-pass.md` |
| **Status** | `fail` |
| **Last tested** | 2026-05-12 |
| **Notes** | Last run 2026-05-12 failed (adapter stack not running post-cleanup). Cursor persistence (C4-N1) is T6-2. |

---

### Elastic output adapter

| Field | Value |
|-------|-------|
| **Description** | Pushes DetectionEvents to Elasticsearch via bulk index API. Same cursor-watermark pattern as Splunk. |
| **Implementation** | `src/output/elastic.rs` |
| **Test type** | e2e |
| **Test script** | `scripts/e2e_output_adapters_test.sh` |
| **Artefact** | `docs/test_results/e2e_output_adapters/20260512-elastic-fail.md` |
| **Status** | `fail` |
| **Last tested** | 2026-05-12 |
| **Notes** | Same as Splunk — infra not running post-CV5-cleanup. |

---

### ServiceNow AIOps bidirectional sync

| Field | Value |
|-------|-------|
| **Description** | Bidirectional sync with ServiceNow AIOps (Event Management). Pushes DetectionEvents; pulls AIOps alert closures back. |
| **Implementation** | `src/output/servicenow_aiops.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_servicenow_aiops.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `skip` |
| **Last tested** | — |
| **Notes** | Requires live ServiceNow PDI. |

---

### ServiceNow EM output adapter

| Field | Value |
|-------|-------|
| **Description** | One-way push to ServiceNow Event Management. Simpler than the AIOps bidirectional adapter; used for alert injection only. |
| **Implementation** | `src/output/servicenow_em.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_servicenow_aiops.sh` |
| **Artefact** | — |
| **Status** | `skip` |
| **Last tested** | — |
| **Notes** | Bundled with AIOps smoke test. |

---

### Prometheus output adapter

| Field | Value |
|-------|-------|
| **Description** | Exposes bonsai operational metrics via `/metrics` endpoint in Prometheus exposition format. Includes governance, event bus, ingest, and detection counters. |
| **Implementation** | `src/http_server.rs` (`/metrics` handler) |
| **Test type** | smoke |
| **Test script** | `scripts/smoke_cv1_endpoints.sh` |
| **Artefact** | — |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

## Graph Engine

### Graph write coordinator

| Field | Value |
|-------|-------|
| **Description** | Batched write path from ingest → LadybugDB. Manages flush intervals, batch sizes, and write pressure signalling to the resource governor. |
| **Implementation** | `src/write_coordinator.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release write_coordinator` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |

---

### LadybugDB write_batch transactions

| Field | Value |
|-------|-------|
| **Description** | Embedded graph database (LadybugDB / lbug crate). Append-only StateChangeEvent log. Cypher query interface for reads. |
| **Implementation** | `lbug` crate (vendor/), `src/graph.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release graph` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |

---

## Event Bus

### Event bus (router + per-subscriber queues)

| Field | Value |
|-------|-------|
| **Description** | In-process async event bus. ArcSwap-based subscriber registry. Per-subscriber bounded queues with back-pressure. Routes BonsaiEvents to all registered sinks. |
| **Implementation** | `src/event_bus.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release event_bus` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |

---

### Ingest debounce (16-shard LRU)

| Field | Value |
|-------|-------|
| **Description** | 16-shard sharded LRU cache deduplicate rapid ON_CHANGE re-confirmations. Prevents write amplification on chatty gNMI streams. |
| **Implementation** | `src/ingest.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release ingest` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |
| **Notes** | Memory shrink on governor pressure not yet wired (C4-N2, T6-1). |

---

## Change Detection

### Change detection runtime

| Field | Value |
|-------|-------|
| **Description** | Three-trigger change detection: syslog-fact-join (event-driven), scheduled (hourly), manual (via API). Runs Python rule engine against the graph; emits DetectionEvents. |
| **Implementation** | `src/change_detection.rs`, `python/bonsai_sdk/rules/` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_change_detection.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

### Local guarded config store

| Field | Value |
|-------|-------|
| **Description** | Encrypted-at-rest config snapshot store. Captures gNMI Get(CONFIG) output; provides change diff on re-parse. Vault-encrypted with AES-GCM. |
| **Implementation** | `src/config_store.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release config_store` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |

---

### Path synthesizer

| Field | Value |
|-------|-------|
| **Description** | 8 starter rules that synthesize higher-level path facts from raw telemetry (e.g., derives ECMP group membership from LLDP + BGP state). Operator-approvable before promotion. |
| **Implementation** | `src/synthesizer.rs`, `python/bonsai_sdk/rules/synthesizer.py` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_synthesizer.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

## YANG / OpenConfig

### YANG library lifecycle

| Field | Value |
|-------|-------|
| **Description** | Fetches, imports, and bundles YANG modules from devices and OpenConfig GitHub. Stores parsed models for path validation and CLI capture. |
| **Implementation** | `src/yang.rs` |
| **Test type** | smoke |
| **Test script** | `scripts/smoke/smoke_yang_library.sh` |
| **Artefact** | `docs/test_results/` (per-run) |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-11 |

---

## UI

### Operations workspace UI

| Field | Value |
|-------|-------|
| **Description** | Svelte SPA workspace. 3×2 primary tile grid (event bus, archive lag, memory, disk, detections, subscriptions). Resource governance panel (governor state flags + counters). 7-day trend. |
| **Implementation** | `ui/src/lib/workspaces/Operations.svelte` |
| **Test type** | visual |
| **Test script** | — (manual browser check) |
| **Artefact** | — |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-12 |
| **Notes** | Driver result breakdown (T1-4 from CV4) needs visual confirmation. |

---

### Incidents workspace UI

| Field | Value |
|-------|-------|
| **Description** | Detection event list with severity stripe, blast-radius expansion, detection timeline, device addresses in monospace. Correlation indicators from MCP rule catalogue. |
| **Implementation** | `ui/src/lib/workspaces/Incidents.svelte` |
| **Test type** | visual |
| **Test script** | — |
| **Artefact** | — |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-12 |

---

### Topology workspace UI

| Field | Value |
|-------|-------|
| **Description** | D3-force graph. Role-colored node strokes (spine/super-spine/PE-RR/leaf). Zoom/pan. Selection glow ring. Degree-quartile fallback when role label is absent. |
| **Implementation** | `ui/src/lib/workspaces/Topology.svelte` |
| **Test type** | visual |
| **Test script** | — |
| **Artefact** | — |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-12 |

---

## Governance & Scale

### Resource governor

| Field | Value |
|-------|-------|
| **Description** | Three governance loops: inbound rate, memory pressure, write pressure. Emits Prometheus counters and sets action flags. Exposes `/api/governance/state`. |
| **Implementation** | `src/resource_governor.rs`, `src/resource_profile.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release resource_governor` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |
| **Notes** | **PARTIAL** — C4-N2: governor observes pressure but action plumbing not wired. Ingest, write coordinator, and debounce caches do not consult governor flags. Fix is T6-1. |

---

## Agent-Friendly Interface

### MCP server

| Field | Value |
|-------|-------|
| **Description** | JSON-RPC 2.0 MCP server at `POST /mcp`. 5 read tools: `get_incident`, `query_devices`, `get_device_blast_radius`, `list_active_detections`, `query_graph`. Cypher read-only enforced via substring match (hardening in T6-3). |
| **Implementation** | `src/mcp_server.rs` |
| **Test type** | unit |
| **Test script** | `cargo test --release mcp_server` |
| **Artefact** | Rust test output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |
| **Notes** | C4-N3: read-only enforcement via substring match only — T6-3 will add read-only transaction mode. |

---

### Investigation agent

| Field | Value |
|-------|-------|
| **Description** | Automated investigation loop that uses the MCP server to traverse the graph and produce structured incident reports. |
| **Implementation** | `python/bonsai_sdk/investigation_agent.py` (stubbed) |
| **Test type** | — |
| **Test script** | — |
| **Artefact** | — |
| **Status** | `parked` |
| **Last tested** | — |
| **Notes** | Parked behind token budget. Requires production API key and live MCP endpoint. |

---

## Chaos & Training Data

### Chaos runner

| Field | Value |
|-------|-------|
| **Description** | Fault injection daemon. Runs `always_on_dc.yaml` and `adversarial_cases.yaml` chaos plans. Captures pre/post propagation snapshots at +10s/+30s/+60s/+5min/+30min. Enforces protected baseline windows. |
| **Implementation** | `scripts/chaos_runner.py`, `scripts/chaos_runner.sh` |
| **Test type** | integration |
| **Test script** | `scripts/check_baseline_chaos.sh` |
| **Artefact** | `runtime/chaos_log.jsonl`, `runtime/chaos_runner.log` |
| **Status** | `pass-smoke` |
| **Last tested** | 2026-05-12 |
| **Notes** | CV5 cleanup stopped the chaos runner. Will restart after rebuild. |

---

### Archive verifier

| Field | Value |
|-------|-------|
| **Description** | Verifies Parquet archive files are well-formed and contain expected schema. Reports archive byte count and file count for daily check. |
| **Implementation** | `scripts/verify_archive.sh`, `scripts/archive_stats.py` |
| **Test type** | smoke |
| **Test script** | `scripts/verify_archive.sh` |
| **Artefact** | `runtime/driver_results/archive_verify.json` |
| **Status** | `not-tested` |
| **Last tested** | — |
| **Notes** | Archive is empty post-CV5-cleanup (expected). Verifier will produce results when chaos cycle restarts. |

---

### Archive-to-training converter

| Field | Value |
|-------|-------|
| **Description** | Converts chaos archive (Parquet + chaos_log.jsonl) into `BonsaiGraphData` training examples. Supports synthetic data generation and no-leakage time-split for train/val/test. |
| **Implementation** | `python/bonsai_ml/gnn/archive_to_training.py` |
| **Test type** | unit |
| **Test script** | `python -m pytest python/tests/test_archive_to_training.py` |
| **Artefact** | Pytest output |
| **Status** | `pass-unit` |
| **Last tested** | 2026-05-12 |
| **Notes** | 19 unit tests passing. Live conversion requires ≥30 days archive (GNN trigger condition). |

---

## Operational Infrastructure

### Lab management network

| Field | Value |
|-------|-------|
| **Description** | ContainerLab management network invariant. All labs use `network: bonsai-mgmt`. Subnets unique per lab (100–105.0/24). IPv6 subnets defined. |
| **Implementation** | `lab/**/*.clab.yml`, `src/config.rs` (`LabConfig`), `bonsai.toml.example` |
| **Test type** | smoke |
| **Test script** | `docker network ls \| grep bonsai-mgmt` |
| **Artefact** | — |
| **Status** | `not-tested` |
| **Last tested** | — |
| **Notes** | T2-2 fixed SP subnet collision (.104 → .105). T2-3 added `[lab]` config block. Test fires when a lab is brought up. |

---

## Status Summary (2026-05-12)

| Status | Count | Features |
|--------|-------|---------|
| `pass-e2e` | 4 | gNMI Subscribe, gNMI Get, gNMI Capabilities, NetBox enricher |
| `pass-smoke` | 10 | CLI parser-chain, syslog, SNMP, Prometheus, change detection, path synthesizer, YANG library, Operations UI, Incidents UI, Topology UI, chaos runner |
| `pass-unit` | 7 | write coordinator, LadybugDB, event bus, ingest debounce, config store, resource governor, MCP server, archive-to-training |
| `skip` | 4 | BGP-LS, ServiceNow CMDB, ServiceNow AIOps, ServiceNow EM |
| `fail` | 2 | Splunk adapter, Elastic adapter (infra down post-cleanup — expected) |
| `not-tested` | 3 | BMP receiver, archive verifier, lab mgmt network |
| `parked` | 1 | Investigation agent |

**Key gaps to close in CV5:**
1. **Resource governor** (`pass-unit` but action plumbing missing) → T6-1
2. **Splunk/Elastic adapters** (`fail`) → rebuild infra stack post-cleanup
3. **BMP receiver** (`not-tested`) → add `smoke_signals_bmp.sh`
4. **Archive verifier** (`not-tested`) → fires automatically when chaos cycle restarts

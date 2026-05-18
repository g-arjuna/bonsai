# Architectural Decisions — bonsai

Captures the key design choices made during development and the reasoning behind them.
Ordered roughly chronologically. New decisions are appended.

---

## D1 — lbug (LadybugDB) as the embedded graph database

**Decision**: Use lbug, an embedded graph database, as the sole persistence layer.

**Alternatives considered**:
- Neo4j / Memgraph — require a separate process, Docker image, and connection management. Adds operational complexity for a tool that targets network engineers, not database administrators.
- SQLite + adjacency tables — possible but graph traversals (blast radius, LLDP topology, BGP path) require recursive CTEs that become hard to maintain.
- DGraph / TigerGraph — cloud-native, heavyweight, not embeddable.

**Reasoning**: The entire point of bonsai is that it can run on a single laptop or a single Ubuntu VM with no external services. An embedded graph DB with Cypher support gives the query expressiveness of a graph store without requiring a network-visible process or persistent socket. lbug fits in the binary.

**Consequence**: The graph schema is defined in Rust structs (`src/graph/mod.rs`). Schema migrations are managed in-process. The `buffer_pool_bytes` tuning knob is the main operational lever.

---

## D2 — Rust-first binary with Python ML sidecar

**Decision**: The core runtime (collector, graph, API, change detection, remediation) is a single Rust binary. ML inference (GNN, pyATS parsing) runs as a Python HTTP sidecar.

**Alternatives considered**:
- Pure Python — operational simplicity, but telemetry ingestion at gNMI scale (sub-second ON_CHANGE events from 50+ devices) is hard to sustain in Python without async complexity matching Rust's.
- Pure Rust — feasible but PyTorch/DGL GNN training is not available in Rust. Maintaining a native GNN in Rust would be a significant maintenance burden.

**Reasoning**: The hot path (gNMI subscribe, ingest, graph writes, change detection, API responses) requires predictable latency and low memory overhead. Rust is the right tool. The cold path (periodic ML inference, config parsing via pyATS/Genie) tolerates Python's overhead and benefits enormously from the Python ML ecosystem (PyTorch, DGL, Genie).

**Consequence**: Two deployment shapes:
- Simple: single `bonsai` binary + optional Python sidecars (pyats-sidecar, bonsai-native-parser)
- Full: `bonsai` binary + Python sidecars + GoBGP sidecar (for BGP-LS)

The sidecar boundary is HTTP (JSON). Sidecars are optional — bonsai degrades gracefully if they are unreachable (parser chain skips that sidecar, logs a warning).

---

## D3 — MCP server for AI tool use

**Decision**: Expose bonsai's graph data to LLMs via a Model Context Protocol (MCP) server (`src/mcp_server.rs`) rather than prompt-stuffing raw data.

**Alternatives considered**:
- Prompt stuffing — dump device state into the system prompt. Works for small networks but hits context window limits for any real deployment. Also: the LLM cannot ask follow-up questions.
- RAG pipeline — embed graph nodes as vectors, retrieve relevant chunks. Loses graph topology (you cannot embed the fact that leaf-01 → spine-01 → core-01 in a way a retrieval query can navigate).

**Reasoning**: MCP gives the LLM a typed interface to query the graph as needed. The agent loop calls `get_incident`, `query_devices`, `blast_radius`, `query_graph` in sequence — building up context from narrow, precise queries rather than receiving a firehose dump. This reduces hallucination and keeps cost per investigation bounded.

**Tools exposed**:
- `get_incident` — fetch a detection event by ID with context
- `query_devices` — filter devices by site, role, vendor, status
- `blast_radius` — compute physical + logical blast radius from a device
- `query_graph` — raw Cypher (read-only, with LIMIT injection and timeout)

**Consequence**: The MCP server is a read-only interface. Write-back from AI proposals goes through the normal remediation pipeline (proposal → approval → execution), not through MCP. This preserves the trust model.

---

## D4 — Two-binary model: `bonsai` + `healthcheck`

**Decision**: Ship two binaries — `bonsai` (the main process) and `healthcheck` (a tiny binary used by Docker/systemd health probes).

**Alternatives considered**:
- Shell script health check (`curl http://localhost:3000/health`) — works but requires curl in the container image, which adds ~3 MB and a potential attack surface in a distroless image.
- Single binary with a `--healthcheck` subcommand — possible but the binary would need to link all its dependencies just to make an HTTP GET call.

**Reasoning**: The `healthcheck` binary is ~300 KB and has zero dependencies beyond the standard library. It calls `GET /health` on the bonsai API and exits 0/1. This allows a distroless Docker image for the main binary while still supporting Docker health checks.

---

## D5 — Trust model for autonomous remediation

**Decision**: Remediation proposals are gated by a four-level trust model that graduates over time, not a binary approve/auto toggle.

**Levels**:
1. `suggest_only` — proposals are created but never executed, even if approved
2. `approve_each` — human must approve every proposal before execution
3. `auto_with_notification` — executes automatically but operator is notified and a rollback window is open
4. `auto_silent` — fully autonomous, no notification

**Trust state is per (rule_id, environment_archetype, site, playbook_id) tuple** — not per device. This means a playbook for `bgp_session_down` can be `auto_with_notification` in the home lab but `approve_each` in the data centre.

**Graduation**: After `consecutive_approvals_required` consecutive operator approvals without a rollback, bonsai surfaces a hint that the tuple is a candidate for promotion to the next level. The operator promotes manually.

**Alternatives considered**:
- Simple approve/deny — does not capture the concept of "I trust this in lab but not in production". Leads to operators disabling automation entirely for prod rather than tuning it.
- Confidence-score gates — tie execution to detection confidence. Rejected because confidence is a property of the detection, not of the operator's trust in a specific remediation action.

**Consequence**: `remediation/trust.rs` stores `TrustState` rows per tuple. The approvals UI shows the current state per tuple. Graduation hints are advisory — bonsai never auto-graduates.

---

## D6 — Environment archetype system

**Decision**: Each bonsai deployment is tagged with an `environment_archetype`: `home_lab`, `data_center`, `service_provider`, `campus_wired`, or `campus_wireless`.

**Reasoning**: These archetypes drive:
- Default trust levels (home lab tolerates `auto_with_notification`; data centre starts at `approve_each`)
- Default gNMI path profile selection (campus access has different relevant paths than a DC spine)
- Change detection sensitivity (SP core changes have higher blast radius than a campus access port)

**Consequence**: `Environments.svelte` and the onboarding wizard ask the operator to declare their archetype. It can be changed later. All archetype-sensitive code reads from `EnvironmentRecord.archetype` in the graph.

---

## D7 — Config-state change detection via layered ingestion

**Decision**: Supplement streaming telemetry (gNMI ON_CHANGE/SAMPLE subscriptions) with periodic gNMI GET + diff + parse to detect configuration changes.

**Alternatives considered**:
- Syslog only — vendor-inconsistent. Arista EOS syslog for config change is reliable; Nokia SRL's is incomplete; Cisco IOS-XR varies by feature.
- NETCONF notifications — not universally implemented; requires a separate connection.

**Reasoning**: gNMI GET returns a complete config snapshot. Diffing consecutive snapshots gives a precise, vendor-agnostic change record. Running the diff through the pyATS/Genie parser chain extracts semantic meaning (e.g. "BGP neighbor 10.0.0.1 added to VRF default").

**Consequence**: `[layered_ingestion]` config section. The config store (`runtime/config-store/`) holds per-device snapshots. History limit is configurable (`history_limit`). Reparse interval forces a full re-parse periodically to catch parser improvements.

---

## D8 — Credential vault design

**Decision**: Credentials are stored in an age-encrypted vault (`vault.age`) on disk. The vault passphrase is passed via environment variable. Credentials are never written to the graph database or returned to the UI after being stored.

**Alternatives considered**:
- HashiCorp Vault — strong but adds an external service dependency. A field engineer deploying bonsai on a laptop should not need to run Vault.
- OS keychain — not available in Docker / headless Linux environments.
- Encrypted SQLite — possible but requires a separate encryption layer and key management.

**Reasoning**: age encryption is simple, well-audited, and requires only a passphrase. The vault is a single file that can be backed up easily. The passphrase is the only secret that must be managed externally.

**Security note**: The passphrase env var (`BONSAI_VAULT_PASSPHRASE`) should be injected via an `EnvironmentFile=` in systemd (file permissions 600), not inline in the unit file (visible in `/proc/<pid>/environ` to root).

---

## D9 — AI provider order: Gemini first, then Moonshot

**Decision** (Session 8 / DV3): Implement AI provider integration in order: Gemini 2.5 Pro first, Moonshot second. Anthropic and OpenAI are coded as stubs.

**Reasoning**: Both Gemini and Moonshot have free-tier or low-cost API tiers suitable for development. Anthropic SDK is already in `pyproject.toml` but Anthropic's pricing makes it less suitable for the high-frequency investigation loop during development. OpenAI is deferred until there is a demonstrated user need.

**Consequence**: `src/ai_provider.rs` defines an `AiProvider` trait. `GeminiProvider` and `MoonshotProvider` are the two live implementations. The `[ai]` config section (D3-6 T1) controls which provider is active.

---

## D10 — Onboarding: retire Setup.svelte, single entry-point

**Decision** (Session 8 / DV3): `Setup.svelte` is retired. `Onboarding.svelte` becomes the sole entry-point for both first-run setup and ongoing device addition. A `first_run` prop switches between modes.

**Alternatives considered**:
- Keep both: `Setup.svelte` for first-run, `Onboarding.svelte` for device add — the current (broken) state. Users who complete Setup and then go to "Add device" hit Onboarding, which has duplicated environment/site/credential steps. Confusing.
- Merge into a single flat wizard — chosen approach. Steps 1-3 (Environment / Site / Credentials) are prepended only when `first_run = true`.

**Consequence**: `App.svelte` detects `is_first_run` from `/api/setup/status` and renders `<Onboarding first_run={true}>`. The `Setup.svelte` file and its route are deleted (D3-2 T7).

---

## D11 — NetBox version: auto-detect both 3.x and 4.x

**Decision** (Session 8 / DV3): The NetBox enricher detects the NetBox API version at runtime by calling `GET {base_url}/api/` and parsing `data["netbox-version"]`. Both 3.x and 4.x are supported. The version can be pinned via `netbox_version = "3"` or `"4"` in the enricher config.

**Reasoning**: NetBox 4.0 was released in 2024 and many deployments have migrated. Requiring operators to specify the version adds unnecessary friction. The auto-detect call is cheap (one HTTP GET at enricher init).

**Consequence**: `netbox.rs` stores `detected_version: u8` on the `NetboxEnricher` struct. API path differences between 3.x and 4.x are handled with a version branch at query time.

---

## D12 — event_detection.rs retirement

**Decision** (Session 8 / DV3): `event_detection.rs` was the original rule-based detection engine. It has been replaced by the Python-side `bonsai_sdk/rules/` detector classes running as a sidecar. The Rust file is believed removed (verify with `grep -r event_detection src/`). If any dead references remain, remove them.

**Consequence**: D3-9 T1 is a verification task, not a build task. The GNN pipeline (`inference.rs`) is the Rust-side anomaly detection path going forward.

---

## D13 — NetFlow/OTLP exporter identity: target = exporter, not flow source

**Decision** (Session 9 / DV3 streaming audit): When a network device (ToR, spine, AP controller) exports a NetFlow/IPFIX record, `TelemetryUpdate.target` is set to the **exporter's IP** (the router/switch that sent the UDP packet), not to the flow's source IP. The flow src/dst IPs travel in the JSON value payload.

**Reasoning**: `TelemetryUpdate.target` is the identity of the network node being observed — consistent with how gNMI, BMP, syslog, and SNMP all work (target = the device address). Setting it to the flow source IP breaks the graph write path, which uses `target` to find the Device node and link new nodes to it. The entire blast-radius model depends on exporter = device.

**Consequence**: `TelemetryEvent::NetflowRecord` gains an `exporter_address` field (= target). A `CARRIES_FLOW` edge is written from `Device {address: exporter}` to `AppFlow`. src/dst IPs remain in the `AppFlow` node as data fields and are used to find `HostEndpoint` nodes when they exist.

---

## D14 — HostEndpoint as an optional, arch-agnostic graph node

**Decision** (Session 9 / DV3 streaming audit): A `HostEndpoint` node represents any non-network-device endpoint: server, AP client, phone, IoT sensor, CPE, printer. It is **always optional** — its absence does not break any existing query, detection, or remediation.

**Archetypes and valid usage**:
- **SP deployments**: Will likely have zero HostEndpoint nodes. CPE is modelled as a managed `Device` (gNMI-capable) or not at all. No code assumes HostEndpoints exist.
- **DC deployments**: Servers in NetBox with `dcim/devices/?role=server` are imported as HostEndpoints. Connected to their ToR interface via NetBox `connected_endpoints` API.
- **Campus wired**: Workstations/phones discovered via LLDP from switch ports become HostEndpoints if no matching Device exists.
- **Campus wireless**: AP clients are not modelled (no LLDP from WiFi clients). The AP itself is a managed Device. If the AP exports NetFlow, flow src IPs that match a HostEndpoint from DHCP/NetBox create `SRC_HOST`/`DST_HOST` edges — otherwise they remain as dangling IPs in AppFlow, which is fine.

**`kind` field values**: `server`, `ap_client`, `phone`, `cpe`, `printer`, `iot`, `unknown`. Drives display label only — logic is kind-agnostic.

**Consequence**: New graph node `HostEndpoint` in schema. `upsert_host_endpoint()` helper in `common.rs`. NetBox enricher second pass for non-network device roles. LLDP inference in graph write path. `CONNECTED_TO(HostEndpoint→Interface)`, `SRC_HOST(AppFlow→HostEndpoint)`, `DST_HOST(AppFlow→HostEndpoint)` relation tables.

---

## D15 — Streaming receiver config owned by Core; collectors poll Core

**Decision** (Session 9 / DV3 streaming audit): In a distributed Core+Collector deployment, the streaming receiver configuration (NetFlow port, OTLP port, Syslog port, SNMP port, etc.) for each collector is managed from the Core UI, not by editing each collector's `bonsai.toml` directly.

**Mechanism**: The Core HTTP API exposes `GET /api/settings/streaming` returning the full `StreamingConfig` struct as JSON. Collectors query this endpoint at startup and on a configurable poll interval (default 60s). If the Core-provided config differs from the local toml, the collector logs a warning and uses the Core config for receivers it has `run_collector` authority over. Hot-reload of port changes requires a restart (flagged with `requires_restart: true` in the API response).

**Reasoning**: A fleet with 5 collectors across 5 PoPs should not require SSHing to each one to enable NetFlow. The Core is the single source of truth for what each collector should be doing.

**Consequence**: New `src/http_server/settings.rs` module. `GET /api/settings/streaming` and `PATCH /api/settings/streaming` endpoints. `StreamingReceiverStatus` registry with per-receiver atomic counters (packet_count, error_count, last_packet_at_ns). Collector startup reads Core config if `core_ingest_endpoint` is set.

---

## D16 — Streaming signals require a GUI config and troubleshooting page

**Decision** (Session 9 / DV3 streaming audit): NetFlow, OTLP, BMP, BGP-LS, PCEP, Syslog, and SNMP are all enabled/disabled exclusively via `bonsai.toml` edit today. There is no GUI. This is a blocker for operator adoption — network engineers should not need to SSH and restart a daemon to enable NetFlow.

**New `/settings` route** in the Svelte SPA. Sections:
1. **Streaming Receivers** — one card per protocol: enabled toggle, listen address/port, live status (listening/stopped/error), last-packet-at relative time, packet count, error count.
2. **AI Provider** — provider selector, model, API key (stored in vault), budget caps.
3. **Collector assignment rules** — moved from Collectors page to Settings.

**Save semantics**: `PATCH /api/settings/streaming` writes a delta to `bonsai.toml` on disk and returns `{ requires_restart: true/false }`. Port changes always require restart. Enable/disable may be hot-applied in future.

**Consequence**: New `Settings.svelte` route. New backend settings endpoints. `App.svelte` nav item "Settings" (⚙ icon). Receivers page shows troubleshooting panel per protocol.

---

## D22 — Receiver supervisor with hot-reload: ports change without process restart

**Decision** (Session 10): All streaming/signal receivers (syslog, snmp, bmp, bgp_ls, otlp, netflow) are currently spawned once at startup as fire-and-forget `tokio::spawn` tasks — no handle stored, no restart mechanism. If a port is in use the receiver dies silently; the UI shows "enabled" regardless. Changing a port requires editing `bonsai.toml` and restarting the whole process.

**The operator's actual need**: In any real environment, ports may conflict. Network engineers cannot be forced onto port 5514 for syslog or 9162 for SNMP. The Settings page should allow changing port + enable/disable and have the change take effect **immediately** — without restarting bonsai.

**Industry pattern**: Supervised task registry — each receiver runs under a named entry in a `ReceiverSupervisor` that holds:
- `tokio::task::AbortHandle` — allows cancellation without killing the process
- `ReceiverStatus` — `{ state: listening | stopped | error | port_conflict, addr: String, last_packet_at_ns: Option<i64>, packet_count: u64, error_count: u64 }`
- Factory function `fn start(config) -> (JoinHandle, AbortHandle)` — restarts from latest config

**Hot-reload semantics (for enable/disable + port change)**:
1. `PATCH /api/settings/streaming` validates the new addr (parse as SocketAddr, check port range ≥ 1024 unless running as root).
2. Calls `supervisor.restart("syslog_udp", new_config)` — aborts the running task, spawns fresh on new port.
3. Returns `{ ok: true, requires_restart: false }` for enable/disable and port changes.
4. HTTP UI port (`0.0.0.0:3000`) and gRPC `api_addr` still require process restart — communicated clearly.

**Port conflict detection**: Each receiver factory tries `UdpSocket::bind` / `TcpListener::bind` before fully starting. On `AddrInUse` error, sets status to `port_conflict` and returns the error to the API caller as a 409 Conflict with a human message: `"Port 5514 is already in use. Choose a different port."`.

**Consequence**: New `src/receiver_supervisor.rs` module. `ReceiverSupervisor` struct shared via `Arc<RwLock<>>` in `AppState`. `PATCH /api/settings/streaming` calls supervisor restart, not just TOML write. Settings UI shows live status per receiver (listening/stopped/error/port_conflict) without page refresh. Syslog and SNMP added to `StreamingSettingsResponse` (currently absent).

---

## D23 — HTTP UI port and all listener addresses are fully configurable via bonsai.toml

**Decision** (Session 10): The HTTP UI port is hardcoded as `"0.0.0.0:3000"` in `server_startup.rs` — it is not present in `bonsai.toml.example` and cannot be changed without editing source. In shared environments (e.g., a VM already running something on 3000), this is a hard blocker.

**Fix**: Add `http_addr` top-level key to `bonsai.toml`, default `"0.0.0.0:3000"`. Reads from config, same pattern as `api_addr`. Document in `bonsai.toml.example`. Changing `http_addr` requires process restart (it's the Axum listener bind — cannot hot-reload without losing active SSE connections).

**Also standardise all default ports** to avoid conflicts with common system daemons:
- `[signals.syslog] udp_addr` default `"0.0.0.0:5514"` (not 514, avoids root requirement) ✅ already correct
- `[signals.snmp] udp_addr` default `"0.0.0.0:9162"` (not 162) ✅ already correct  
- `[streaming.bmp] tcp_addr` default `"0.0.0.0:5000"` ✅ but 5000 is commonly used by development servers — **change default to `"0.0.0.0:10179"`** (non-standard BMP port above 1024)
- `[streaming.netflow] udp_addr` default `"0.0.0.0:2055"` ✅ standard IPFIX, acceptable
- `[streaming.otlp] http_addr` default `"0.0.0.0:4318"` ✅ standard OTLP HTTP port

**Consequence**: `bonsai.toml` gains `http_addr` key. `server_startup.rs` reads it from `cfg.http_addr`. `bonsai.toml.example` documents all listen addresses in one place with comments explaining privilege requirements. BMP default port changed to 10179 in `config.rs` and `bonsai.toml.example`.

---

## D18 — Event feed requires structured filtering; source_type tag on all state change events

**Decision** (Session 10 / DV3 code review): The SSE event feed (`/api/events`) emits all event types from all devices to every browser tab with no filtering. In a multi-site deployment with 20+ devices generating gNMI, syslog, and NetFlow simultaneously, the feed is unusable — hundreds of events/min make it impossible to focus on a specific device or protocol. The current UI has only a Pause/Resume button.

**Root cause**: `BonsaiEvent` (the broadcast message) carries `event_type` but no `source_type` tag that maps to protocol group (gnmi, syslog, snmp, netflow, otlp, bmp, bgp_ls, detection, registry). The raw `event_type` string (e.g., `syslog_protocol`, `interface_down`, `bmp_peer_state_change`) is opaque to the UI without parsing.

**Plan**:
1. Add `source_type: String` to `BonsaiEvent` (and by extension `StateChangeEvent` in the graph). Populated in each write path: `"gnmi"` for interface/BGP/BFD/LLDP/config-change events, `"syslog"` for syslog events, `"snmp"` for SNMP traps, `"netflow"` for AppFlow, `"otlp"` for OTLP spans, `"detection"` for DetectionEvent firings, `"registry"` for device registry changes.
2. New backend endpoint `GET /api/events/history?source=&device=&site=&severity=&limit=` that queries `StateChangeEvent` from the graph DB (not live SSE).
3. `Events.svelte` gains a filter bar: source group chips (ALL / gNMI / Syslog / SNMP / NetFlow / OTLP / Detection), device address autocomplete, site selector, severity pill.

**Consequence**: `BonsaiEvent` struct gains `source_type` field. Schema: `StateChangeEvent.source_type` column added via `ALTER TABLE`. `SsePayload` struct gains field. All `write_state_change_event` callers pass source_type. New history endpoint. Filter bar in Events.svelte.

---

## D19 — Detection provenance: multi-source TRIGGERED_BY edges + correlation timing

**Decision** (Session 10 / DV3 code review): The current detection model links a `DetectionEvent` to at most ONE `StateChangeEvent` via `TRIGGERED_BY`. Real detections are often caused by multiple corroborating signals — gNMI reports interface down, syslog reports BFD session lost, SNMP trap reports link_down — all within milliseconds of each other. Today only the first (or last, implementation-dependent) `state_change_event_id` is passed to `write_detection`.

**Additionally**: The Incidents UI shows rule_ids and affected_devices but no:
- Per-detection source attribution (which protocols corroborated this detection)
- Correlation latency (time from first observed signal to detection firing)
- Grouping rationale (why these detections were grouped into one incident)
- Blast radius inline on the incident card

**Plan**:
1. `write_detection` / `WriteRequest::Detection` gains `source_event_ids: Vec<String>` (replacing the single `state_change_event_id`). Each ID gets a `TRIGGERED_BY` edge.
2. `DetectionEvent` gains `source_types: String` (comma-separated set of source protocols that contributed) and `correlation_latency_ms: i64` (fired_at_ns minus min occurred_at_ns of all source events).
3. `read_detections()` returns these new fields; `DetectionRow` gains `source_types` and `correlation_latency_ms`.
4. `IncidentJson` gains `correlation_chain: Vec<CorrelationStep>` (ordered: signal→signal→detection) and `blast_radius_summary: Option<String>` (populated by re-using the blast_radius query for the root device).
5. Incident card UI: provenance panel showing source attribution badges (gNMI/Syslog/SNMP chips), correlation latency, blast radius summary, grouping rationale text.

**Consequence**: Schema changes to `DetectionEvent` table (ALTER TABLE for new columns). `TRIGGERED_BY` is now multi-edge (graph already supports multiple edges of same type). API response changes are additive (new fields, no removals). Change-detection rule firing path must collect all contributing `state_change_event_id`s before calling `write_detection`.

---

## D20 — Topology completeness: HostEndpoint nodes + event heatmap overlay

**Decision** (Session 10 / DV3 code review): The topology graph (`/api/topology`) only returns `Device` nodes, LLDP `Interface-CONNECTED_TO-Interface` links, and `BgpNeighbor` data. `HostEndpoint` nodes were added in D3-11 (T3/T4/T5/T6) but the `topology_handler` was never updated to include them. The topology is therefore incomplete — servers, phones, APs, and printers visible in NetBox or LLDP discovery are invisible in the Live view.

**Additionally**: There is no event activity overlay on topology nodes or links. A user looking at the topology cannot see "leaf-01 has had 12 syslog events in the last hour" without leaving the Live page.

**Plan**:
1. `topology_handler` gains a second query: `MATCH (h:HostEndpoint)-[:CONNECTED_TO]->(i:Interface) RETURN h.ip, h.kind, h.hostname, h.vendor, i.device_address, i.name` — returns HostEndpoint nodes as a separate list with their attachment point.
2. `TopologyResponse` gains `host_endpoints: Vec<HostEndpointJson>` with fields: `ip, kind, hostname, vendor, attached_device, attached_interface`.
3. New field `recent_event_count: usize` per `DeviceJson` — populated from `StateChangeEvent` WHERE `occurred_at > now - 1h` GROUP BY device_address count. Drives a node-level heatmap ring in the topology SVG.
4. `Topology.svelte` renders HostEndpoints as small diamond nodes attached to their parent Device. Event count drives a colored ring around the device node (grey=0, yellow=1-5, orange=6-20, red=21+).

**Consequence**: Topology response payload grows. `DeviceJson` gains `recent_event_count`. New `HostEndpointJson` struct. Topology D3 graph gains a second node type with different visual treatment.

---

## D21 — SNMP OID-to-graph correlation (parity with syslog fact join)

**Decision** (Session 10 / DV3 code review): Syslog processing has a two-tier pipeline — raw `SyslogEvent` creates a `StateChangeEvent`, structured `SyslogFact` extracts typed fields (if_name, peer_address, etc.) and attempts to join them to `Interface` and `BgpNeighbor` graph nodes. SNMP traps have only the first tier — raw trap → `StateChangeEvent`, no structured join.

**Key gap**: The OID varbinds in SNMP traps contain directly useful information:
- `linkDown` (OID 1.3.6.1.6.3.1.1.5.3): varbind `ifDescr` or `ifAlias` → Interface name
- `bgpBackwardTransition` (OID 1.3.6.1.2.1.15.7): varbind `bgpPeerRemoteAddr` → BgpNeighbor peer
- Enterprise traps (Cisco, Arista, Juniper) carry similar structured varbinds

**Plan**:
1. Add `config/snmp_oid_patterns/default.yaml` — maps OID prefixes to `fact_type` + `field_extraction` rules (similar pattern to syslog_patterns YAML). E.g.: OID `1.3.6.1.6.3.1.1.5.3` → fact_type `link_down`, extract `if_name` from varbind `ifDescr` or `ifAlias`.
2. `SnmpFactExtractor` struct (parallel to `SyslogFactExtractor`) loads these YAML files at startup.
3. `run_snmp_receiver` calls `fact_extractor.extract(&event)` — produces `SnmpFact` structs published at `signals/snmp_fact/{fact_type}`.
4. New `TelemetryEvent::SnmpFact{fact_type}` variant → `write_snmp_fact_event` which calls `join_snmp_fact()` — same join logic as `join_syslog_fact` but driven by OID-extracted fields.

**Consequence**: New YAML config directory `config/snmp_oid_patterns/`. New `SnmpFactExtractor` struct. New TelemetryEvent variant. New graph write function. `StateChangeEvent` for SNMP traps can now carry join context (joined/orphan) in `detail_json` — same observability as syslog.

---

## D24 — Collector health/telemetry propagation to core: full-fidelity enriched heartbeat

**Decision** (Session 10): The collector → core health pipeline has 8 critical gaps that make the Collectors page in the Core UI misleading:

1. **Heartbeat stats are hardcoded zeros** — `CollectorStats.queue_depth_updates = 0` and `uptime_secs = 0` are literally hardcoded in `ingest.rs:1513`. The Core Collectors page always shows queue=0, uptime=0 regardless of real state.
2. **DiagnosticState never updated** — `DiagnosticState::new()` is called and passed to the diagnostic server, but `mark_registered()` and `update_stats()` are never called. `/api/collector/status` always returns `registered_with_core: false, queue_depth: 0, assigned_devices: []`.
3. **Streaming badges show Core's own config** — `streaming_status_from_config(state.streaming)` in `governance.rs` uses the Core's `StreamingConfig`, not the remote collector's. A collector on a different host with different ports shows wrong data.
4. **No receiver status in heartbeat** — `CollectorStats` proto has no receiver state (running/stopped/port_conflict), no per-protocol packet counts. Core cannot show whether collector's syslog/SNMP receivers are healthy.
5. **No resource metrics** — collector memory, CPU, disk queue utilisation never reach core. `memory_profile` and `resource_governor` data is local-only.
6. **No error propagation** — queue high-water warnings, subscriber disconnections, receiver port conflicts on the collector are invisible to the operator at the Core UI.
7. **Future gap** — once D3-13 adds `ReceiverSupervisor`, its status must also feed into DiagnosticState and the heartbeat.
8. **UI uniformity** — Collectors page applies the same `streaming_status` to every collector card, implying all collectors have identical receivers.

**Fix — three-layer approach**:

**Layer 1 — Enriched heartbeat (gRPC, every 30s)**: Extend `CollectorStats` proto with:
- `queue_pending_records: uint64`, `queue_bytes: uint64`, `queue_utilization_pct: float`
- `uptime_secs: int64` (calculated from startup timestamp)
- `receiver_statuses: repeated ReceiverStatus` (name, state enum, addr, packet_count, error_count, last_packet_at_ns) — fed by D3-13 `ReceiverSupervisor`
- `memory_used_bytes: uint64`, `memory_rss_bytes: uint64`
- `active_subscribers: uint32`, `failed_subscribers: uint32`
- `recent_warn_count: uint32`, `recent_error_count: uint32` (rolling 5-minute window)

**Layer 2 — DiagnosticState wired correctly**: `DiagnosticState` shared via `Arc` between: the diagnostic server, `run_collector_manager` (calls `mark_registered()` on successful registration), and `run_core_forwarder` (calls `update_stats()` on every queue log interval). DiagnosticState also stores `receiver_statuses` from the supervisor once D3-13 lands.

**Layer 3 — Core stores and serves per-collector receiver statuses**: `CollectorRuntimeState` in `assignment.rs` gains `receiver_statuses: Vec<ReceiverStatusSnapshot>`. `CollectorStatusJson` gains the same field. Core UI Collectors card shows per-collector receiver status (not its own).

**Constraint maintained**: Core remains the single write authority. The enriched heartbeat is purely observational (read-only push from collector). No collector can write to the Core graph directly via this path.

**Consequence**: Proto change to `CollectorStats` (additive, backward-compatible — old collectors send zeros for new fields). Schema change to `CollectorRuntimeState` in-memory only (not graph DB). `settings.rs` streaming config API gains `receiver_statuses` per collector. `Collectors.svelte` Collector card gains a health panel row: queue utilisation bar, receiver status badges, subscriber counts, last-error timestamp.

---

## D17 — OTLP span writes Application node and RUNS_SERVICE edge

**Decision** (Session 9 / DV3 streaming audit): OTLP trace spans were received and broadcast as BonsaiEvents but never written to the graph. The `Application` schema node exists but is never populated.

**Fix**: `write_otlp_span()` upserts an `Application` node keyed by `service_name`. If `peer_address` matches a known `Device.address` (IP prefix match) or `HostEndpoint.ip`, a `RUNS_SERVICE(Device/HostEndpoint → Application)` edge is written. `CARRIES_APPLICATION(AppFlow → Application)` is written when an AppFlow's src_address is the same as the Application's peer_address.

**Consequence**: OTLP spans now participate in the graph. Blast-radius traversal can answer "which services ran on devices affected by this incident".

---

## D25 — Detection/Remediation on a priority write channel

**Decision** (Session 13 / scalability audit): `WriteCoordinator` originally used a single `mpsc` channel for all write types — telemetry, subscription status, detection, and remediation. Under high telemetry ingest load, detection writes queued behind large telemetry batches, causing seconds of latency between a fault being detected and the `DetectionEvent` node appearing in the graph.

**Fix**: Split `WriteRequest` into two separate channels — a normal `WriteRequest` channel (telemetry + subscription status) and a `PriorityWriteRequest` channel (detection + remediation). The coordinator's `select!` uses `biased` to poll the priority channel first on every iteration. Added `submit_priority()` method. Detection and remediation latency is now bounded by the coordinator loop interval, not the telemetry batch depth.

**Consequence**: `src/write_coordinator.rs` — `PriorityWriteRequest` enum, `priority_tx/rx` channel pair, `biased select!`, `submit_priority()`. All `write_detection` and `write_remediation` callers use the priority path.

---

## D26 — SubscriptionStatus batched independently of telemetry

**Decision** (Session 13): `SubscriptionStatusWrite` messages were previously flushed through the write coordinator on every gNMI subscription renewal — which for 50 devices with 30-second keepalives produces ~100 flushes/minute. Each flush interrupted the telemetry batch pipeline.

**Fix**: `sub_status_pending: Vec<SubscriptionStatusWrite>` accumulates up to 128 entries in the coordinator. Flushed on the timer tick or when the telemetry batch is full, not on individual subscription renewal. Added `flush_sub_status_batch()` helper.

**Consequence**: `src/write_coordinator.rs`. No API change. Subscription status writes batch with telemetry naturally, eliminating unnecessary pipeline interruptions.

---

## D27 — CorrelationBuffer: multi-source signal deduplication within 45s window

**Decision** (Session 13): When multiple telemetry sources (gNMI, syslog, SNMP) independently observe the same fault on the same device (e.g. BGP session down from gNMI + syslog + BMP), the current pipeline creates three separate `StateChangeEvent` nodes with identical semantic meaning, and the detection rule fires three times with three separate incidents.

**Fix**: New `CorrelationBuffer` (`src/correlation_buffer.rs`) keyed by `CorrelationKey{device_address, semantic_type, sub_key}`. 45-second deduplication window. `record()` returns `NewSlot` (first observation) or `Absorbed` (duplicate within window). `semantic_key_for_event()` normalises event types across all sources (bgp, bfd, interface, ospf, isis) to a canonical semantic type. A sweep task runs every 10s, logs multi-source fusions, and emits a `bonsai_correlation_multi_source_total` Prometheus counter.

**`write_state_change_event()` calls `record()` after each write**: if `Absorbed`, the event is still written (for observability) but downstream detection is not re-fired.

**Consequence**: `src/correlation_buffer.rs` (new), wired into `GraphStore` as `Arc<CorrelationBuffer>`. All 6 correlatable graph write sub-functions carry `corr_buf`. `write()` and `write_batch()` clone the Arc into `spawn_blocking`. Sweep task in `server_startup.rs`.

---

## D28 — change-detection subscriber: 8× capacity, DropOldest policy

**Decision** (Session 13): The change-detection subscriber (`MpscSubscriber`) had a capacity of 256. Under burst telemetry load (e.g. 50 devices with ON_CHANGE firing simultaneously), the subscriber queue filled and either blocked ingest or dropped detection signals non-deterministically.

**Fix**: `MpscSubscriber::new("change-detection", 2048, OverflowPolicy::DropOldest)` — 8× capacity. On overflow, the oldest (stale) signal is dropped rather than the newest (fresh). This preserves the most recent observation in all cases.

**Consequence**: `src/change_detection.rs`. The `DropOldest` policy is correct for a fault-detection system: a 30-second-old BGP-down signal superseded by a BGP-up is noise; the reverse is not.

---

## D29 — Python SDK bounded WindowRegistry and TTL-evicted rule cooldowns

**Decision** (Session 13 / Python scalability): Two Python-side memory leaks were identified:

1. `WindowRegistry` in `bonsai_sdk/window.py` had no size cap. In long-running collector sessions (30+ days), every unique `(device, feature_key)` pair accumulated unbounded entries. Fixed with `max_entries=4096` cap and FIFO eviction via `evict_stale()`.

2. `_last_fired` dict in `bonsai_sdk/rules/streaming.py` accumulated stale rule-fire timestamps indefinitely. For rules with long cooldowns on rarely-seen devices, this dict grew without bound. Fixed: `_evict_last_fired(now)` called at the start of `evaluate_graph()` removes keys older than the cooldown window.

**Consequence**: `python/bonsai_sdk/window.py` — `max_entries`, `evict_stale()`. `python/bonsai_sdk/rules/streaming.py` — `_evict_last_fired()`. Both changes are backward-compatible.

---

## D30 — Live UI: 3-panel environment-agnostic architecture

**Decision** (Session 14 / UI refactor): The Live Status UI had four compounding problems:
1. **DC-only topology tiering** — role-to-tier mapping used hostname heuristics (`hostname.includes('super')`) and only recognised `superspine/spine/leaf`. Any campus, SP, WAN, or wireless device ended up at the wrong tier or miscoloured.
2. **BGP table always rendered** — confusing and noisy on campus/IoT environments with no BGP.
3. **Fixed-height event feed** — `max-height: 600px` made the feed unusable at scale; SSE had no reconnect logic.
4. **No site context** — site selector was a buried `<select>` inside the topology panel; no per-site health summary; no incident count visible at a glance.

**Fix — 4 components, 1 orchestrating shell**:

- **`SiteRail.svelte`** (new): narrow left column listing all sites derived from topology data. Each entry shows a health dot, device count, and incident badge. Click isolates the topology canvas to that site.
- **`LiveStatusBar.svelte`** (new): 32px top bar — total devices, health pills (critical/warn/healthy), open incident count, pulsing SSE dot, "updated Ns ago".
- **`Live.svelte`**: 3-column grid (`140px | 1fr | 320px`) inside a flex column. Single topology fetch drives SiteRail + StatusBar via `onTopoLoad` callback — no extra requests.
- **`Topology.svelte`** refactor:
  - Accepts `activeSite` prop (site state owned by parent, not topology).
  - `ROLE_TIER` alias map covers 30+ roles: DC fabric, campus (wlc, distribution, access, ap), SP/WAN (pe, p, ce, cpe, wan), firewall, LB.
  - Degree-percentile auto-tier is now a **first-class path**: any topology with missing/unknown roles is correctly tiered by fabric connectivity (top-25% degree → tier 0, bottom-25% → tier 2).
  - Tier rail labels derived from actual node roles, not hardcoded strings.
  - BGP column hidden when no device in the filtered set has BGP data (`hasBgpData` derived).
  - Canvas uses `flex: 1` — fills remaining height; device table capped at `32vh`.
- **`Events.svelte`** refactor:
  - `flex: 1` scroll list (no fixed height).
  - SSE exponential-backoff reconnect (1s → 30s max).
  - Per-event severity coloring derived from event type semantics (down/fail/lost → critical; change/flap → warn).
  - Collapsible JSON detail per event (▾/▴ toggle, hover-reveal actions).
- **`colors.js`**: `roleStrokeColor()` expanded to all 30+ aliases; hostname-based heuristics removed.

**Consequence**: No backend changes. All changes are Svelte/JS. The topology is now environment-agnostic and self-adapts to any network archetype declared in the environment config.

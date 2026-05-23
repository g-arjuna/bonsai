# Bonsai — Consolidated DV4 Backlog

> **Sprint**: DV4
> **Analysis basis**: Full codebase review (`src/`, `python/`, `ui/src/routes/`), Ubuntu Testing Guide S-00→S-69 all ⚠️/⬜ gaps, DV3 backlog state, config sprawl audit, latest commit diff.
> **Principle**: Every item is grounded in actual code state — not documentation assumptions. File paths and testing guide step references are cited throughout.

---

## Epics Summary

| Epic | Title | Priority |
|------|-------|----------|
| D4-1 | SNMP + Syslog: Vendor Architecture + MIB Management | P0 |
| D4-2 | Syslog Shunning / Suppression Engine | P0 |
| D4-3 | App Security: RBAC, LDAP, Secrets, LLM Key UI | P0 |
| D4-4 | Incidents UI Revamp | P0 |
| D4-5 | Flow: sFlow + Server/VM Nodes + TSDB | P1 |
| D4-6 | Graph Quality Metrics + LLM Readiness Score | P1 |
| D4-7 | Config Consolidation: TOML/YAML Sprawl to DB/UI | P1 |
| D4-8 | LLM Feedback Loop + RCA Supervised Testing | P1 |
| D4-9 | Sidecar ML: Tight Integration + Health UI | P1 |
| D4-10 | NetFlow/OTLP Multi-Source Deep Analysis | P1 |
| D4-11 | BMP RFC 7854 Completeness | P1 |
| D4-12 | Redundancy Use Case | P2 |
| D4-13 | UI-based DB Management | P2 |
| D4-14 | Vault Hardening + Init Caveats | P0 |
| D4-15 | HITL + Remediation Testing | P1 |
| D4-16 | BGP Config Change: FRR + BMP + Remediation | P1 |
| D4-17 | Revised Device Onboarding: PyATS-first Bootstrap | P0 |
| D4-18 | NetBox + SNOW Enrichment + AIOps Quality Testing | P1 |
| D4-19 | End-to-End Clean-Slate Testing | P0 |
| D4-20 | Environment Data: Power, Temperature, Optics | P2 |
| D4-21 | Resource Governor UI Page | P1 |
| D4-22 | install.sh + Makefile + CI Hardening | P1 |
| D4-23 | Ubuntu Testing Guide: Remaining Items | P0 |

---

---

## D4-1 — SNMP + Syslog: Vendor Architecture + MIB Management

### Analysis

**SNMP — gaps in `src/signals/snmp.rs` + `config/snmp_oid_patterns/default.yaml`:**
- Custom BER decoder handles v1/v2c; v3 noAuth only — encrypted v3 PDUs bail with "encrypted SNMPv3 scoped PDU is not supported".
- Only 10 OID patterns in `default.yaml`. Nokia TIMETRA-BGP-MIB entries exist but `fields: {}` because peer_address is OID-index-encoded (suffix after trap OID), not a varbind value. Comment in yaml confirms: "Peer address is encoded in the OID index suffix... OID-suffix parsing not yet impl." This is the S-29 ⚠️ gap.
- `SnmpTargetMap.resolve()` matches by peer IP only — no community-string validation, no v3 USM user table.
- **Critical correlation bug**: SNMP `bgpBackwardTransition` trap encodes device socket address (`172.100.109.16:42730`) as `peer_addr`, not the BGP peer IP (`10.9.0.1`). CorrelationKey `sub_key` mismatch → orphan SNMP detection fires separately from gNMI. Confirmed at S-44 ⚠️ PARTIAL and S-53 (separate detections per source, not merged).
- No SNMPv3 USM/VACM. No MIB upload. Zero UI for SNMP management.

**Syslog — gaps in `config/syslog_patterns/` + `src/signals/syslog.rs`:**
- 7 vendor YAML files. Nokia SRL required a second `interface_state` regex for firmware-variant messages (fixed, confirmed S-43 ✅).
- No per-device/per-vendor pattern routing — all patterns applied globally regardless of device `vendor` field.
- No UI for pattern management or hot-reload (disk YAML only).
- TCP syslog receiver: S-24 removed from checklist as "N/A". Dead code status unclear.
- No syslog severity remapping (vendor 0-7 → Bonsai critical/high/medium/low).
- No multiline syslog handling.

### Tasks

**T1 — SNMP: OID index-suffix parser** ✅ batch1
- Implement OID instance-suffix parsing in `SnmpFactExtractor::extract()`.
- Nokia TIMETRA-BGP-MIB: peer IP is last 4 octets of OID suffix. Add `index_suffix_field` option to `SnmpOidPattern`: `{field: peer_address, byte_offset: -4, type: ipv4}`.
- Update `default.yaml` Nokia entries with `index_suffix_field` config.
- Fixes S-29 ⚠️.

**T2 — SNMP: Fix CorrelationKey sub_key for BGP traps** ✅ batch1
- After T1 populates `peer_address`, update `semantic_key_for_event()` in `correlation_buffer.rs` to use peer IP from `snmp_fact` path instead of raw socket `peer_addr`.
- Eliminates orphan SNMP `bgp_neighbor_down` detections. Resolves S-44 PARTIAL.

**T3 — SNMP: SNMPv3 USM auth + priv** ✅ batch16
- Add v3 USM: MD5/SHA auth, DES/AES-128 priv in BER parser.
- `[[signals.snmp.v3_users]]` config: `security_name`, `auth_protocol`, `auth_key_env`, `priv_protocol`, `priv_key_env`.
- Keys stored in vault by alias — never plaintext in TOML.

**T4 — SNMP: MIB upload + compile pipeline**
- UI: upload `.mib` text files.
- Backend: Python subprocess via `pysmi`/`mibdump.py` to compile OID name → numeric OID mapping.
- Auto-generate `SnmpOidPattern` entries from compiled MIB. Store in DB (not just disk YAML).
- Bundle standard MIBs: RFC 2863 IF-MIB, RFC 1657 BGP4-MIB, RFC 2932 IP-MIB.
- Bundle vendor MIBs: Nokia TIMETRA-BGP-MIB, Cisco CISCO-BGP4-MIB/CISCO-OSPF-MIB, Juniper JUNIPER-BGP-MIB.

**T5 — SNMP: UI page (new `src/routes/Snmp.svelte`)** ✅ batch14
- Sections: receiver config (bind addr, community allowlist, version toggles, v3 users), OID pattern library (list/edit/upload MIB), live receiver status badge from `/api/receivers/status`.
- Save → PATCH `/api/settings/streaming` + DB update.

**T6 — SNMP: Trap dedup window + community filtering** ✅ batch9
- Per-device per-OID dedup window (5s): same OID from same device within 5s → suppress second (prevents linkDown/linkUp oscillation storms).
- Community allowlist: accept only configured communities; log warn on unknown community.

**T7 — Syslog: UI page (new `src/routes/Syslog.svelte`)** ✅ batch3
- List vendor pattern files with count; add/edit/disable patterns; test regex against sample syslog message inline; hot-reload via watch channel to receiver without restart.
- Per-device vendor override: force vendor X pattern set on device Y regardless of hostname.

**T8 — Syslog: Vendor coverage expansion** ✅ batch3
- Cisco IOS-XE/XR: `%BGP-5-ADJCHANGE`, `%LINEPROTO-5-UPDOWN`, `%LINK-3-UPDOWN`, `%OSPF-5-ADJCHG`, `%SYS-5-RELOAD`.
- Arista EOS: `%BGP-5-ESTABLISHED`, `%BGP-3-NOTIFICATION`.
- Juniper JunOS: `rpd[*]: bgp_listen_accept_connect:`, `if.info:`.
- Nokia SR-OS: `bgp#0001`, `ISIS if`, `MGMT_CORE #2011` (SR-OS differs from SRL format).
- FRR: `bgpd: %BGP-5-ADJCHANGE`, `isisd: adjacency change`.
- Huawei VRP: zero coverage today — research and add common categories.
- Add multivendor severity translation table (RFC 5424 level → Bonsai severity by vendor).

**T9 — Syslog: TCP receiver audit** ✅ batch9
- S-24 removed as "N/A". Audit `src/signals/syslog.rs` for TCP receiver code path.
- Either: implement RFC 6587 octet-framing properly and reinstate, or formally deprecate and remove dead code.

---

## D4-2 — Syslog Shunning / Suppression Engine

### Analysis

Zero suppression capability exists today. All matching syslog messages emit to the event bus and correlation buffer with no filtering. `resource_governor.rs` has `should_shed()` but it is a blunt global instrument — it cannot target specific message categories, specific devices, or sites. If a Nokia device floods with `LICC: License warning` at 100/sec, every message hits the graph writer. No per-device or per-category rate-limiting exists anywhere in the codebase.

### Tasks

**T1 — ShunRule data model + DB storage** ✅ batch3
- `ShunRule` fields: `id`, `scope_type` (device/group/site/global), `scope_value`, `match_type` (substring/regex/fact_type), `match_value`, `action` (drop/rate_limit), `rate_limit_per_min`, `expires_at_ns`, `created_by`, `created_at_ns`.
- Store as graph DB `ShunRule` nodes or dedicated table.

**T2 — Syslog receiver: shun evaluation on ingest** ✅ batch3
- In `src/signals/syslog.rs`, after fact extraction, before `bus.publish()`:
  - Evaluate active ShunRules matching `(device_address, site, message_body, fact_type)`.
  - `drop`: skip `bus.publish()`, increment `bonsai_syslog_shunned_total{rule_id}` counter.
  - `rate_limit`: per-rule token bucket — allow N/min, drop excess.
  - Still archive shunned messages (raw log preserved) unless `drop_archive: true` explicitly set on rule.

**T3 — REST API for shun management** ✅ batch3
- `GET /api/settings/syslog/shuns` — list active rules with per-rule stats.
- `POST /api/settings/syslog/shuns` — create rule.
- `DELETE /api/settings/syslog/shuns/{id}` — remove rule.
- `GET /api/settings/syslog/shuns/{id}/stats` — events shunned, last fired timestamp.

**T4 — UI: Interactive Shun Panel** ✅ batch3
- In `Syslog.svelte` (D4-1 T7), add a "Shun Rules" tab.
- From live syslog feed: click a message → "Shun this pattern" → auto-populates regex from message text → choose scope (this device / group / site) → set TTL → save.
- Show live shun counter badge per rule. One-click "silence 1h / 24h / permanent" for device scope.
- Visual indicator in Events feed when a category has an active shun rule.

**T5 — Pre-seeded noise patterns** ✅ batch3
- `config/syslog_shun_seeds.yaml`: Nokia `LICC: License warning`, Nokia `LOGGER: Timed out waiting for sync`; Cisco `%SYS-5-CONFIG_I`; FRR BGP max-prefix warning.
- All disabled by default. Operator enables per environment.

---

## D4-3 — App Security: RBAC, LDAP, Secrets + LLM Key Management

### Analysis

**`src/credentials.rs` — vault issues found in code:**
- `StoredCredential.password: String` — decrypted credentials are plain Rust Strings in heap. Not zeroed on drop. Accessible in core dumps.
- `persist_locked()` writes directly to `vault.age` — no atomic rename. Process crash mid-write = vault corruption. No integrity checksum.
- No vault re-key support.

**Auth/RBAC — nothing implemented:**
- Zero RBAC, zero user management, zero role definitions anywhere in the codebase.
- HTTP API has no authentication — all endpoints open to anyone who can reach the port.
- No LDAP/AD integration. No JWT/session mechanism.

**LLM keys — `src/ai_provider.rs`:**
- `build_provider()` reads key from `std::env::var(&cfg.api_key_env)` — key lives as env var string in process memory.
- No vault integration for LLM keys. Supported providers: `gemini` and `moonshot` only. No OpenAI, Anthropic, Ollama, or custom URL.

**DB/transport security:**
- KuzuDB files at `runtime/bonsai.db` — no encryption at rest. Directory permissions not enforced in startup code.
- HTTP server: plain HTTP only — no TLS.

### Tasks

**T1 — Secure credential memory (zeroize)** ✅ batch2
- Add `zeroize` crate. Replace `StoredCredential.password: String` with `zeroize::Zeroizing<String>`. Same for `ResolvedCredential.password`.
- Implement `Drop` on `VaultState` to zero `entries` BTreeMap contents.
- Audit all `resolve()` call sites — ensure no long-lived clones of password into gNMI client config or HTTP request headers.

**T2 — RBAC model**
- Roles: `admin`, `operator`, `viewer`, `api_readonly`.
- `User` + `Role` nodes in graph DB or dedicated users table.
- JWT sessions. Axum middleware in `src/http_server/mod.rs` checks role before dispatching route handlers.
- Scope enforcement: GET topology = viewer+; POST remediation = operator+; vault/user management = admin only.

**T3 — LDAP / Active Directory integration**
- `[auth.ldap]` config block: `server_url`, `bind_dn`, `bind_password_env`, `user_search_base`, `group_search_base`, `role_mapping` (LDAP group → Bonsai role).
- `ldap3` Rust crate for bind + group lookup.
- Fallback to local user DB if LDAP not configured.

**T4 — UI user management (new `src/routes/Users.svelte`)** ✅ batch14
- List users with role badges, last login, created_at. Add/edit/delete local users (admin only). LDAP settings panel + test connection button.

**T5 — UI-based LLM API key management** ✅ batch14
- In `Settings.svelte` or new `src/routes/AiKeys.svelte`.
- Per-provider entry: name, model, API key (stored in vault under alias `llm-{provider}`, masked in UI), custom base URL (for Ollama/LM Studio/vLLM), active toggle.
- "Test connection" → `POST /api/ai/test` → minimal prompt → shows model name, latency, estimated token cost.
- `build_provider()` resolves key from vault by alias rather than env var.
- New providers: OpenAI-compatible (covers OpenAI, Azure OpenAI, Groq, Together, OpenRouter via `base_url` override), Anthropic (Claude 3.5 Sonnet/Haiku), Ollama (local, configurable base URL).

**T6 — Scoped API keys for external consumers**
- `POST /api/auth/apikeys` → generate scoped key (read/remediation/webhook) → return once, store hash.
- Key rotation endpoint. UI list: key alias, last-used timestamp, scope, expiry.

**T7 — DB + transport security**
- Enforce `runtime/` directory mode 700 in server startup.
- TLS for HTTP API: `[server.tls]` cert/key config using axum-server + rustls. Self-signed cert auto-generated at startup if none provided, with warning log.
- Makefile: `make backup` → tarballs `runtime/` with timestamp to `backups/`.

**T8 — Vault write safety + re-key** ✅ batch2
- Atomic write: write to `vault.age.tmp` first, then `rename` over `vault.age`. Prevents corruption on crash.
- Add HMAC-SHA256 integrity tag over encrypted payload. Verify on open. Clear error message on corruption.
- `bonsai credential rekey` subcommand: decrypt with current passphrase → re-encrypt with new passphrase. Test: rekey → restart with new passphrase → credentials accessible.

---

## D4-4 — Incidents UI Revamp

### Analysis

**`ui/src/routes/Incidents.svelte` (548 lines) — gaps found in code review:**
- `co_fire_signature` shown only as `title` attribute (hover tooltip) — invisible on touch/keyboard.
- `row-primary` uses `flex-wrap: wrap` — on narrow viewports device address and rule pills overlap.
- `context-line` has no overflow control — long signatures break layout.
- `TraceRoute.svelte` is a confirmed 310-byte placeholder — zero implementation exists.
- No grouping rationale visible: why are these events one incident? Completely opaque to operator.
- No trigger/trace terminology explanation in UI. Users see "trace →" button with zero context.
- Multi-device incidents show only `N devices · <signature>` — no per-device fault drill-down.
- `incidentDetections().slice(0, 8)` hard cap, "+N more" dead-end with no expand.
- `inc.rule_ids` vs `inc.root?.rule_id` inconsistency — shows 'unknown' if neither is populated.
- From S-53: leaf4 isolation produced 6 detections across 3 devices (leaf4, spine2, spine1). The causal chain is critical context — the current UI provides none of it.

### Tasks

**T1 — Fix layout overflow + blur** ✅ batch4
- `device-addr`: max-width 180px, truncate with ellipsis, full address in tooltip.
- `rule-pills`: show max 3, "+N" badge for overflow with expand on click.
- `context-line`: `max-width: 55%; overflow: hidden; text-overflow: ellipsis`.
- Responsive: stack `row-primary` + `row-secondary` vertically below 600px viewport.
- Move `co_fire_signature` inline into secondary row (not just `title` tooltip).

**T2 — Incident type taxonomy + explanatory chips** ✅ batch4
- Backend: classify incidents as `single_device`, `cascading_failure`, `multi_device_correlated`, `config_caused`.
- UI: type chip on each card with per-type tooltip: "Cascading failure — root fault on leaf4 propagated to 2 neighboring devices."

**T3 — Grouping rationale: visible in expanded view** ✅ batch4
- "Why grouped?" section in expanded body for each incident.
- Cascading: "leaf4 lost uplink at T+0 → BGP hold timer expired 64s later → spine1 lost peer. Temporal proximity + shared blast radius."
- Multi-source: "Same BGP state change confirmed by gNMI + SNMP trap — merged into one detection."
- Config-caused: "Config change by operator 'admin' preceded this fault by 200ms."
- Backend: compute `grouping_rationale` string in `/api/incidents` handler. Compute heuristically if not already in DB.

**T4 — Full Trace page (replace 310-byte placeholder in `TraceRoute.svelte`)** ✅ batch4
- Timeline: all source events contributing to this detection (gNMI/syslog/SNMP timestamps, source-type badges).
- 45-second correlation window visualization showing event positions within the window.
- Investigation link if an investigation exists for this detection.
- HITL history: any approvals/rejections on remediation proposals spawned from this detection.
- Graph context: Device → Interface/BGPPeer implicated.

**T5 — Terminology clarity: trigger + trace explained** ✅ batch4
- Persistent ℹ icon next to "trace →" explaining: "Shows the full timeline of signals (gNMI telemetry, syslog, SNMP trap) that triggered this detection."
- Rename "trace →" to "Trace & Explain."
- In expanded incident: "Triggered by" label already present — keep but add tooltip explaining the 45s correlation window concept.

**T6 — Multi-device drill-down** ✅ batch4
- For `device_count > 1`: "Affected Devices" expandable section in expanded body.
- Per device: address, rules fired, `is_root` flag, `detected_at_ns`.
- Backend: return `affected_devices: [{address, rules, is_root, detected_at_ns}]` in `/api/incidents` response.

**T7 — Backend incident API completeness audit** ✅ batch4
- Audit `src/http_server/observability.rs` incident handler.
- Verify all fields populated: `correlation_chain`, `blast_radius_summary`, `co_fire_signature`, `grouping_rationale`, `affected_devices`, `started_at_ns`, `ended_at_ns`, `remediation_status`, `event_count`, `device_count`.
- Fix nulls silently swallowed by `?? ''` / `?? []` in Svelte UI.
- Add integration test: seed known detections → call `/api/incidents` → assert all expected fields present and non-null.

---

## D4-5 — Flow Support: sFlow + Server/VM Nodes + TSDB Integration

### Analysis

- NetFlow receiver: v9 + IPFIX (v10) only. v5 silently dropped.
- **sFlow not supported** — stated explicitly in Ubuntu guide Phase 8: "Nokia SRL exports sFlow, not NetFlow/IPFIX. Bonsai's NetFlow receiver supports only NetFlow v9 and IPFIX (v10)."
- Nokia SRL (lab devices) exports sFlow natively → no managed-device `CARRIES_FLOW` edges possible until sFlow is implemented. S-38 ⚠️.
- `AppFlow` nodes + `CARRIES_FLOW(Device→AppFlow)` only written when exporter IP matches a Device. Lab uses unmanaged `linux-host1` Alpine box — hence S-38 ⚠️ with 0 rows.
- `HostEndpoint` nodes populated only from LLDP inference + NetBox enrichment. Alpine linux-host1 has no LLDP daemon → S-45/S-46 both ⚠️.
- No sFlow receiver code anywhere in the codebase.
- No bidirectional TSDB integration. Prometheus Remote Write adapter pushes out only — no query path back into Bonsai for historical data.
- `AppFlow.bytes_per_sec` is a point-in-time field — no live rate window, no liveliness alerting.

### Tasks

**T1 — sFlow v5 receiver (RFC 3176)** ✅ batch11
- Implement UDP sFlow v5 parser in Rust.
- Parse: flow samples (raw packet → decoded IP/TCP/UDP headers) and counter samples (interface stats).
- Map flow samples → `AppFlow` nodes using the same model as NetFlow.
- Map counter samples → `Interface` node updates (in/out octets, errors).
- Use sFlow `agent_address` field (not UDP source IP) as exporter identity for `CARRIES_FLOW` Device lookup.
- `[streaming.sflow]` config block: `udp_addr`, `enabled`.
- **Directly unblocks S-38 ⚠️** — Nokia SRL exports sFlow v5; lab CARRIES_FLOW validation becomes possible.

**T2 — Managed device flow validation in lab**
- Configure Nokia SRL devices in signal-test-lab to export sFlow (after T1).
- Add SRL sFlow export config to `signal-test.clab.yml` / device startup config.
- Update Ubuntu testing guide: new step S-38b "sFlow CARRIES_FLOW validation with SRL managed device."

**T3 — ComputeNode / server representation model** ✅ batch11
- Extend `HostEndpoint` or add `ComputeNode` node type: `kind` (server/vm/k8s_node/docker_host/container), `management_ip`, `hostname`, `os`, `k8s_cluster`.
- Multi-source upsert pipeline:
  1. ARP table parsing from PyATS/Genie → MAC→IP → ComputeNode behind ToR switch.
  2. NetFlow/sFlow frequent flow endpoints not in Device table → ComputeNode upsert.
  3. OTLP `peer.address` matching no Device → upsert as `server` ComputeNode.
  4. NetBox enrichment (existing, already upserts HostEndpoints for server/ap/workstation roles).
- Link ComputeNode to uplink via `CONNECTED_TO(ComputeNode→Interface)`.
- UI: clicking a ToR/leaf in topology canvas reveals "Connected Compute" side panel listing attached servers/VMs/containers.

**T4 — Bidirectional TSDB integration**
- `[integrations.tsdb]` config: `type` (prometheus/victoria_metrics/influxdb/thanos), `query_url`, `credential_alias`.
- `GET /api/tsdb/query?metric=...&device=...&start=1h` → proxies to configured TSDB → returns series data.
- Graph Explorer: interface nodes + AppFlow nodes show "Historical Data" sparkline panel fed from TSDB query.
- Preserves graph-first philosophy: graph is live truth layer; TSDB is the historical time-series layer.

**T5 — Live flow rate + liveliness indicators** ✅ batch11
- Per-exporter sliding 60s window: bytes/sec, packets/sec, last_seen_ns maintained in memory.
- `GET /api/flows/live` endpoint: `[{exporter_address, src_prefix, dst_prefix, bytes_per_sec_60s, pps_60s, last_seen_ns}]`.
- Liveliness detection: if exporter silent > 3× expected export interval → emit `flow_exporter_silent` DetectionEvent.
- UI: flow exporter devices show active-flows badge in topology canvas.

---

## D4-6 — Graph Quality Metrics + LLM Readiness Score

### Analysis

No graph quality evaluation exists today. `investigation_runtime.rs` spawns investigations with no pre-flight check on data sufficiency. The NL-query schema in `nl_query.rs` knows what node types exist but has no awareness of how populated they are. When the LLM says "I cannot find sufficient data," there is no structured way to know what is specifically missing.

There is also no UI indicator of graph health — operators have no visibility into which devices have stale telemetry, which interfaces have never sent a gNMI update, or how complete the topology is.

### Tasks

**T1 — Graph quality metric model** ✅ batch5
- Define quality dimensions computed from graph DB via Cypher:
  - **Device Coverage**: % of managed devices with gNMI subscription active, syslog received <24h, SNMP trap received <24h, BMP session active.
  - **Interface Coverage**: % of interfaces with in/out counters populated (non-zero) and last updated <5 minutes.
  - **Topology Completeness**: % of expected device-pair links with LLDP-discovered `CONNECTED_TO` edges.
  - **Protocol Coverage**: % of devices with BGP sessions mapped, IS-IS adjacencies mapped, BFD sessions mapped.
  - **Enrichment Coverage**: % of devices with NetBox enrichment (`netbox_site` property), % with SNOW CMDB CI linked.

**T2 — `GET /api/graph/quality` endpoint** ✅ batch5
- Returns JSON: `{overall_score, device_coverage{total, gnmi_active, syslog_recent, snmp_recent, bmp_active}, interface_coverage{total, with_counters, recently_updated}, topology_completeness{links_expected, links_discovered}, protocol_coverage{bgp_mapped, isis_mapped, bfd_mapped}, enrichment_coverage{netbox_enriched, snow_enriched}, weak_devices[]}`.
- Computed via Cypher queries at call time or cached with 60s TTL.

**T3 — Graph Health tab in Explorer UI** ✅ batch5
- Add "Graph Health" tab in `Explorer.svelte`.
- Radar/spider chart of all quality dimensions.
- Weak devices table: devices below threshold → click to navigate to device detail.
- Last computed timestamp + manual refresh button.
- Color-coded per dimension: green >80%, amber 50-80%, red <50%.

**T4 — Investigation pre-flight check** ✅ batch5
- In `investigation_runtime.rs`, before spawning an investigation: compute `investigation_readiness` score for the target device using quality metric logic.
- If score < 40: prefix investigation summary with "WARNING: Graph data sparse for this device. Missing: {list_of_missing_signals}."
- Store `context_quality_score` field on `Investigation` graph node.
- Surfaces `missing_data` in investigation result UI card to help operators understand why results may be incomplete.

---

## D4-7 — Config Consolidation: TOML/YAML Sprawl to DB/UI

### Analysis

**Config sprawl audit (actual directories found in repo):**
- `config/syslog_patterns/*.yaml` — 7 vendor pattern files
- `config/snmp_oid_patterns/default.yaml` — 1 file
- `config/path_profiles/*.yaml` — 18 gNMI path profile files (subscription paths per vendor)
- `config/synthesizer_rules/*.yaml` — 8 detection rule files
- `config/vendor_state_mapping/*.yaml` — 6 vendor state mapping files
- `config/gnmi_known_issues/*.yaml` — 1 known issues file
- `playbooks/library/*.yaml` — 9 playbook files
- `bonsai.toml` / `docker/configs/signal-test.toml` — main TOML config
- `.env.example` — Docker env vars

5+ config tree directories requiring disk edits. Zero UI management for any of these. Adding a vendor requires editing files in at least 4 separate directories. No hot-reload for any of these config trees.

**Already DB-backed (confirmed)**: managed devices, enrichment adapters, output adapters, investigations, detections, remediations.

**Should move to DB**: syslog patterns, SNMP OID patterns, gNMI path profiles, synthesizer rules, vendor state mappings, playbooks, shun rules, LLM provider configs.

### Tasks

**T1 — DB-backed ConfigItem table** ✅ batch13
- `ConfigItem` schema: `id`, `config_class` (syslog_pattern/snmp_oid_pattern/gnmi_path_profile/synthesizer_rule/vendor_state_mapping/playbook/shun_rule), `vendor`, `name`, `version`, `content_json`, `enabled`, `created_at_ns`, `updated_at_ns`, `created_by`.

**T2 — Boot-time YAML migration** ✅ batch13
- First boot with empty DB: scan all YAML config directories → insert each entry as a `ConfigItem` row.
- Subsequent boots: load config from DB. Fall back to YAML only if DB is empty (graceful degradation during migration window).
- Migration is idempotent: detect already-migrated state by checking for existing rows in `ConfigItem` table.

**T3 — Hot-reload architecture per config class**
- Syslog patterns: `watch::Sender<Arc<SyslogFactExtractor>>` — send updated extractor on PATCH API without process restart.
- SNMP OID patterns: same watch channel approach.
- Synthesizer rules: SIGHUP to Python sidecar or dedicated reload API endpoint on sidecar.
- Playbooks: reload on PATCH; `PlaybookLibrary.load_dir()` in `src/playbook.rs` becomes a DB-backed call.
- gNMI path profiles: complex (subscription restart needed) — flag "apply on next device reconnect" and log notice.

**T4 — UI pages for each config class**
- Existing `Profiles.svelte` (16KB) — audit what it currently covers, extend for gNMI path profiles.
- New `SynthesizerRules.svelte`: list/edit detection rules, enable/disable per rule, test rule against historical detection events.
- Syslog pattern manager already planned in D4-1 T7 (`Syslog.svelte`).
- SNMP OID pattern manager already planned in D4-1 T5 (`Snmp.svelte`).
- Navigation path: Settings → Advanced Config → {Syslog Patterns | SNMP OID Patterns | Detection Rules | gNMI Paths | Playbooks}.

**T5 — Minimal TOML after migration**
- Post-migration, reduce `bonsai.toml` to: server bind address, mode, DB path, vault passphrase env var name, resource profile.
- Everything else managed via UI + DB.
- Update `bonsai.toml.example` to reflect the minimal config and add comments pointing to UI for further configuration.

**T6 — Blank-boot wizard**
- On first boot with empty DB + no managed devices: show onboarding wizard flow:
  1. Set vault passphrase.
  2. Choose resource profile.
  3. Add first managed device (triggers PyATS bootstrap — see D4-17).
  4. Select vendor → auto-load matching syslog patterns, gNMI paths, SNMP patterns from bundled defaults.
- No editing of TOML files required for basic operation after wizard completion.

---

## D4-8 — LLM Feedback Loop + RCA Supervised Testing

### Analysis

**`src/investigation_runtime.rs`** — 15-iteration agentic loop with 4 MCP tools (`get_incident`, `get_device_blast_radius`, `list_active_detections`, `query_graph`). Stores free-text summary only — no structured RCA fields in the `Investigation` graph node.

**`src/ai_provider.rs`** — 2 providers: `gemini` and `moonshot`. `build_provider()` reads key from `std::env::var(&cfg.api_key_env)` — no vault integration. No OpenAI, Anthropic, Ollama, or custom URL support.

**Gaps identified:**
1. Investigation result is unstructured text — no `root_cause_node_id`, `confidence`, `affected_scope`, `missing_data` fields.
2. No operator feedback mechanism — cannot mark an investigation as correct/wrong.
3. No graph schema injected into system prompt — LLM must discover available data by querying instead of knowing upfront.
4. No fault injection → LLM RCA accuracy test harness.
5. Provider set is too narrow for production use.
6. No coverage gap report from completed investigations.

### Tasks

**T1 — Structured RCA output** ✅ batch5
- After final LLM response, run a JSON-extraction pass to populate: `root_cause_type` (interface_down/bgp_peer_down/config_change/packet_loss/...), `confidence` (0.0-1.0), `affected_scope[]` (device addresses), `recommended_action` (human-readable), `missing_data[]` (what the LLM couldn't find).
- Store as `result_json` on `Investigation` graph node.

**T2 — Operator feedback loop** ✅ batch5
- `POST /api/investigations/{id}/feedback`: `{rating: "correct"|"partial"|"wrong", comments: string, actual_root_cause: string}`.
- Store as `InvestigationFeedback` node linked to `Investigation` via `HAS_FEEDBACK` edge.
- UI: thumbs up/down + comment form in `Investigations.svelte` after investigation completes.
- `GET /api/investigations/accuracy` → precision/recall stats across all feedback data.

**T3 — Coverage gap reporter** ✅ batch9
- At end of each investigation: compare queried node/edge paths vs graph quality metrics (D4-6 T1).
- Append `missing_data` list: "No syslog for leaf4 in last 24h", "Interface ethernet-1/1 has no gNMI counter data", "BFD session not mapped."
- Expose in investigation result UI card — helps operator understand why LLM result may be incomplete.

**T4 — LLM provider expansion** ✅ batch5
- Add `OpenAIProvider`: compatible with OpenAI, Azure OpenAI, Groq, Together AI, OpenRouter via `base_url` override.
- Add `AnthropicProvider`: Claude 3.5 Sonnet and Haiku.
- Add `OllamaProvider`: local inference, configurable `base_url` (default `http://localhost:11434`).
- All keys resolved from vault by alias (D4-3 T5) rather than env vars.

**T5 — Graph-schema-aware system prompt** ✅ batch5
- Import `GRAPH_SCHEMA` constant from `src/http_server/nl_query.rs` into `src/investigation_runtime.rs`.
- Inject graph schema as additional system context so LLM knows available node types, relationships, and properties before it starts querying.
- Add few-shot investigation examples covering: BGP session down, interface down, 30% packet loss, config-caused fault, redundancy loss.

**T6 — Fault injection RCA test harness**
- `python/inject_fault.py` (26KB, existing) — audit current capabilities, then extend: `--inject-packet-loss 30`, `--inject-bgp-flap`, `--inject-config-change`, `--inject-interface-down`.
- After each injection: trigger investigation programmatically → wait for completion → compare `root_cause_type` vs expected value.
- Test matrix: interface_down (gNMI only), bgp_neighbor_down (gNMI+syslog), 30% packet loss, config-caused fault, redundancy degraded.
- Track `rca_accuracy_by_scenario` metric over time as graph completeness and prompt quality improve.

---

## D4-9 — Sidecar ML Python: Tight Integration + Health UI

### Analysis

- `python/collector_engine.py` (9.5KB) — gRPC-based detection publisher. No health HTTP endpoint exists.
- `python/bonsai_ml/gnn/` — GNN model code exists; `DeviceEmbedding` schema is in the graph DB; but no scheduled embedding pipeline is wired to server startup.
- **S-49/S-50 ⚠️**: `mode=all` collector never registers via gRPC → `/api/collectors` returns `[]` → no sidecar status in `Collectors.svelte` UI. Root cause: `run_collector_manager` spawn is conditional on `run_collector && !run_core`. When both flags are true (mode=all), neither path registers an in-process collector.
- `python/soak_test.py` (11KB) — soak test script exists but results are not surfaced anywhere in the UI or via any API.
- `python/bonsai_ml/gnn/` has model training code; `export_training.py` (2KB) and `train_anomaly.py` (4.6KB) exist but no live scheduled pipeline runs embeddings against the production graph.

### Tasks

**T1 — Python sidecar health HTTP endpoint** ✅ batch14
- Add lightweight HTTP server to `collector_engine.py`: `GET /health` returns `{status, uptime_secs, rules_loaded, last_detection_at_ns, detections_today, queue_depth}`.
- `[sidecar] health_port = 9200` in TOML config.

**T2 — Rust backend: `/api/sidecar/status`** ✅ batch6
- New endpoint that proxies to sidecar health URL or returns last-known gRPC heartbeat timestamp.
- Show sidecar as a card in `Collectors.svelte`: rules count, last detection, queue depth, health badge (green/yellow/red/grey).

**T3 — Fix mode=all collector registration (resolves S-49/S-50 ⚠️)** ✅ batch9
- When `run_core && run_collector` (mode=all): auto-register local in-process collector with well-known ID at startup rather than requiring external gRPC registration.
- After fix: `/api/collectors` returns the in-process collector entry. Receiver badges on Collectors card become functional.

**T4 — Rules visibility + hot-reload from UI**
- `GET /api/sidecar/rules` → list loaded detection rules: `rule_id`, enabled, description, last_fired_at_ns, fires_today.
- `PATCH /api/sidecar/rules/{id}` → enable/disable without sidecar restart (hot-reload via IPC or signal).
- UI: rule list panel in the sidecar card in Collectors.

**T5 — GNN embedding pipeline wiring** ✅ batch6
- Scheduled background job in `collector_engine.py` (every 30 min): export graph node features from KuzuDB → run GNN forward pass → write `DeviceEmbedding` nodes back to graph.
- Use embeddings in anomaly detection: cosine similarity between current embedding and historical baseline → compute anomaly score.
- `export_training.py` and GNN model code already exist — this is a wiring task not a model task.

**T6 — Dedicated Sidecar UI page (new `src/routes/Sidecar.svelte`)** ✅ batch14
- Active rules panel: name, vendor, severity, last fired, fires today, enable/disable toggle.
- Detection feed: last 50 detections from sidecar with per-detection latency.
- Embedding space: 2D UMAP projection of current `DeviceEmbedding` nodes, coloured by health state.
- Training controls: "Re-train anomaly model" button, training data summary, last model accuracy metric.
- Model card viewer: renders markdown files from `python/bonsai_ml/model_cards/`.

---

## D4-10 — NetFlow/OTLP Multi-Source Correlation: Deep Analysis

### Analysis

**NetFlow — gaps from code review:**
- `AppFlow` node: `src_address`, `dst_address`, `exporter_address`, `bytes_per_sec`, `protocol`.
- `CARRIES_FLOW(Device→AppFlow)` written only when `exporter_address` matches a registered Device. Tested only with unmanaged `linux-host1` (Alpine) — S-38 ⚠️ confirms 0 CARRIES_FLOW edges from managed devices.
- `AppFlow` nodes are **not** in `semantic_key_for_event()` in `correlation_buffer.rs`. No detection fires from flow anomalies — flow data enters the graph only, not the detection pipeline.
- No threshold-based flow detection rule exists (e.g., interface utilization >90% derived from flow data).

**OTLP — gaps from code review:**
- `Application` node + `RUNS_SERVICE(Device→App)` verified working (S-41/S-42 ✅).
- Peer match uses `d.address STARTS WITH peer_address` — works for `:57400` gNMI suffix. Fragile if spans arrive from non-gNMI ports on devices not registered in graph.
- **No OTLP metrics receiver** (`/v1/metrics`) — only traces (`/v1/traces`). Server CPU/memory/request-rate metrics from OpenTelemetry agents are not ingested.
- No temporal correlation between OTLP application latency and network `DetectionEvent` nodes.

**Multi-source correlation readiness:**
- Flow + network events: no correlation path — `AppFlow` events don't enter `CorrelationBuffer`.
- OTLP + network events: no correlation path — `Application` node has no link to `DetectionEvent`.

### Tasks

**T1 — Flow-based detection rules** ✅ batch12
- `flow_interface_utilization_high` synthesizer rule: if `AppFlow.bytes_per_sec / Interface.speed > 0.9` → `medium` severity detection.
- `flow_exporter_silent` rule (see D4-5 T5): exporter silent > 3× expected interval → detection.
- Wire `AppFlow` write events through `semantic_key_for_event()` so flow anomalies enter `CorrelationBuffer` and can merge with gNMI/syslog events for the same device.

**T2 — OTLP metrics receiver (`/v1/metrics`)** ✅ batch12
- Add `POST /v1/metrics` handler alongside the existing `/v1/traces` at port 4318.
- Parse OTLP proto metrics: gauge, sum, histogram types.
- Write application metrics to `Application` node properties: `cpu_pct`, `memory_mb`, `req_per_sec`, `error_rate`.
- Enable synthesizer rules on app metrics (e.g., `app_error_rate_spike` detection when error_rate > threshold).

**T3 — OTLP trace + network event temporal correlation**
- If an `Application` node has a `RUNS_SERVICE` Device that has an active `DetectionEvent` within ±30s of a trace span latency anomaly → create `APP_IMPACTED_BY_NETWORK(Application→DetectionEvent)` edge.
- Surface in incident detail: "Application bonsai-test-app experienced 3× p99 latency during leaf4 BGP reconvergence."
- Threshold for "latency anomaly": configurable in config, default 3× p50 baseline.

**T4 — Multi-server OTLP testing in lab**
- Ubuntu testing guide: add steps to send OTLP traces + metrics from multiple simulated server processes on `linux-host1` and `linux-host2`.
- Validate: each simulated server maps to a different `ComputeNode` behind a different ToR switch.
- Validate: `APP_IMPACTED_BY_NETWORK` edge created during leaf4 isolation fault injection (S-68 round-trip scenario).

**T5 — CARRIES_FLOW from managed device: validation step**
- After D4-5 T1 (sFlow receiver) + D4-5 T2 (Nokia SRL sFlow export config): validate end-to-end.
- Run: SRL device exports sFlow → bonsai receives → `AppFlow` node created → `CARRIES_FLOW(Device→AppFlow)` edge written → verify via Graph Explorer Cypher query.
- Update Ubuntu testing guide: new S-38b "sFlow CARRIES_FLOW from Nokia SRL managed device."

---

## D4-11 — BMP RFC 7854 Completeness

### Analysis

**From Ubuntu testing guide results:**
- S-30/S-31/S-32 ✅: BMP session established, `ROUTE_MONITORING` messages arriving with `rib_type` being written.
- S-32b ⚠️: PeerUp BGP OPEN capabilities — `hold_time` offset bug → capabilities list always empty.
- S-33 ⚠️: BMP+gNMI cross-device correlation structurally impossible with current per-device correlation model. gNMI sees BGP flap from super1's perspective (`device_address=172.100.109.16`); BMP sees it from frr-rr's perspective (`device_address=172.100.109.20`). Different `device_address` → different CorrelationKey → events never merge. This is noted as "structural — cross-device device_address mismatch."

**RFC 7854 coverage gaps from code analysis:**
- BGP OPEN capabilities in PeerUp message: parsing confirmed broken (S-32b — `hold_time` offset wrong).
- `STATS_REPORT` messages (RFC 7854 §4.8): ~30 stat counters defined — no evidence any are parsed in the codebase.
- `PEER_DOWN` reason codes (RFC 7854 §4.9): 7 reason codes defined. Full mapping status unknown.
- BMP Initiation TLVs (RFC 7854 §4.3): `sysDescr`, `sysName`, `bgpID` — whether written to Device node is unverified.
- Adj-RIB-Out support: optionally sent by some BMP implementations; handling status unknown.
- Extended communities + large communities (RFC 8092) in ROUTE_MONITORING UPDATE: likely not parsed.
- `ROUTE_MIRRORING` (RFC 7854 §4.7): almost certainly unimplemented.

### Tasks

**T1 — Fix PeerUp BGP OPEN capabilities parsing (S-32b)** ✅ batch10
- RFC 7854 §4.10: BGP OPEN message layout is `version(1) + AS(2) + hold_time(2) + bgp_id(4) + opt_params_len(1) + opt_params`.
- Find and fix the `hold_time` offset calculation in the BMP PeerUp handler.
- Parse capability TLVs: Multi-protocol extensions (type 1), Route Refresh (type 2), Graceful Restart (type 64), 4-Byte AS (type 65), Add-Path (type 69).
- Write capability list to `BgpSession` or `Device` node properties.

**T2 — STATS_REPORT parsing + graph write** ✅ batch12
- Parse at minimum: rejected prefixes (type 0), Adj-RIB-In prefix count (type 7), Loc-RIB prefix count (type 8).
- Write to `BgpSession` node as properties with timestamps.
- Add synthesizer rule `bgp_rib_prefix_spike`: if Adj-RIB-In count changes >20% in one STATS_REPORT cycle → `medium` severity detection.

**T3 — PEER_DOWN reason code completeness** ✅ batch10
- RFC 7854 §4.9 reason codes: 0=local system closed, 1=local NOTIFICATION sent, 2=deconfigured, 3=remote NOTIFICATION received, 4=remote system closed, 5=peer config change, 6=VRF peer deleted.
- Map each to a distinct `bmp_peer_down` event sub-type for more precise incident classification.

**T4 — BMP Initiation TLVs → Device node properties** ✅ batch10
- RFC 7854 §4.3: parse `sysDescr` (type 1), `sysName` (type 2), `bgpID` (embedded in Per-Peer Header) TLVs from BMP Initiation message.
- Write to Device node: `bmp_sys_descr`, `bmp_sys_name`, `bmp_bgp_id`.
- Critical for FRR nodes which have BMP only and no gNMI — this is their only source of system identity.

**T5 — Cross-device BMP+gNMI correlation: architecture decision**
- S-33 ⚠️ is a confirmed structural gap. Before implementation, produce an Architecture Decision Record.
- Proposed design: introduce `BgpSessionKey(lower_ip, higher_ip)` as a canonical cross-device session identifier (canonical form: IPs sorted lexicographically).
- Extend `CorrelationBuffer` to support cross-device slot merging via `BgpSessionKey` lookup.
- `semantic_key_for_event()` for BGP events generates a `BgpSessionKey` in addition to (or replacing) the device-scoped key.
- This is significant design work — ADR required before code changes.

**T6 — Extended + large community attribute parsing** ✅ batch12
- Parse BGP UPDATE extended communities (RFC 4360) and large communities (RFC 8092) in ROUTE_MONITORING messages.
- Write to prefix properties in graph: `ext_communities[]`, `large_communities[]`.
- Enables graph explorer queries: "which prefixes carry community 64500:100?" and policy-aware correlation.

---

## D4-12 — Redundancy Use Case: Graph-baked Detection

### Analysis

Redundancy detection is **not implemented** anywhere in the current codebase. The graph has the raw materials — `CONNECTED_TO(Interface→Interface)` from LLDP, `BGP_SESSION(Device→Device)` from gNMI/BMP, `HostEndpoint` with `CONNECTED_TO` to interface — but no `RedundancyGroup` model, no redundancy-loss detection rule, no UI indication.

For a dual-homed server (two interfaces to two ToR switches), the graph doesn't know it's dual-homed until an analysis pass runs. For ECMP paths, the routing table is not yet in the graph. This is a significant operational gap — many network incidents are redundancy-loss events where a single-point-of-failure appears without any individual detection firing.

### Tasks

**T1 — RedundancyGroup model in graph** ✅ batch14
- `RedundancyGroup` node: `id`, `type` (ecmp/lag/dual_homed/bgp_multipath/vrrp), `member_count`, `original_member_count`, `member_node_ids[]`, `protects_node_id` (device or host endpoint ID).
- `MEMBER_OF(Device/Interface→RedundancyGroup)` edge with `role` (primary/secondary/member).
- `PROVIDES_REDUNDANCY_FOR(RedundancyGroup→Device/HostEndpoint)` edge.

**T2 — Redundancy discovery via PyATS/Genie (during onboarding)**
- During device onboarding (D4-17): parse live device state for:
  - LAG/port-channel membership: `show etherchannel summary` (Cisco), `show lacp` (Nokia/Arista).
  - ECMP next-hops: `show ip route`, `show route` (multiple next-hops = ECMP group).
  - Dual-homed server detection: ARP table shows same MAC IP on two different interfaces of the same ToR → dual-homed host.
  - Dual-uplink ToR: LLDP shows same host on two leaf switches → dual-homed endpoint.

**T3 — SuzieQ integration evaluation**
- SuzieQ (suzieq OSS) parses and normalises multi-vendor topology, LLDP, BGP, routing, interfaces across Cisco/Arista/Juniper/Nokia/FRR.
- Evaluate using SuzieQ as a library or subprocess for redundancy discovery analysis rather than maintaining separate PyATS parsers per vendor.
- Deliverable: Architecture Decision Record — SuzieQ vs PyATS/Genie vs Bonsai-native parsing.

**T4 — Redundancy loss detection rules** ✅ batch14
- `redundancy_degraded` rule: `RedundancyGroup.member_count < original_member_count` → `medium` severity.
- `redundancy_lost` rule: `member_count = 1` (single point of failure) → `high` severity.
- Trigger path: `interface_down` or `bgp_session_down` detection → check if affected node is a `RedundancyGroup` member → compute new member_count → fire redundancy rule if threshold crossed.
- Severity escalation: if the protected host is a critical application server → escalate to `critical`.

**T5 — UI: Redundancy indication in topology canvas**
- Redundancy group members shown with chain/link icon on their topology node.
- When redundancy degrades: icon turns amber; when lost: icon turns red.
- Tooltip on red icon: "Server X was dual-homed to leaf1+leaf2. leaf2 uplink is down — single point of failure."
- Incidents panel: `redundancy_lost` incidents show impacted servers/hosts with their connectivity state.

---

## D4-13 — UI-based DB Management

### Analysis

No DB management UI exists. KuzuDB stores all graph data at `runtime/bonsai.db`. There is no way to: inspect table sizes and record counts, purge stale data, view the schema, export node types, or safely run management operations — except via the open-ended Graph Explorer Cypher tab which has no guardrails, no safety controls, and no confirmation steps.

Over time, `DetectionEvent` nodes, `AppFlow` nodes from heavy flow sources, and `AgentToolCall` nodes from many investigations will accumulate. No TTL or automated pruning exists. There is also no documented backup or restore procedure.

### Tasks

**T1 — DB stats API** ✅ batch6
- `GET /api/db/stats` → `{node_counts: {Device: N, Interface: N, AppFlow: N, DetectionEvent: N, ...}, rel_counts: {CONNECTED_TO: N, CARRIES_FLOW: N, ...}, db_size_bytes, wal_size_bytes, oldest_record_ns, newest_record_ns}`.
- Computed via KuzuDB `COUNT(*)` queries per table.

**T2 — Schema viewer endpoint + UI tab** ✅ batch6
- `GET /api/db/schema` → returns all node tables with column names and types, all rel tables with from/to node types and columns.
- UI: new `src/routes/DbManagement.svelte` with a "Schema" tab showing this information in a formatted table.

**T3 — Safe data management operations (admin-only)** ✅ batch6
- Purge old detections: `DELETE /api/db/purge?node_type=DetectionEvent&older_than_days=90` (admin role required — D4-3 T2).
- Purge orphan AppFlow nodes: nodes with no `CARRIES_FLOW` edge and older than N days.
- Purge old AgentToolCall nodes: older than N days.
- KuzuDB checkpoint: `POST /api/db/checkpoint` to force a WAL flush and compaction.
- Export: `GET /api/db/export?node_type=Device` → returns JSONL download of all nodes of that type.

**T4 — Backup + restore** ✅ batch6
- `POST /api/db/backup` → tar+gzip the `runtime/` directory to `backups/bonsai-{iso_timestamp}.tar.gz`. Return backup filename.
- `POST /api/db/restore` with multipart backup file upload → extract to a `runtime.restore/` staging directory → swap on next restart.
- Makefile targets: `make backup`, `make restore BACKUP=backups/bonsai-2026-01-15T10:30:00.tar.gz`.
- UI: Backup/Restore section in `DbManagement.svelte` — shows existing backups, "Create backup" button, restore from file upload.

---

## D4-14 — Vault Hardening + Init Caveats

### Analysis

**Memory risks confirmed in `src/credentials.rs`:**
- `StoredCredential { password: String }` — decrypted credentials are plain Rust `String` on the heap. Not zeroed on drop. Accessible in core dumps, memory snapshots, and allocator pools after deallocation.
- `VaultState.entries: BTreeMap<String, StoredCredential>` — holds ALL decrypted credentials in memory for the entire process lifetime after vault unlock. There is no per-credential TTL or re-encryption at rest in memory.

**Init/write safety risks confirmed in code:**
- `persist_locked()` calls `std::fs::write(vault_path, &encrypted)` — writes directly to `vault.age` in one call. If the process crashes mid-write (OOM, SIGKILL), the vault file is partially written and unrecoverable. No atomic rename protection.
- No HMAC or checksum on the vault file. Corruption error message is generic.
- No vault re-key subcommand exists. Changing the passphrase requires manually deleting and re-creating all credentials.

**Startup sequence risk:**
- `server_startup.rs` spawns multiple tokio background tasks before vault unlock can fail. If vault unlock fails partway through startup, whether all already-spawned tasks receive the shutdown signal and stop cleanly is unverified.

### Tasks

**T1 — Zeroizing credential memory** ✅ batch2
- Add `zeroize` crate to `Cargo.toml`.
- Change `StoredCredential.password: String` → `zeroize::Zeroizing<String>`.
- Change `ResolvedCredential.password: String` → `zeroize::Zeroizing<String>`.
- Add `impl Drop for VaultState` that explicitly calls `.clear()` and `.zeroize()` on entries before deallocation.
- Audit all callers of `vault.resolve()`: verify no long-lived `Arc<ResolvedCredential>` or cloned password strings in gNMI client configs, HTTP client headers, or enrichment adapter configs.

**T2 — Atomic vault write + integrity checksum** ✅ batch2
- In `persist_locked()`: write to `{vault_path}.tmp` first using `std::fs::write`, then `std::fs::rename` over the final path. Atomic on POSIX — crash during write leaves `.tmp`, original `vault.age` intact.
- Add HMAC-SHA256 integrity tag over the encrypted payload bytes (key derived from passphrase). Prepend tag to vault file.
- On `decrypt_entries()`: verify HMAC before attempting decryption. Return actionable error on failure: "vault integrity check failed — file may be corrupt, restore from backup."

**T3 — Vault re-key subcommand** ✅ batch2
- Add `bonsai credential rekey` CLI subcommand.
- Flow: open vault with `BONSAI_VAULT_PASSPHRASE` → decrypt all entries → re-encrypt with new passphrase from `BONSAI_VAULT_NEW_PASSPHRASE` env var → atomic write.
- Test: rekey → set new passphrase env var → restart bonsai → verify all credentials accessible.

**T4 — Startup crash path audit** ✅ batch2
- Trace `src/server_startup.rs` initialization sequence: map all `tokio::spawn()` calls before and after vault unlock.
- If vault unlock fails: ensure all already-spawned tasks receive the shutdown watch signal and cleanly stop before process exit.
- Add test: simulate vault init failure (wrong passphrase) → verify clean process exit, no leaked background tasks.

**T5 — Vault init documentation** ✅ batch9
- Document in `scripts/install.sh` and README: vault passphrase requirements (minimum length, complexity), what happens if passphrase is lost (credentials are unrecoverable — backup the `.age` file before any re-key operation), and how to run re-key.

---

## D4-15 — HITL + Remediation Testing

### Analysis

- `src/http_server/remediation.rs` — remediation endpoints exist. `RemediationProposal` trust states (`pending_review`, `approved`, `executing`, `completed`, `failed`) are defined.
- `src/playbook.rs` — `PlaybookLibrary` loads 9 YAML playbooks from `playbooks/library/` covering: bgp_session_down, bgp_all_peers_down, bgp_never_established, bgp_session_flap, bfd_session_down, interface_down, interface_error_spike, interface_high_utilization, topology_edge_lost.
- `ui/src/routes/Approvals.svelte` (13KB) — HITL approval UI exists.
- `python/train_remediation.py` (6.3KB) — remediation model training script exists.

**Confirmed gaps:**
- No end-to-end test of: detection → investigation → HITL proposal in Approvals UI → operator approves → remediation executed → outcome verified.
- For 60% packet loss scenario: no playbook exists. Which playbook fires? Is the HITL suggestion realistic?
- Playbook execution verified only as "proposal generated" — actual gNMI/CLI command execution not tested end-to-end.
- Rejection path: whether rejection prevents command dispatch and logs an audit entry has not been tested.
- `verify_graph` steps in playbooks (wired in python/bonsai_agent/tools.py `propose_playbook`) never end-to-end tested against a post-remediation graph state.

### Tasks

**T1 — End-to-end HITL test scenario (Phase 18 in Ubuntu testing guide)** ✅ batch15
- Scenario: inject BGP down on leaf4 (variant of S-53) → wait for detection → trigger investigation → verify `RemediationProposal` appears in Approvals UI with correct device + playbook.
- Approve the proposal → verify remediation command executes on device (check `sr_cli` command logs or gNMI confirm).
- Verify detection resolves after heal → verify `RemediationProposal.state = completed`.
- Document pass/fail in Ubuntu testing guide as new section "Phase 18 — Remediation Round-Trip."

**T2 — 60% packet loss HITL realism test** ✅ batch15
- Configure policer/rate-limiter on ContainerLab node to drop 60% of traffic on a leaf4 uplink interface.
- Trigger investigation on affected device. Evaluate: does a `RemediationProposal` appear? What playbook maps to this scenario?
- If no playbook covers packet loss: create `interface_high_error_rate` playbook with steps: `get_interface_error_counters`, `clear_interface_counters`, `check_cable_diagnostic` (Nokia `sfm-check` or Cisco `test cable-diagnostics`).
- Test that HITL suggestion is realistic — not a generic "restart BGP" for a packet-loss scenario.

**T3 — Remediation outcome verification** ✅ batch13
- After an approved remediation executes: run playbook `verify_graph` step → query graph to confirm fault condition resolved (BGP session re-established, interface oper_status = up, error counters cleared).
- `POST /api/remediations/{id}/verify` endpoint → executes verify_graph Cypher → returns `{passed: bool, details: string}`.
- Surface verification result in Approvals UI: green "Verified healed" or red "Verification failed — fault may still be active."

**T4 — Rejection path test**
- Test operator rejecting a `RemediationProposal`.
- Verify: `RemediationProposal.state = rejected`, zero device commands dispatched, audit log entry created with operator identity + timestamp.
- Test: partial approval workflow — operator edits proposal steps before approving (if this capability is intended).

**T5 — Playbook library gap analysis**
- Audit the 9 existing playbooks against common detection rule IDs in `config/synthesizer_rules/*.yaml`.
- For each detection rule that fires in S-53 lab scenario: verify a matching playbook exists.
- Document missing playbooks as items: `ospf_neighbor_down`, `isis_adjacency_down`, `config_caused_fault` recovery, `flow_exporter_silent` recovery.

---

## D4-16 — BGP Config Change: FRR + BMP + Graph + Remediation

### Analysis

FRR runs as `frr-rr` in the lab topology. BMP session confirmed working (S-30/S-31 ✅). `ROUTE_MONITORING` messages arrive with `rib_type` written. The config change detection pipeline (`src/change_detection.rs`, `src/integrations/change_management.rs`) is implemented and tested for Nokia SRL devices via syslog commit detection.

**Gaps specific to FRR + BGP config changes:**
- FRR has no gNMI support. Its telemetry is exclusively via BMP and syslog from the FRR daemon log.
- `ChangeRequest` and `CHANGE_CAUSED_DETECTION` edges are wired in the ServiceNow + AAP change management integration. But FRR config changes (via `vtysh` or FRR config file reload) produce no `ConfigChange` nodes today — there is no syslog pattern that captures FRR `vtysh` configuration commits.
- FRR BGP config changes: `bgpd` logs `%BGP-5-ADJCHANGE: neighbor X Down User reset` when a session is manually cleared. This is a config-caused BGP down but not detected as such — it fires as a plain `bgp_neighbor_down` detection with no `config_caused_fault` annotation.
- No playbook for FRR BGP session recovery via FRR-specific CLI.

### Tasks

**T1 — FRR syslog pattern: config change detection** ✅ batch10
- Add to `config/syslog_patterns/frr.yaml`:
  - `config_change_detail` fact_type pattern for: `bgpd[*]: Configuration changed`, `zebra[*]: route-map changed`, vtysh config-write log lines.
  - `process_restart` pattern for: `bgpd: starting`, `ospfd: starting` (FRR daemon respawn after config reload).
- These patterns produce `ConfigChange` nodes with `username` and `change_description` fields.

**T2 — FRR BGP: "User reset" as config-caused fault** ✅ batch10
- Add syslog pattern for `%BGP-5-ADJCHANGE: neighbor X Down User reset` → classify as `config_caused_bgp_down` fact_type.
- In `write_state_change_event()` for FRR BGP events, if `change_source = "user_reset"` → set `config_correlated = true` on the resulting `DetectionEvent`.
- Triggers `CHANGE_CAUSED_DETECTION` edge creation and `[DURING CONFIG CHANGE]` prefix on detection reason.

**T3 — BMP route policy change detection**
- When `STATS_REPORT` Adj-RIB-In count drops sharply (> 20% within one report cycle) without a session going down → `bgp_policy_filter_spike` detection.
- Indicator of a route-map or prefix-list change that silently filtered accepted routes.
- Correlate with `ConfigChange` nodes from FRR syslog within ±60s window.

**T4 — FRR BGP playbook** ✅ batch10
- Create `playbooks/library/frr_bgp_session_down.yaml` for FRR-specific recovery.
- Steps: `vtysh -c "clear ip bgp X soft"` → verify via BMP STATS_REPORT Adj-RIB-In restores → verify via graph `BgpSession.state = established`.
- Risk tier: `low` (soft clear is non-disruptive).
- Second playbook for hard reset: risk tier `medium`, requires HITL approval.

**T5 — FRR + BMP investigation integration**
- Ensure `investigation_runtime.rs` query tools can retrieve FRR BMP data: `get_device_blast_radius` for `frr-rr` device address returns BMP session state, STATS_REPORT prefix counts.
- Test: inject FRR BGP fault → trigger investigation → verify LLM identifies FRR as root cause using BMP data.

---

## D4-17 — Revised Device Onboarding: PyATS-first Bootstrap

### Analysis

**Current onboarding flow (from `scripts/install.sh` + `ui/src/lib/Onboarding.svelte`):**
- Operator manually enters device address, hostname, vendor, gNMI port, credential alias in UI.
- Device is added to managed device list. gNMI subscription starts.
- No automated discovery. No pre-connection verification. No vendor-specific config bootstrapping.
- `Onboarding.svelte` is 1660 lines — pre-existing `{@const}` error noted in UI design session; this file has known technical debt.

**What is missing:**
- No "bootstrap" step that SSHes into the device before adding it, to discover: vendor, OS version, hostname, available gNMI paths, interface names, BGP neighbors, IS-IS neighbors, routing table.
- For Nokia SRL: gNMI requires TLS with CA cert. No automated cert generation or push for new devices.
- For FRR: no gNMI. BMP is the only telemetry path. No automated BMP target configuration.
- No bulk onboarding — adding 8 devices requires 8 separate form submissions.
- No onboarding from seed file (YAML/CSV device list).
- PyATS/Genie library (`pyats`, `genie`) is not referenced anywhere in the Python codebase despite being the industry standard for multi-vendor network device automation.

**Why PyATS-first matters:**
- Genie parsers cover 200+ show commands across Cisco IOS/IOS-XE/IOS-XR/NX-OS, Arista EOS, Juniper JunOS, Nokia SR-OS/SR Linux, FRR.
- A single PyATS `learn('bgp', 'interface', 'routing')` call against a new device → normalised structured data → can auto-populate the graph with interface names, BGP neighbors, IS-IS adjacencies, and redundancy groups at onboarding time.
- Replaces the need for 8+ separate gNMI subscription paths to be known in advance per vendor.

### Tasks

**T1 — PyATS bootstrap agent (new `python/bootstrap_agent.py`)** ✅ batch7
- Input: device address, credentials (from vault), optional vendor hint.
- Connect via SSH (Netmiko/PyATS connection plugin).
- Run `device.learn('bgp', 'interface', 'routing', 'lldp', 'lag')` using Genie.
- Output structured JSON: hostname, vendor, OS version, interface list (name/speed/oper_status), BGP neighbors (peer_address, peer_as, state), IS-IS adjacencies, LAG memberships, LLDP neighbors, ARP table.
- Call `POST /api/devices` to register device in Bonsai graph with all discovered properties.
- Call `POST /api/devices/{address}/topology` to seed interface, BGP, IS-IS, LLDP data before first gNMI subscription.

**T2 — Bootstrap integration in Onboarding UI** ✅ batch7
- Add "Bootstrap from device" step to `Onboarding.svelte` flow:
  1. Enter address + credential alias.
  2. Click "Discover" → calls `POST /api/devices/bootstrap` → runs `bootstrap_agent.py` → shows progress.
  3. Review discovered properties (hostname, vendor, interfaces, BGP peers) in a confirmation panel.
  4. Confirm → device added with pre-seeded graph data. gNMI subscription starts using auto-detected paths for vendor.
- Show bootstrap progress via SSE stream from `/api/devices/bootstrap/{job_id}/stream`.

**T3 — Bulk onboarding from seed file** ✅ batch7
- `POST /api/devices/bulk` accepts YAML/JSON seed file: `[{address, hostname, vendor, credential_alias, bootstrap: true}]`.
- Runs bootstrap agent for each device in parallel (max 4 concurrent to avoid SSH flooding).
- Returns bulk result: `[{address, status: ok|failed, error: string}]`.
- UI: file upload widget in Onboarding page for bulk import.

**T4 — Nokia SRL: automated gNMI TLS setup** ✅ batch14
- During bootstrap for Nokia SRL: detect that gNMI requires TLS.
- Retrieve ContainerLab CA cert or device self-signed cert via RESTCONF or SCP.
- Store cert in `runtime/certs/{device_address}.pem`.
- Configure gNMI subscription to use cert for this device automatically.

**T5 — FRR: automated BMP target configuration** ✅ batch14
- During bootstrap for FRR device: detect FRR via `show version` / `vtysh -c "show version"`.
- Push BMP target configuration to FRR via SSH: `router bgp {as}`, `bmp targets bonsai`, `bmp connect {bonsai_ip} port 5000 min-retry 30 max-retry 120`.
- Verify BMP session establishes within 60s.

**T6 — Post-bootstrap graph pre-seeding** ✅ batch14
- After bootstrap completes, write to graph: `Interface` nodes for all discovered interfaces, `BgpSession` nodes for all discovered BGP neighbors, `IsIsAdj` nodes for discovered adjacencies, `RedundancyGroup` nodes for discovered LAGs.
- Mark these nodes with `source: bootstrap` and `bootstrap_at_ns: now` so telemetry updates can be tracked as delta-from-bootstrap.
- This means first gNMI subscription has a baseline to compare against rather than starting cold.

---

## D4-18 — NetBox + ServiceNow Enrichment + AIOps Quality Testing

### Analysis

**NetBox enrichment state (from `src/enrichment/netbox.rs` + S-60/S-61 in testing guide):**
- S-60 (register NetBox enricher via UI) and S-61 (verify enrichment wrote graph nodes with `netbox_site`) are part of Phase 17 which was not run in previous test sessions — these steps are ⬜ blank.
- NetBox enricher second pass (D3-11 T5) is implemented: `get_devices_by_roles()` with configurable `endpoint_roles`, `NbInterfaceConnected` struct, HostEndpoint upsert + CONNECTED_TO wiring.
- NetBox has no devices seeded in a fresh install — S-61 provides a seed script, but it requires NetBox device_type and role IDs which may differ per instance.
- Reconciliation engine: `reconcile_and_write_provenance()` in `src/enrichment/servicenow.rs` with default priority `cli > netbox > servicenow`. NetBox conflict detection in `DeviceDrawer.svelte` 'enrichment' tab + conflict banner.

**ServiceNow CMDB + AIOps state:**
- CMDB integration complete: 9 concurrent table fetches, ChangeRequest polling, `AFFECTED_BY_CHANGE`, `RELATED_TO_CHANGE` edges all implemented.
- ServiceNow PDI is used for testing (not WireMock). S-58 (verify PDI connectivity) and S-65 (register SNOW EM adapter) are ⬜ not yet run in testing sessions.
- `src/integrations/servicenow_aiops.rs`: incident upsert → check active change context → link incident to change → annotate SNOW incident with CHG ref. This round-trip has never been tested end-to-end.
- AIOps event push: `em_event` creation on SNOW PDI — S-65/S-68 are the only test steps for this.

### Tasks

**T1 — NetBox enrichment end-to-end test (S-60/S-61)**
- Run Phase 17 S-57 through S-61 on Ubuntu ops box with fresh ContainerLab deployment.
- Seed NetBox with lab device data using the S-61 seed script — verify script handles edge cases (missing device_type ID, site ID).
- Run enricher → verify `netbox_site`, `netbox_rack`, `netbox_role` properties written to Device nodes.
- Verify HostEndpoint nodes created for server-role devices with `CONNECTED_TO` edges.
- Document results in Ubuntu testing guide.

**T2 — NetBox bulk seed script hardening**
- The S-61 seed script uses `device_type:1, role:1, site:1` as hardcoded IDs — these are not guaranteed to exist in a fresh NetBox install.
- Rewrite: auto-create or discover `device_type`, `device_role`, `site` via NetBox API before creating devices.
- Add error handling: if device already exists → skip with "already exists" log.

**T3 — ServiceNow PDI enrichment end-to-end test (S-58/S-59/S-65)**
- Pre-requisite: active PDI is awake (PDIs hibernate after 10 days of inactivity — must be activated at `developer.servicenow.com`).
- Run S-58: verify PDI table API reachable.
- Run S-59: provision vault credentials (`snow-pdi` alias).
- Run S-65: register SNOW EM adapter via UI, test connection, save.
- Trigger detection → wait 60s → verify `em_event` appears in PDI via S-65 API check.
- Document exact PDI setup steps required (PDI activation, MID server not needed for table API).

**T4 — SNOW AIOps incident round-trip test**
- Create investigation for a real detection → investigation completes → SNOW incident upsert fires.
- Verify: `Incident` node in graph with `snow_incident_number`.
- If device is in an active ChangeRequest window → verify `RELATED_TO_CHANGE` edge created + work_note added to SNOW incident with CHG reference.
- This is the end-to-end test of `src/integrations/servicenow_aiops.rs` annotate path.

**T5 — Enrichment conflict UI test**
- Seed conflicting data: set `hostname = router-a` in CLI config; set `hostname = r-spine1` in NetBox for same device address.
- Open `DeviceDrawer.svelte` enrichment tab → verify conflict banner appears with `netbox` vs `cli` conflict for hostname field.
- Verify `GET /api/devices/{address}/enrichment/conflicts` returns the conflict.
- Verify provenance winner (`cli` takes precedence over `netbox` per `source_priority_rank()`).

**T6 — Adapter push audit completeness test (S-66/S-67/S-68)**
- Run S-66: verify push audit log shows entries for all 4 adapters (prom-lab, elastic-lab, splunk-lab, snow-em-lab).
- Run S-67: Grafana smoke test — verify `bonsai_interface_statistics_in_octets` metric appears.
- Run S-68: full integration round-trip — inject leaf4 BGP fault → wait → verify all 4 sinks received event.
- Document pass/fail for each step in Phase 17 checklist.

---

## D4-19 — End-to-End Clean-Slate Testing

### Analysis

The Ubuntu testing guide (S-00 through S-69) represents a structured test protocol but several phases have never been run fully from a clean state on a fresh Ubuntu install. Previous test sessions have accumulated: leftover ContainerLab state, pre-seeded credentials, pre-built binaries, and pre-loaded graph data. The clean-slate protocol from S-00 ("port check") through S-14 ("8 managed devices in graph") has been verified but the Phase 17 integration testing (S-57 through S-69) has never been run.

Key ⬜ (never run) and ⚠️ (partial) items from the full checklist:
- S-45/S-46 ⚠️: HostEndpoint LLDP inference — Alpine linux-host1 has no LLDP daemon → permanently ⚠️ until lab adds a Linux host with LLDP.
- S-49/S-50 ⚠️: mode=all collector registration — fixed in D4-9 T3.
- S-51 ⬜: Live UI 3-panel layout manual check — requires browser.
- S-56 ⬜: Teardown lab destroy — optional, skipped in previous runs.
- All of Phase 17 (S-57 through S-69) ⬜: never run.
- S-32b ⚠️: BMP PeerUp hold_time offset — fixed in D4-11 T1.
- S-33 ⚠️: BMP+gNMI cross-device correlation — architectural issue, D4-11 T5 ADR needed.
- S-29 ⚠️: SNMP BGP peer_address OID suffix — fixed in D4-1 T1.
- S-44 ⚠️: Detection dedup — fixed by D4-1 T2.
- S-38 ⚠️: CARRIES_FLOW managed device — fixed by D4-5 T1+T2.

### Tasks

**T1 — Clean-slate S-00 to S-56 run (all Phases 0-16)**
- On Ubuntu ops box: wipe `runtime/` directory, destroy any existing ContainerLab, clean `cargo build --release` from scratch.
- Execute every step S-00 through S-56 sequentially. Track ✅/❌ per step in a fresh results file.
- Expected outcome after D4-1/D4-5/D4-9/D4-11 fixes: S-29, S-38, S-44, S-49/S-50 should flip from ⚠️ to ✅.
- Screenshot evidence: S-51 (3-panel UI), S-52 (SSE stream), S-53 (round-trip fault injection).

**T2 — Phase 17 full run (S-57 to S-69)**
- Prerequisites: active ServiceNow PDI (verify not hibernated), Docker with external images available.
- Run all 13 steps of Phase 17 sequentially.
- Expected first-time results: some steps may fail due to NetBox seeding issues (T2 in D4-18), PDI connectivity, or adapter config gaps.
- Document every failure with root cause and fix applied.

**T3 — Screenshot + evidence capture automation** ✅ batch8
- Write a script `lab/signal-test-lab/capture_evidence.sh` that:
  - Runs key `curl` verification commands from the testing guide for S-12 through S-56.
  - Saves output to `test-results/run-{timestamp}/` with one file per step.
  - Summarises pass/fail to stdout and writes `test-results/run-{timestamp}/summary.md`.
- This replaces manual note-taking and enables CI-like test runs.

**T4 — Live UI smoke test coverage (S-51)**
- S-51 is ⬜ "manual — requires browser." Write a Playwright smoke test instead:
  - Opens `http://localhost:3000`.
  - Navigates to each route in NAV array (Live, Incidents, Explorer, Collectors, Integrations, Settings).
  - Checks each page renders without console errors and primary content is visible.
  - Saves screenshot per page to `test-results/screenshots/`.
- Add to `package.json` as `npm run test:smoke` script.

**T5 — Summary checklist integration**
- After each test run (from T1 evidence script + T4 Playwright), auto-generate the summary checklist table (format matching the existing table at the bottom of UBUNTU_TESTING_GUIDE.md).
- Fill ✅/❌ based on actual command output rather than manual marking.
- Produce a Markdown diff against the previous run showing regressions or improvements.

---

## D4-20 — Environment Data: Power, Temperature, Optics, Sensors

### Analysis

Today Bonsai collects only control-plane and data-plane telemetry: BGP/IS-IS/BFD state, interface counters, LLDP topology, flow data. No environmental/physical layer data is collected or modelled.

Environmental data is critical for:
- Pre-failure indicators: optics Rx power degradation before link drops, temperature trending toward thermal shutdown, PSU voltage out of tolerance.
- Root cause correlation: high CPU temperature → process restart → BGP session drop (the real cause was thermal, not a routing bug).
- Capacity planning: power draw per device, rack power budget utilisation.

**What gNMI paths are available (from `config/path_profiles/`):**
- Nokia SRL: `/platform` subtree includes `chassis/environment/temperature`, `/component[name=*]/oper-state`, `/component[name=*]/temperature`, `/port[port-id=*]/transceiver` (optics Rx/Tx power, wavelength, connector type).
- Cisco IOS-XR: `/openconfig-platform:components` with environment and transceiver subtrees.
- Arista EOS: `/interfaces/interface/state/counters` includes `in-discards`, `in-errors` but no optics path — requires `openconfig-platform` or proprietary path.
- FRR: no gNMI, no environmental telemetry path.

**Current graph schema:** No `SensorReading`, `OpticsTelemetry`, `PowerDomain`, or `ThermalZone` node types exist.

### Tasks

**T1 — Environmental telemetry schema** ✅ batch10
- `SensorReading` node: `device_address`, `component_name`, `sensor_type` (temperature/voltage/power/current/fan_speed), `value`, `unit`, `threshold_critical`, `threshold_warning`, `updated_at_ns`.
- `OpticsTelemetry` node: `device_address`, `interface_name`, `rx_power_dbm`, `tx_power_dbm`, `wavelength_nm`, `temperature_c`, `bias_current_ma`, `updated_at_ns`.
- `REPORTED_BY(SensorReading→Device)`, `OPTICS_ON(OpticsTelemetry→Interface)`.

**T2 — gNMI path profiles: environmental paths** ✅ batch10
- Add Nokia SRL environmental paths to `config/path_profiles/nokia-srlinux.yaml`:
  - `/platform/chassis/environment/temperature`
  - `/platform/component[name=*]/temperature`
  - `/interface[name=*]/transceiver/rx-power`, `/interface[name=*]/transceiver/tx-power`
- Add Cisco IOS-XR environmental paths (platform components).
- Add Arista EOS optics paths if available.

**T3 — gNMI telemetry writer: environmental + optics** ✅ batch13
- In `src/graph/mod.rs`: add `write_sensor_reading()` and `write_optics_telemetry()` functions.
- Parse incoming gNMI values from environmental paths → upsert `SensorReading` and `OpticsTelemetry` nodes.

**T4 — Environmental detection rules** ✅ batch13
- `temperature_threshold_warning`: `SensorReading.value > threshold_warning` → `low` severity.
- `temperature_threshold_critical`: `SensorReading.value > threshold_critical` → `high` severity.
- `optics_rx_power_low`: `OpticsTelemetry.rx_power_dbm < -25` (vendor threshold) → `medium` severity.
- `optics_rx_power_degrading`: Rx power dropped >3dB in 5 minutes (trending) → `medium` severity.
- `fan_failure`: fan RPM = 0 → `critical` severity.

**T5 — Environmental context in investigations** ✅ batch16
- Inject `SensorReading` and `OpticsTelemetry` data into investigation context (`investigation_runtime.rs`).
- LLM can then correlate: "Temperature on leaf3 chassis reached 68°C (warning threshold: 65°C) 5 minutes before the BGP session dropped — possible thermal-induced process restart."

**T6 — UI: Optics + sensor panel in DeviceDrawer** ✅ batch16
- New "Physical" tab in `DeviceDrawer.svelte`.
- Per-interface: Rx/Tx power with colour coding (green/amber/red vs vendor thresholds).
- Per-device: temperature graph (sparkline from TSDB query if configured, or latest value).
- PSU/fan status badges.

---

## D4-21 — Resource Governor UI Page

### Analysis

The resource governor (`src/resource_governor.rs`) implements three governance loops: memory pressure (5s poll, soft 80% / hard 95% of budget), write pressure (batch-size expansion), and rate shedding. It exposes `GovernanceSnapshot` with: `profile`, `memory_budget_mb`, `rate_budget_eps`, `memory_pressure_active`, `write_pressure_active`, `rate_shedding_active`, `memory_shrink_count`, `memory_flush_count`, `write_batch_expand_count`, `rate_shed_count`.

The existing `/api/governance` endpoint (in `src/http_server/governance.rs`) returns this snapshot. However:
- No UI page exists to display this information — operators have no live visibility into whether the system is under memory pressure or shedding events.
- The `GovernorHandle.register_memory_pressure_callback()` API exists but no feedback loop is wired for the UI to receive real-time pressure alerts.
- When `should_shed()` is true, ingest paths (syslog, BMP) drop low-priority bus publishes silently — operators have no way to know this is happening from the UI.
- Resource profiles (`low`/`standard`/`high`) are in TOML config — no UI to switch profiles at runtime.
- No historical graph of RSS, write queue depth, or event rate over time.

### Tasks

**T1 — Resource Governor UI page (new `src/routes/Governance.svelte`)** ✅ batch6
- Live status section: memory RSS gauge (current/budget), rate shedding active badge (green/red), write pressure active badge, memory pressure active badge.
- Counters section: memory_shrink_count, memory_flush_count, write_batch_expand_count, rate_shed_count — all from `/api/governance`.
- Auto-refresh every 5 seconds via polling (or SSE stream if governance events are added to event bus).

**T2 — SSE stream for governance events** ✅ batch9
- Add `GovernanceEvent` variant to the `SsePayload` enum or `BonsaiEvent` broadcast.
- Emit on: memory pressure transitions (none → soft → hard → clear), rate shedding start/stop, write pressure start/stop.
- UI: live event feed in Governance page showing transitions with timestamps: "14:32:01 — Memory pressure: SOFT (RSS 820 MB / 1024 MB budget)."

**T3 — Historical RSS + rate sparkline** ✅ batch6
- Maintain a ring buffer of last 60 RSS samples (5s × 60 = 5 minutes of history) in the governor.
- Expose via `/api/governance/history` as a time-series array.
- UI: mini sparkline charts in Governance page for: RSS over last 5 min, inbound event rate over last 5 min.

**T4 — Resource profile switcher** ✅ batch6
- `PATCH /api/governance/profile` with `{profile: "low"|"standard"|"high"}` → updates governor thresholds at runtime without restart.
- Requires reconfiguring memory_budget_bytes and rate_budget_eps from the new profile defaults.
- UI: profile selector radio buttons (Low / Standard / High) in Governance page. Shows current active profile with its memory budget and rate budget numbers.

**T5 — Shedding indicator in signal receivers** ✅ batch9
- When `should_shed()` is true in syslog receiver or BMP receiver, increment a per-receiver `shed_event_count` counter.
- Expose via `/api/receivers/status` alongside existing receiver stats.
- UI: in `Collectors.svelte` receiver badges, show a shed-events counter when non-zero. Tooltip: "Resource governor dropped N events due to memory pressure."

**T6 — Wire Governance page into App nav** ✅ batch6
- Add `{path: '/governance', label: 'Governance', icon: '⚖'}` to the Configure nav group in `App.svelte` NAV array.
- Guard with `admin` or `operator` role when RBAC is implemented (D4-3 T2).

---

## D4-22 — install.sh + Makefile + CI Pipeline Hardening

### Analysis

**`scripts/install.sh` (100 lines reviewed):**
- Detects platform (Linux/macOS), locates repo root, checks Docker availability, manages vault passphrase setup.
- Supports two paths: Docker Compose (recommended) or build-from-source.
- **Gaps**: No dependency version pinning (Rust toolchain version, Docker Compose version). No checksum verification on downloaded ContainerLab binary. No idempotency check — re-running on an existing install may leave stale state. No rollback capability.

**`Makefile` (61 lines reviewed):**
- Targets: `build`, `test`, `lint`, `ui`, `docker-start`, `docker-stop`, `clean`.
- **Gaps**: No `check-deps` target (verify Rust, Node, Docker, cmake, protobuf-compiler are present). No `install-deps` target for Ubuntu. No `test-integration` target distinct from unit tests. No `release` target that builds + packages for Ubuntu distribution. No CI workflow file (`.github/workflows/ci.yml` or similar) referenced or present.
- `make test` runs `cargo test` only — no Python test runner, no UI test runner.
- No database migration target.

**Build reproducibility:**
- `rust-toolchain.toml` may or may not exist — not verified. Without it, `cargo build` uses whatever Rust version is installed.
- `ui/package.json` exists but `package-lock.json` lockfile status unknown — npm install may resolve to different versions on different machines.

**Ubuntu-specific gaps (confirmed from testing guide):**
- Phase 0 build fix requires: `build-essential pkg-config libssl-dev clang cmake protobuf-compiler git curl wget jq python3 python3-pip python3-venv nodejs npm snmp`.
- `cmake` not on dev Mac — this breaks local builds. Documented in memories but not handled in `install.sh`.

### Tasks

**T1 — `rust-toolchain.toml` pin** ✅ batch8
- Create `rust-toolchain.toml` at repo root with `channel = "1.78.0"` (or current stable used in CI).
- Ensures reproducible Rust builds on all machines.

**T2 — `Makefile` hardening** ✅ batch8
- Add `check-deps` target: verify `rustc`, `cargo`, `node`, `npm`, `docker`, `cmake`, `protoc` are present and show versions.
- Add `install-deps-ubuntu` target: runs the `apt install` command from Phase 0 of the testing guide.
- Add `test-integration` target: runs `cargo test --test integration_tests` (if integration test files exist) separately from unit tests.
- Add `test-python` target: `cd python && python -m pytest tests/ -v`.
- Add `test-ui` target: `cd ui && npm run test:smoke` (from D4-19 T4 Playwright).
- Add `test-all` target: runs `test` + `test-python` + `test-ui` sequentially.
- Add `release` target: `cargo build --release` + `cd ui && npm run build` + create `dist/` tarball for Ubuntu distribution.
- Add `db-migrate` target: runs any pending graph schema migrations.

**T3 — `install.sh` hardening** ✅ batch15
- Add idempotency check: detect if Bonsai is already installed at target path, prompt for upgrade vs fresh install.
- Add dependency version checks before starting install: Rust ≥ 1.70, Docker ≥ 24.0, Docker Compose ≥ 2.20.
- Add ContainerLab install option (currently documented only in testing guide troubleshooting).
- Add rollback: if install fails, restore previous binary from backup.
- Add `--uninstall` flag: remove bonsai binary, service file, and (optionally) runtime data.

**T4 — GitHub Actions CI workflow** ✅ batch8
- Create `.github/workflows/ci.yml`.
- Jobs:
  - `build`: `cargo build --release` on Ubuntu 22.04.
  - `test`: `cargo test` + Python `pytest` on Ubuntu 22.04.
  - `lint`: `cargo clippy -- -D warnings` + `cargo fmt --check`.
  - `ui-build`: `cd ui && npm ci && npm run build`.
- Trigger: on push to main and on PR.
- Cache: `~/.cargo/registry`, `target/`, `ui/node_modules`.
- Fail fast: if `build` fails, skip `test`.

**T5 — `ui/package-lock.json` and dependency audit** ✅ batch15
- Verify `package-lock.json` is committed to the repo (enables reproducible `npm ci`).
- Run `npm audit` and document/fix any high-severity vulnerabilities.
- Pin major dependency versions: Svelte, Vite, D3 to avoid breaking changes on fresh installs.

---

## D4-23 — Ubuntu Testing Guide: Remaining Items + Phase 17 Completion

### Analysis

Full checklist status from the Ubuntu Testing Guide (S-00 through S-69):

**Already ✅ (confirmed working):**
S-25 (syslog multi-source fusion counter), S-26/S-27/S-28 (SNMP manual + linkDown + graph), S-30/S-31/S-32 (BMP session + ROUTE_MONITORING), S-34/S-35/S-36/S-37 (NetFlow softflowd), S-39/S-41/S-42 (OTLP curl + Application node + RUNS_SERVICE), S-43 (gNMI+syslog correlation), S-47/S-48 (Settings API streaming), S-52 (SSE stream), S-53 (round-trip fault injection), S-54/S-55 (teardown).

**⚠️ partial or known gap (to be resolved by other D4 epics):**
- S-29: SNMP BGP peer_address → fixed by D4-1 T1.
- S-32b: BMP PeerUp hold_time offset → fixed by D4-11 T1.
- S-33: BMP+gNMI cross-device correlation → ADR + design in D4-11 T5.
- S-38: CARRIES_FLOW managed device → sFlow receiver in D4-5 T1+T2.
- S-44: Detection dedup SNMP orphan → fixed by D4-1 T2.
- S-45/S-46: HostEndpoint LLDP from Alpine linux-host1 → requires LLDP daemon on host (lab topology change).
- S-49/S-50: mode=all collector registration → fixed by D4-9 T3.

**⬜ never run:**
- S-51: Live UI manual browser check → Playwright test in D4-19 T4.
- S-56: Lab teardown optional.
- All of Phase 17 (S-57 through S-69) → D4-18 and D4-19 cover these.

### Tasks

**T1 — Fix S-45/S-46: LLDP from linux-host1** ✅ batch13
- Alpine Linux does not include `lldpd` by default.
- Option A: modify `signal-test.clab.yml` to use an Ubuntu-based host image that includes `lldpd`.
- Option B: add `lldpd` install step to linux-host1 startup config.
- After fix: verify LLDP neighbor advertisement from host1 → leaf switch → `HostEndpoint` node with `CONNECTED_TO` edge written.
- This unblocks S-45 and S-46 from ⚠️ to ✅.

**T2 — Update testing guide: S-29, S-32b, S-38, S-44, S-49/S-50** ✅ batch15
- After implementing fixes from D4-1, D4-5, D4-9, D4-11: re-run each affected test step.
- Update status in the summary checklist table from ⚠️ to ✅.
- Add verification commands that specifically confirm the fix: e.g., for S-29, show that `peer_address` field is now populated in the snmp_fact graph node.

**T3 — Add Phase 18 to testing guide: Remediation Round-Trip** ✅ batch15
- New section after Phase 17 covering D4-15 T1 test scenario:
  - Prerequisites, inject fault steps, investigation trigger, Approvals UI check, approve remediation, verify execution, verify graph state after heal.
  - Same format as existing phases (numbered steps, expected results, troubleshooting section).

**T4 — Add Phase 19 to testing guide: Enrichment Quality Tests** ✅ batch15
- New section covering D4-18 scenarios:
  - NetBox enrichment round-trip (S-60/S-61 expanded with conflict test).
  - SNOW PDI connectivity + em_event verification (S-58/S-65 expanded).
  - Enrichment conflict UI verification.
  - AIOps incident annotation round-trip.

**T5 — Testing guide: sFlow steps (S-38b)** ✅ batch15
- After D4-5 T1+T2: add new test step S-38b to Phase 8 (NetFlow section):
  - Configure Nokia SRL to export sFlow → verify `CARRIES_FLOW(Device→AppFlow)` edge from an SRL managed device.
  - Expected: `exporter_address = 172.100.109.11` (srl-leaf1), `AppFlow` node with protocol/port data.

**T6 — Testing guide: Common Failure Patterns expansion** ✅ batch15
- Add new failure patterns section entries for:
  - sFlow not arriving: check sFlow config on SRL device, check port 6343 (sFlow default).
  - Investigation finds no data: check graph quality score `/api/graph/quality` for target device.
  - Remediation proposal not appearing: check investigation completed + `RemediationProposal` node in graph.
  - SNOW PDI not receiving events: verify PDI is not hibernated (must be active at developer.servicenow.com).
  - RBAC 403 errors (after D4-3): verify user role has required permission for the operation.

**T7 — Checklist tooling: auto-generate from test run** ✅ batch15
- From D4-19 T3: the `capture_evidence.sh` script collects curl output per step.
- Parse the output to auto-fill the summary checklist table: compare expected vs actual API responses.
- Generate `test-results/checklist-{timestamp}.md` with the filled-in table replacing the blank checkboxes.
- Diff against the checked-in checklist to show what improved or regressed since last run.

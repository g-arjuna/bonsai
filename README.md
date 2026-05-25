# bonsai

**Network observability and autonomous remediation for engineers who own the infrastructure.**

Bonsai collects streaming telemetry from network devices via gNMI, builds a live graph of your network state, detects anomalies with multi-source correlation, and can autonomously remediate faults — with a human-in-the-loop trust model that graduates to full autonomy only when you are ready.

---

## What bonsai does

```
gNMI   syslog   SNMP   BGP-LS   BMP   OTLP   NetFlow   sFlow   PCEP
  │       │       │       │       │      │       │        │       │
  └───────┴───────┴───────┴───────┴──────┴───────┴────────┴───────┘
                              │
                 ┌────────────▼────────────┐
                 │  Collector(s)            │  gNMI ON_CHANGE + SAMPLE
                 │  (mode=collector)        │  config-state diff
                 │  per-site, auto-restart  │  parser chain + CorrelationBuffer
                 └────────────┬────────────┘
                              │ gRPC (mTLS optional)
                 ┌────────────▼────────────┐
                 │  Core                    │  lbug embedded graph DB (56 node types)
                 │  (mode=core / all)       │  change detection (priority lane)
                 │                          │  enrichment + RBAC + HA (etcd)
                 └──┬──────────┬────────────┘
                    │          │
          ┌─────────▼──┐  ┌───▼──────────────┐
          │  REST/SSE   │  │  AI Agent         │
          │  API + UI   │  │  MCP tools +      │
          │  (Svelte 5) │  │  LLM loop         │
          └─────────┬──┘  └───────────────────┘
                    │
       ┌────────────▼──────────────────────────────┐
       │  Output Adapters (Integrations UI)          │
       │  Prometheus Remote Write · Splunk HEC       │
       │  Elasticsearch · ServiceNow Event Mgmt      │
       └────────────────────────────────────────────┘
                    │
       ┌────────────▼──────────────────────────────┐
       │  Remediation engine                         │
       │  suggest_only → approve_each                │
       │  → auto_with_notification → auto_silent     │
       └────────────────────────────────────────────┘
```

**Core capabilities:**
- **Graph database**: lbug (LadybugDB) — embedded, zero external dependencies, Cypher query interface
- **Telemetry sources**: gNMI (ON_CHANGE + SAMPLE), syslog (UDP + TCP), SNMP traps (v1/v2c/v3 USM), BGP-LS (via GoBGP sidecar), BMP (RFC 7854), PCEP, OTLP spans + metrics (HTTP), NetFlow v5/v9/IPFIX, sFlow v5 (RFC 3176)
- **Graph model (56 node types)**: Device, Interface, BgpNeighbor, BfdSession, IsisAdjacency, OspfNeighbor, Vrf, VLAN, StpInstance, NtpPeer, AclSummary, MplsLsp, AppFlow, Application, HostEndpoint, ComputeNode, SensorReading, OpticsTelemetry, PowerUnit, OpticalChannel, RedundancyGroup, BmpSession, BgpRibEntry, BgpLsNode, BgpLsLink, SrPolicy, Location, Rack, Prefix, Site, Environment, ConfigSnapshot, ConfigChange, ConfigItem, Investigation, Incident, ChangeRequest, and more
- **Multi-source correlation**: `CorrelationBuffer` deduplicates BGP/BFD/interface/OSPF/ISIS events across gNMI+syslog+SNMP within a 45-second window, keyed by `(device_address, semantic_type, sub_key)`
- **Change detection**: config-state diff, pyATS/Genie parser chain (17 learn features), native YANG parser — all on a dedicated priority write channel
- **Enrichment**: NetBox (3.x + 4.x auto-detected), ServiceNow CMDB (9 table fetches + provenance reconciliation), CLI scraping — with conflict detection and source-priority ranking (cli > netbox > servicenow)
- **Output adapters**: Prometheus Remote Write, Splunk HEC, Elasticsearch Bulk, ServiceNow Event Mgmt — each with customisable scheme/host/port/path
- **Remediation**: playbook-based, trust-graduated, rollback window, auto-proposal, graph-verified outcome
- **AI investigations**: MCP tool server + LLM agent loop (Gemini, Moonshot, OpenAI, Anthropic, Ollama) with structured RCA, operator feedback loop, per-investigation + daily budget caps
- **ML pipeline (EV1)**: STGNN (Spatio-Temporal GNN with GATv2 + GRU temporal encoder, 8-snapshot window), conformal prediction uncertainty bounds, NCT self-supervised pre-training, control-weighted loss for change-window awareness
- **Semantic embeddings**: syslog message embeddings (`all-MiniLM-L6-v2` / Ollama / OpenAI), device config embeddings, syslog cluster analysis (MiniBatchKMeans), detection reason clustering (HDBSCAN), PCA-compressed embedding injection into GNN feature vector
- **ML job engine**: APScheduler 4.x with SQLite job store — scheduled export, training, inference, embedding, clustering jobs with dependency chains, retry/dead-letter, progress SSE streaming
- **GNN**: graph neural network anomaly detection — accumulate → NCT pretrain → train → calibrate → promote. GNN inference results and attention weights written back to graph. Uncertainty-gated auto-investigation triggering.
- **Auth**: JWT session tokens, RBAC (admin/operator/viewer/api_readonly), LDAP/AD integration, scoped API keys
- **Syslog shunning**: per-device/per-category noise suppression with rate-limiting and regex-based rules
- **HA**: etcd-based leader election, config replication across core nodes
- **TSDB integration**: bidirectional Prometheus/Thanos/VictoriaMetrics/InfluxDB query proxy via `/api/tsdb/query`
- **Bootstrap agent**: PyATS/Genie-based device onboarding with topology-aware feature profiles (dc_leaf, dc_spine, campus_access, campus_core, sp_pe, homelab, etc.)

---

## Quick start — Docker (no lab required)

Requires: Docker 24+, Docker Compose v2.

```bash
git clone https://github.com/g-arjuna/bonsai && cd bonsai

# 1. Create your config
cp .env.example .env
# Edit .env — at minimum set BONSAI_VAULT_PASSPHRASE to a strong passphrase.

# 2. Start bonsai
docker compose --profile standalone up -d

# 3. Open the UI
open http://localhost:3000
```

On first launch, the UI shows the **Onboarding** wizard — create your environment, site, and first credential, then add your first device.

---

## Quick start — native binary (macOS / Linux)

Requires: Rust 1.95+ (pinned in `rust-toolchain.toml`), clang, cmake, protoc.

```bash
git clone https://github.com/g-arjuna/bonsai && cd bonsai

# Build
cargo build --release

# Build the UI (Svelte 5 + Vite)
cd ui && npm ci && npm run build && cd ..
cd ui-bonpy && npm ci && npm run build && cd ..

# Configure (minimal bootstrap — runtime tunables are managed via UI/API)
cp bonsai.toml.example bonsai.toml

export BONSAI_VAULT_PASSPHRASE="your-passphrase"
./target/release/bonsai --config bonsai.toml
# Add devices via the UI (Settings → Devices) or POST /api/onboarding/devices
```

The UI is served at `http_addr` (default `0.0.0.0:3000`). The Bonpy Python/ML dashboard is at `/bonpy/` on the same port. The gRPC ingest API is at `api_addr` (default `[::1]:50051` — use `0.0.0.0:50051` in Docker or distributed deployments).

---

## Docker profiles

| Profile | Command | Description |
|---|---|---|
| `standalone` | `docker compose --profile standalone up -d` | Single container, no ContainerLab. Best for first install. |
| `dev` | `docker compose --profile dev up -d` | Local dev with fast-iteration ContainerLab topology. |
| `distributed` | `docker compose --profile distributed up -d` | Core + two collectors on separate containers. |
| `cloud-dc` | `docker compose --profile cloud-dc up -d` | 6-node cloud DC topology (Nokia SRL). |
| `parsers` | append `--profile parsers` | Enables pyATS + native parser sidecars. |
| `streaming` | append `--profile streaming` | Enables GoBGP BGP-LS sidecar. |

---

## Configuration

Bonsai uses a **DB-first configuration model**. Only bootstrap settings (graph path, mode, credentials) live in `bonsai.toml`. All runtime tunables — retention, ingest, streaming, AI, remediation, logging, integrations, etc. — are stored in the embedded graph database and managed via the **Settings UI** or REST API.

### Bootstrap config (TOML)

```bash
cp bonsai.toml.example bonsai.toml
```

`bonsai.toml.example` is a minimal 67-line file with only the essentials:

```toml
graph_path = "runtime/bonsai.db"

[runtime]
mode = "all"    # "all" | "core" | "collector"

[credentials]
path = "bonsai-credentials"
passphrase_env = "BONSAI_VAULT_PASSPHRASE"
```

Devices are preferably added via the UI or `POST /api/devices`.

### Runtime config (DB-backed)

On first boot, YAML files under `config/` are migrated into the `ConfigItem` database table. Subsequent changes via the API or Settings UI are persisted to DB and override TOML defaults.

| API | Description |
|---|---|
| `GET /api/settings` | List all config sections with DB status |
| `GET /api/settings/{section}` | Read a section (DB value or TOML default) |
| `PATCH /api/settings/{section}` | Write a section to DB (512 KB limit) |
| `POST /api/settings/export` | Export all DB-stored config as JSON |

Managed sections: `retention`, `ingest`, `archive`, `storage`, `event_bus`, `remediation`, `gnn`, `logging`, `ai`, `lab`, `assignment`, `integrations_servicenow`, `integrations_tsdb`, `streaming`.

### Key environment variables

Most env vars now have DB or vault-first equivalents. Env vars serve as fallbacks:

| Variable | Required | Description |
|---|---|---|
| `BONSAI_VAULT_PASSPHRASE` | **Yes** | Passphrase for the encrypted credential vault. |
| `BONSAI_AI_API_KEY` | No | Default API key for AI investigations. Per-provider keys can also be stored in the vault via the UI (Settings → AI Providers). |
| `BONSAI_REQUIRE_AUTH` | No | Set to `1` to enforce RBAC. Also settable via DB (`app_config:require_auth`). |
| `BONSAI_ADMIN_USER` / `BONSAI_ADMIN_PASS` | No | Bootstrap admin credentials. Vault aliases `bonsai-admin` / `bonsai-admin-pass` take priority. |
| `BONSAI_YANG_BUNDLE_KEY` | No | Decryption key for the YANG model bundle (enterprise). |
| `BONSAI_COLLECTOR_DIAG_PASSWORD` | No | Auth for the collector diagnostic HTTP server. |
| `BONSAI_REQUIRE_SIDECAR` | No | Comma-separated sidecar kinds that must register before health reports `ok` (e.g. `collector-engine`). |
| `RUST_LOG` | No | Log filter — e.g. `info,bonsai=debug`. |

### Streaming receiver ports (configurable via UI or TOML)

All receiver ports are configurable. Change them live in **Settings → Streaming** — no restart required. Ports below 1024 require root or `cap_net_bind_service`.

| Receiver | Default port | Protocol | TOML key |
|---|---|---|---|
| gNMI | per device (e.g. `57400`) | gRPC | `[[target]]` |
| Syslog | `5514` (UDP), `6514` (TCP) | UDP + TCP | `[signals.syslog]` |
| SNMP traps | `9162` | UDP | `[signals.snmp]` |
| BMP | `5000` | TCP | `[streaming.bmp]` |
| BGP-LS | `15071` | TCP (GoBGP sidecar) | `[streaming.bgp_ls]` |
| NetFlow / IPFIX | `2055` | UDP | `[streaming.netflow]` |
| sFlow v5 | `6343` | UDP | `[streaming.sflow]` |
| OTLP | `4318` | HTTP | `[streaming.otlp]` |
| PCEP | `4189` | TCP | `[streaming.pcep]` |

---

## UI — navigation

The sidebar is grouped into three sections. Primary workspaces have `⌘1`–`⌘9` keyboard shortcuts. `Ctrl+K` opens the command palette.

### Monitor
| Workspace | Kbd | Description |
|---|---|---|
| **Live** | `⌘1` | 3-panel view: site rail · topology map (auto-tiered) · event stream + active incidents |
| **Incidents** | `⌘2` | Detection events with severity, rule, blast-radius chain, multi-source provenance |
| **Devices** | `⌘3` | Managed devices, per-device gNMI/streaming readiness, config history, CMDB tab, enrichment conflicts |
| **Operations** | `⌘4` | Daily health check, GNN calibration dashboard, weekly trend |
| **Collectors** | `⌘5` | Distributed collector status, queue depth, receiver badges |

### Operate
| Workspace | Kbd | Description |
|---|---|---|
| **Integrations** | `⌘6` | Enrichment sources (NetBox, ServiceNow CMDB) + output adapters (Prometheus, Splunk, Elastic, ServiceNow EM) with scheme/host/port/path fields |
| **Approvals** | `⌘7` | Pending remediation proposals, trust state per playbook, one-click approve/reject/rollback |
| **Explorer** | `⌘8` | Cypher query interface, saved queries, natural-language graph questions (AI), graph health tab with coverage scores |
| **Investigations** | `⌘9` | AI investigation records, reasoning trails, MCP tool calls, operator feedback, accuracy metrics |

### Configure
| Workspace | Description |
|---|---|
| **HA** | High availability status, leader/follower state, etcd config |
| **Environments** | Logical groupings (data_center, campus, cloud, service_provider) |
| **Profiles** | Per-environment archetype config, path profiles, chaos plans |
| **Sites** | Physical/logical site registry |
| **Credentials** | Encrypted vault — add tokens, passwords, API keys |
| **Audit** | Timestamped audit log of all remediation, approval, and config change events |
| **Syslog / Shun** | Syslog vendor pattern management, hot-reload, regex tester; shun rules for noise suppression |
| **Database** | DB stats, schema viewer, purge operations, backup/restore, export |
| **Governance** | Resource governor state, pressure history, profile switcher, RSS sparkline |
| **SNMP** | SNMP receiver config, OID pattern library, MIB upload + compile, v3 USM user management |
| **Sidecars** | Python sidecar registry, rule visibility + toggle, link to Bonpy dashboard (`/bonpy/`) |
| **Users & Access** | Local user management, RBAC role badges, LDAP settings + test connection |
| **Config Library** | Synthesizer rule library |
| **Settings** | Streaming receivers (hot-reload), AI providers, runtime config editor (14 DB-backed sections with JSON editor + export) |

---

## Credential vault

Bonsai encrypts all device credentials (gNMI passwords, SNMP community strings, API keys) in a local vault file (`runtime/vault.age`) using age encryption with an HMAC-SHA256 integrity tag.

### Passphrase requirements

- **Minimum 12 characters** — longer is better.
- Avoid dictionary words. Use a password manager or let `install.sh` auto-generate one.
- The passphrase is read from `BONSAI_VAULT_PASSPHRASE` on every startup.

### What happens if the passphrase is lost

Credentials are **unrecoverable**. The vault file cannot be decrypted without the original passphrase. Always keep a secure backup of the passphrase.

### Backup

Back up `runtime/vault.age` before any re-key operation:

```bash
cp runtime/vault.age runtime/vault.age.bak
```

### Re-keying (changing the passphrase)

**CLI** (offline):
```bash
BONSAI_VAULT_PASSPHRASE="old-pass" \
BONSAI_VAULT_NEW_PASSPHRASE="new-pass" \
  ./target/release/vault-rekey runtime/
```

**API** (while bonsai is running):
```bash
export BONSAI_VAULT_NEW_PASSPHRASE="new-pass"
curl -X POST http://localhost:3000/api/vault/rekey \
  -H 'Content-Type: application/json' \
  -d '{"new_passphrase_env": "BONSAI_VAULT_NEW_PASSPHRASE"}'
```

After re-key, update `BONSAI_VAULT_PASSPHRASE` in `.env` (or your shell profile) to the new passphrase before the next restart.

### Security features

- **zeroize**: Decrypted credentials are zeroed from memory on drop (`zeroize` crate).
- **Atomic write**: Vault persists via write-to-tmp + rename — crash-safe on POSIX.
- **HMAC-SHA256**: Integrity tag verified before decryption. Legacy vaults without HMAC gain one on next write.
- **Runtime directory**: `runtime/` directory is set to mode `700` at startup.

---

## Output adapter port customisation

Each output adapter exposes **four independent fields** in the Integrations UI:

| Field | Example | Notes |
|---|---|---|
| Scheme | `http` / `https` | TLS enforcement |
| Host / IP | `prometheus.internal` | DNS name or IP |
| **Port** | `9090` | Custom port, auto-filled per type |
| Path | `/api/v1/write` | Optional sub-path |

Changing the type (Prometheus / Splunk HEC / Elastic / ServiceNow EM) auto-fills the default port. The composed URL is shown as a live preview. The `endpoint_url` sent to the backend is always the canonical composed form.

**Default ports per adapter type:**

| Adapter | Default port | Scheme |
|---|---|---|
| Prometheus Remote Write | `9090` | http |
| Splunk HEC | `8088` | https |
| Elasticsearch | `9200` | http |
| ServiceNow Event Mgmt | `443` | https |

---

## API

REST API served on `http_addr`. gRPC ingest on `api_addr`.

OpenAPI schema: `GET /api/openapi.json` · Swagger UI: `GET /api/docs`

### Core endpoints

```
GET  /health                                  health + version + git SHA + sidecar status
GET  /healthz                                 Kubernetes liveness probe
GET  /readyz                                  Kubernetes readiness probe
```

### Topology & Observability

```
GET  /api/topology                            full topology graph (nodes + edges)
GET  /api/path?src=X&dst=Y                    shortest path between devices
GET  /api/blast-radius/{address}              blast radius analysis from a device
GET  /api/incidents                            detection events (grouped)
GET  /api/detections                          raw detection list
GET  /api/events                              SSE live event stream
GET  /api/events/history                      paginated event history
GET  /api/flows/live                          live flow data
GET  /api/graph/quality                       graph quality score + coverage dimensions
GET  /api/graph/insights                      graph topology insights
GET  /api/redundancy/groups                   redundancy group inventory
GET  /api/operations/daily-check              daily health summary
GET  /api/operations/weekly-trend             weekly trend data
GET  /api/operations/gnn-calibration          GNN calibration scores
```

### Devices & Onboarding

```
GET  /api/onboarding/devices                  list managed devices
POST /api/onboarding/devices                  add a managed device
POST /api/onboarding/devices/bulk             bulk add/remove devices
POST /api/onboarding/import                   bulk import from CSV/YAML
POST /api/onboarding/discover                 auto-discover device capabilities
POST /api/devices/bootstrap                   trigger PyATS bootstrap for a device
POST /api/devices/bootstrap/bulk              bulk PyATS bootstrap
POST /api/devices/seed                        seed device data (17 data types)
GET  /api/devices/{addr}                      device detail
GET  /api/devices/{addr}/gnmi-readiness       gNMI readiness check
GET  /api/devices/{addr}/streaming-readiness   multi-protocol readiness
GET  /api/devices/{addr}/config-history        config snapshot history
GET  /api/devices/{addr}/enrichment/conflicts  enrichment source conflicts
GET  /api/devices/{addr}/cmdb                  CMDB CI hierarchy + business services
GET  /api/devices/{addr}/sensors               sensor/environmental data
GET  /api/devices/{addr}/optics                optical telemetry
```

### Enrichment & Integrations

```
GET  /api/enrichment                          list enrichment sources
POST /api/enrichment                          upsert enrichment source
POST /api/enrichment/run                      trigger enricher immediately
GET  /api/enrichment/audit                    enrichment run history
GET  /api/adapters                            list output adapters
POST /api/adapters                            upsert output adapter
POST /api/adapters/test                       test adapter connectivity
GET  /api/adapters/audit                      adapter push history
GET  /api/tsdb/config                         TSDB integration config
GET  /api/tsdb/query                          proxy query to Prometheus/Thanos/InfluxDB
```

### AI & Investigations

```
POST /api/investigations                      create AI investigation
GET  /api/investigations/{id}                 investigation detail
GET  /api/investigations/{id}/tool-calls       MCP tool call log
POST /api/investigations/{id}/feedback         operator feedback (agree/disagree/comment)
GET  /api/investigations/accuracy              accuracy metrics over time
POST /api/ai/test                             test AI provider connectivity
GET  /api/ai/providers                        list configured AI providers
POST /api/ai/providers                        add/update AI provider (vault-backed keys)
```

### Remediation & Approvals

```
GET  /api/approvals                           list remediation proposals
POST /api/approvals/{id}/approve              approve a proposal
POST /api/approvals/{id}/reject               reject a proposal
POST /api/approvals/{id}/rollback             roll back an executed remediation
POST /api/remediations/{id}/verify            verify remediation outcome via graph
GET  /api/trust                               trust state per (rule, env, site, playbook)
POST /api/trust/graduate                      graduate trust level
GET  /api/playbooks                           playbook catalogue
GET  /api/audit                               audit log
```

### Auth & RBAC

```
POST /api/auth/login                          login (returns session token)
POST /api/auth/logout                         invalidate session
GET  /api/auth/users                          list users
POST /api/auth/users                          create user
GET  /api/auth/apikeys                        list scoped API keys
POST /api/auth/apikeys                        create scoped API key
GET  /api/auth/ldap/config                    LDAP configuration
POST /api/auth/ldap/test                      test LDAP connectivity
```

### Administration

```
GET  /api/settings                            list all config sections (DB status)
GET  /api/settings/{section}                  read a config section
PATCH /api/settings/{section}                 write a config section to DB
POST /api/settings/export                     export all DB-stored config as JSON
GET  /api/settings/streaming                  streaming receiver config
PATCH /api/settings/streaming                 update receiver config (hot-reload)
GET  /api/receivers/status                    live receiver status badges
GET  /api/config-items                        list ConfigItem records
POST /api/config-items                        upsert ConfigItem (validated)
GET  /api/shun/rules                          syslog shun rules
POST /api/shun/rules                          create shun rule
GET  /api/db/stats                            database size + row counts
GET  /api/db/schema                           graph schema introspection
POST /api/db/backup                           trigger database backup
GET  /api/ha/status                           HA cluster status + leader info
POST /api/snmp/mibs                           upload + compile MIB file
POST /api/explorer/query                      execute Cypher query
POST /api/explorer/ask                        natural-language graph question (AI)
POST /mcp                                     MCP tool server endpoint
```

### ML & GNN (EV1)

```
GET  /api/graph/snapshot                      live graph snapshot for STGNN inference
GET  /api/ml/exports                          Parquet export catalog
POST /api/ml/exports                          create export job record
PATCH /api/ml/exports/{id}                    update export status/metrics
GET  /api/ml/exports/quality                  quality summary across recent exports
GET  /api/ml/models                           model artifact registry
GET  /api/ml/models/active                    active model per type
POST /api/ml/models/{id}/activate             activate a model version
GET  /api/ml/lineage/{model_id}               full training lineage chain
GET  /api/ml/jobs                             job run history
POST /api/ml/jobs                             create job run record
PATCH /api/ml/jobs/{id}                       update job status/metrics
POST /api/ml/jobs/{id}/cancel                 request job cancellation
POST /api/ml/jobs/{id}/retry                  retry a dead-letter job
GET  /api/ml/schedules                        list scheduled jobs
POST /api/ml/schedules                        create or update a schedule
DELETE /api/ml/schedules/{id}                 remove a schedule
GET  /api/ml/events/stream                    SSE stream of ML events (job progress, GNN alerts)
POST /api/ml/events/publish                   publish ML event from Python sidecar
POST /api/gnn/inference-results               batch upsert GNN inference results from Python
GET  /api/gnn/inference-results               query inference history for a device
POST /api/gnn/attention                       batch upsert GNN attention snapshots
GET  /api/events/unembedded                   syslog events pending embedding
POST /api/events/embeddings                   batch upsert event embedding vectors
GET  /api/devices/unembedded-config           devices pending config embedding
POST /api/devices/{addr}/config-embedding     upsert device config embedding
GET  /api/ml/embeddings/stats                 embedding health summary
GET  /api/ml/similar-events                   semantic similarity search for events
GET  /api/sidecar/rules                       list Python rule IDs + enabled state
POST /api/sidecar/rules/{id}/toggle           enable/disable a rule
GET  /api/sidecar/rules/{id}/parameters       rule parameter overrides
PATCH /api/sidecar/rules/{id}/parameters      update rule parameters
POST /api/sidecar/rules/{id}/shadow-mode      toggle shadow mode
GET  /api/sidecar/rules/{id}/shadow-firings   shadow firing history
GET  /api/sidecar/rules/analytics             rule firing stats across time window
GET  /api/playbooks-v2                        DB-backed playbook list
POST /api/playbooks-v2                        create playbook
PUT  /api/playbooks-v2/{id}                   update playbook (version bump)
DELETE /api/playbooks-v2/{id}                 soft-delete playbook
GET  /api/playbooks-v2/{id}/executions        playbook execution history
GET  /api/syslog-rules                        syslog pattern rules from DB
POST /api/syslog-rules                        create syslog rule from UI
POST /api/syslog-rules/{id}/test              test regex against example message
```

---

## Enrichment

Managed entirely via **Integrations → Enrichment Sources** in the UI — no TOML editing required.

**NetBox** (3.x and 4.x, version auto-detected from `GET {base_url}/api/`):
- Pulls: devices, interfaces, VLANs, prefixes, rack/location assignments
- Writes: `netbox_*` properties on graph nodes + VLAN, Prefix, Rack, Location, **HostEndpoint** nodes
- Extra options: REST vs MCP transport, endpoint role list (classify APs/servers/phones as HostEndpoints), max concurrent requests

**ServiceNow CMDB** (full deep integration — 9 concurrent table fetches):
- **Phase 1**: business services (`cmdb_ci_business_service`), network CIs (`cmdb_ci_netgear`), CI relationships (`cmdb_rel_ci`), incidents
- **Phase 2**: servers (`cmdb_ci_server`), locations (`cmn_location`), subnets (`cmdb_ci_ip_network`), IP addresses (`cmdb_ci_ip_address`), network adapters (`cmdb_ci_network_adapter`)
- Writes: `snow_*` properties, `Application` nodes, `RUNS_SERVICE` edges, `CMDB_PARENT_OF` edges, `LOC_PARENT_OF` location hierarchy, `Incident` nodes
- Provenance reconciliation: source-priority ranking (`cli > netbox > servicenow`), conflict detection, `PropertyProvenance` nodes
- AIOps: incident upsert, playbook bridge (parse `bonsai:playbook <id>` from SNOW comments), change request correlation
- Paginated fetch with `sysparm_offset` (500 per page, up to 200 pages)

**Stub**: no-op enricher for CI pipelines.

Enrichers run on a configurable schedule (default 3600 s) or on-demand via UI or `POST /api/enrichment/run`.

---

## Distributed deployment

For multi-site or multi-collector setups:

```toml
# core node
[runtime]
mode = "core"

# each collector node
[runtime]
mode                 = "collector"
collector_id         = "site-a-col-1"
core_ingest_endpoint = "http://core.example.com:50051"
```

- Collectors poll `GET /api/settings/streaming` from Core every 60 s — streaming config is DB-backed and owned by Core.
- Optional mTLS on the collector→core channel via `[runtime.tls]` in `bonsai.toml`.
- Path profiles are migrated to the ConfigItem DB on first boot from `config/path_profiles/`.

---

## Remediation

Bonsai maps detection events to playbooks via a trust-graduated pipeline:

1. **`suggest_only`** — propose only, never execute (safe default)
2. **`approve_each`** — execute after human approval via UI or API
3. **`auto_with_notification`** — auto-execute, notify operator, rollback window open
4. **`auto_silent`** — fully autonomous (graduate to this deliberately)

Remediation settings are DB-backed (via `PATCH /api/settings/remediation`) with TOML fallback:

```toml
[remediation]
auto_propose = true   # auto-create a proposal for every detection
```

Playbooks live in `playbooks/library/`. Each has a `rule_id`, `steps` (gNMI SET or CLI), `risk_level`, and optional `rollback_steps`.

---

## ML pipeline (EV1)

Bonsai's ML pipeline is a full production MLOps stack — from raw graph snapshots to live anomaly detection with explainability and uncertainty bounds.

### Architecture

```
Graph snapshots (every 5 min via /api/graph/snapshot)
    └─► SnapshotBuffer (8-snapshot ring buffer, Arrow IPC on disk)
            └─► STGNN inference (GATv2 spatial → GRU temporal → anomaly score)
                    ├─► Conformal prediction (90% coverage uncertainty bound)
                    ├─► Attention weights → /api/gnn/attention (persisted to graph)
                    └─► /api/gnn/inference-results → investigation_trigger.rs
                                                        (uncertainty-gated auto-investigation)

Parquet export (incremental daily) → ReadinessCheck → NCT pretrain → supervised fine-tune
    └─► ModelArtifact registry → activate → inference loop picks up via /api/ml/models/active

Syslog events tagged needs_embedding=true → syslog_embedding_worker (60s batch)
    └─► EventEmbedding nodes → cosine similarity search for investigation context
    └─► SyslogClusterer (weekly, MiniBatchKMeans, 20 clusters)

Device configs (via bootstrap) → config_embedding_worker (6h) → DeviceConfigEmbedding
    └─► PCA 384→8 dims → injected into STGNN device feature vector (40 dims total)
```

### Pipeline stages

1. **Accumulate**: enable archive via `PATCH /api/settings/archive` (or `[archive] enabled = true`) for ≥7 days
2. **NCT pretrain**: `python -m bonsai_ml.gnn.nct` — self-supervised noise-contrastive training on topology structure. Noise curriculum: light (5% edge removal) → medium (15% + feature perturb) → heavy (30% + spurious edges). Requires ≥30 snapshots.
3. **Train**: `python python/train_stgnn.py --model-type stgnn` — supervised fine-tune on fault labels from chaos archive. Phase 1: NCT pretrain (loads `models/nct_pretrain.pt`). Phase 2: CrossEntropyLoss + CosineAnnealingLR + grad clip 1.0. Quality gate: AUC≥0.65 + F1≥0.40.
4. **Conformal calibration**: run automatically after training. Computes `q_hat` threshold from held-out calibration set. Saved to `models/conformal_qhat_alpha0.1.json`.
5. **Promote**: `POST /api/ml/models/{id}/activate` — or let the ML job engine auto-activate on quality gate pass.
6. **Infer**: STGNN inference loop runs every 5 min via `BonsaiJobEngine`. Results written to graph. Investigation auto-triggered on high-score + low-uncertainty (uncertainty_gate < 0.3).

### Automated job schedule (default)

| Job | Schedule | Description |
|-----|----------|-------------|
| `anomaly_export_daily` | `cron(hour=2)` | Incremental Parquet export, quality gated |
| `remediation_export_weekly` | `cron(day=0, hour=2)` | Full remediation training export |
| `gnn_inference` | `interval(5 min)` | Live STGNN inference → write-back |
| `syslog_embedding` | `interval(60 s)` | Batch embed pending syslog events |
| `graph_snapshot` | `interval(4 h)` | Capture snapshot for STGNN buffer |
| `detection_clustering` | `cron(day=0, hour=3)` | HDBSCAN cluster detection reasons |
| `config_embedding` | `interval(6 h)` | Embed device config text |

Schedules are managed via `POST /api/ml/schedules` or the BonPy UI at `/bonpy/jobs`.

### GNN architecture (EV1)

- **Spatial**: `HeteroGATv2Conv` (GAT v2, Brody et al. 2021) — eliminates rank collapse. 8 heads per layer.
- **Temporal**: per-node `GRU(hidden_channels, hidden_channels)` over 8-snapshot sequence.
- **Node types**: device (40 dims), interface (14 dims), bgp_neighbor (12 dims), bfd_session (10 dims), ospf_neighbor (8 dims), redundancy_group (6 dims), sensor_reading (4 dims).
- **Loss**: `FocalControlWeightedLoss` — focal loss (gamma=2.0) for class imbalance + change-weight=0.0 for fault samples during active ChangeRequest windows.
- **Uncertainty**: conformal prediction (primary, requires calibration set) or MC Dropout (fallback, `mc_dropout_samples=20`).

### Monitoring

- Health: `GET http://localhost:9200/health` — sidecar status, model loaded, inference times, queue depth, memory usage
- Metrics: `GET http://localhost:9201/metrics` — Prometheus metrics (job runs, parquet rows, AUC, pending embeddings, memory)
- BonPy UI: `http://localhost:3000/bonpy/` — full MLOps console (jobs, models, exports, GNN, embeddings, rules)

### Manual operations

```bash
# Export training data (incremental)
python -m bonsai_ml.export_job --type anomaly --incremental

# Train STGNN
python python/train_stgnn.py --model-type stgnn --register

# Start sidecar manually (systemd manages this in production)
python python/collector_engine.py

# Check sidecar health
curl http://localhost:9200/health | python3 -m json.tool

# View active model
curl http://localhost:3000/api/ml/models/active?type=stgnn
```

---

## Lab setup (ContainerLab)

For lab-backed testing with Nokia SRL or Cisco XRd:

```bash
# Fast-iteration 3-node SRL lab
cd lab/fast-iteration
./deploy.sh

# 6-node cloud DC topology
cd lab/dc
make deploy

# External integrations (Prometheus, Splunk, Elastic, NetBox)
cd docker
docker compose -f compose-signal-test.yml up -d
# ServiceNow: uses a real PDI (developer.servicenow.com), not a mock
```

Lab credentials are documented in `lab/signal-test-lab/UBUNTU_TESTING_GUIDE.md` (DV4 Phase 17). For EV1 ML pipeline testing see `docs/EV1_UBUNTU_TESTING_GUIDE.md`.

See `lab/` for topology YAML files and `scripts/` for seed scripts.

---

## Python SDK & Sidecars

```bash
pip install -e python/   # from repo root (local dev)
```

```python
from bonsai_sdk import BonsaiClient
from bonsai_sdk.detection import Detection, Features

client = BonsaiClient("http://localhost:50051")

# Create a detection from a Python rule
d = Detection(rule_id="bgp-all-peers-down", device_address="10.0.0.1",
              features=Features(peer_count=0, source_event_ids=["ev-123"]))
client.create_detection(d)
```

**Python components:**
- `python/bonsai_sdk/` — SDK: client, detection, streaming rules, windowing, rule loader, DB-backed rule overrides with hot-reload
- `python/bonsai_agent/` — AI investigation agent with budget control, `check_change_context` + `ask_graph` tools
- `python/bonsai_ml/` — Full EV1 ML pipeline:
  - `gnn/stgnn.py` — STGNN model (GATv2 spatial + GRU temporal, T=8 snapshots)
  - `gnn/nct.py` — NCT self-supervised pre-training with noise curriculum
  - `gnn/conformal.py` — conformal calibration + uncertainty-gated prediction
  - `gnn/loss.py` — FocalControlWeightedLoss (change-window aware)
  - `gnn/snapshot_store.py` — Arrow IPC snapshot buffer (disk-persistent)
  - `inference_client.py` — STGNN inference + write-back to Bonsai graph
  - `inference_loop.py` — `StgnnInferenceLoop` — 5-min APScheduler job
  - `job_engine.py` — `BonsaiJobEngine` (APScheduler 4.x + SQLite job store, full dependency chain)
  - `export_job.py` — `ParquetExportJob` with catalog integration + quality gate
  - `parquet_validator.py` — schema validation, class balance, PSI drift detection
  - `parquet_store.py` — versioned archive management with `latest` symlink
  - `text_embeddings.py` — `TextEmbedder` wrapping sentence-transformers / Ollama / OpenAI
  - `syslog_embedding_worker.py` — 60s batch embedder for syslog events
  - `config_embedding_worker.py` — 6h batch embedder for device configs
  - `syslog_cluster.py` — `SyslogClusterer` (MiniBatchKMeans, 20 clusters, weekly)
  - `detection_clustering.py` — HDBSCAN cluster detection reasons
  - `memory_manager.py` — `MlMemoryManager` with LRU model cache + RSS bounds
  - `snapshot_client.py` — `GraphSnapshotClient` (fetch + convert `/api/graph/snapshot`)
- `python/bootstrap_agent.py` — PyATS/Genie device bootstrap (17 learn features: interfaces, BGP, BFD, OSPF, STP, VLAN, VRF, NTP, ACL, MPLS, platform, routing, ARP, HSRP, config, LLDP, MAC table)
- `python/collector_engine.py` — Python rule engine sidecar: non-blocking startup, graceful SIGTERM, forward queue backpressure, health on `:9200`, Prometheus on `:9201`
- `python/train_stgnn.py` — standalone STGNN training script (NCT pretrain → supervised finetune → quality gate → register)
- `docker/sidecars/pyats/app.py` — PyATS sidecar with Genie (primary) + TextFSM fallback, 17 features, topology-aware profiles

### BonPy MLOps Console

The BonPy UI is a SvelteKit app served at `/bonpy/` on the same port as the main Bonsai UI. No separate port.

| Route | Description |
|-------|-------------|
| `/bonpy/` | Dashboard — sidecar health, GNN status, parquet freshness, next jobs |
| `/bonpy/jobs` | Job scheduler — cron table, live progress (SSE), dead-letter, manual trigger |
| `/bonpy/models` | Model registry — activate, compare, lineage, model card |
| `/bonpy/exports` | Parquet catalog — quality report, class balance, drift, manual trigger |
| `/bonpy/gnn` | GNN live feed — inference timeline, anomaly scores, attention mini-viz |
| `/bonpy/embeddings` | Embedding health — pending counts, cluster explorer, UMAP projection |
| `/bonpy/rules` | Rule management — enable/disable, parameters, shadow mode, playbooks |
| `/bonpy/detections` | Detection stream — SSE-driven, ML annotations, GNN score overlay |

```bash
# Build BonPy UI
cd ui-bonpy && npm ci && npm run build

# Dev mode (proxies /api/ to localhost:3000)
cd ui-bonpy && npm run dev
```

---

## Development

```bash
# Rust (full suite — requires cmake + protoc)
cargo build
cargo test --workspace

# Bonsai UI — dev server (Svelte 5 + Vite)
cd ui && npm ci && npm run dev
# Production build
cd ui && npm run build

# Bonpy UI — Python/ML/AIOps dashboard (Svelte 5 + Vite)
cd ui-bonpy && npm ci && npm run dev   # dev: proxies /api to localhost:3000
cd ui-bonpy && npm run build           # prod: built assets served at /bonpy/

# Python SDK
cd python && pip install -e ".[dev]"
pytest

# Playwright smoke tests (requires running bonsai instance)
cd ui && npm run test:smoke
```

See `docs/DEVELOPMENT.md` for full dev environment setup including ContainerLab, NetBox, and the parser sidecar stack.

---

## Architecture decisions

See `DECISIONS.md` and `docs/architecture/` for rationale behind key choices:

- **lbug (LadybugDB)** — embedded graph database, zero runtime deps, Cypher query interface
- **Rust binary + Python sidecar** — telemetry + graph in Rust; ML + rules in Python
- **Priority write channel** — detection/remediation events never queued behind telemetry batches (biased `select` on separate mpsc channel)
- **CorrelationBuffer** — 45 s multi-source dedup keyed by `(device, semantic_type, sub_key)` with 10 s sweep
- **MCP server** — AI agent uses read-only tool calls (graph query, incident fetch, topology, blast radius)
- **Two-binary model** — `bonsai` (main process) + `healthcheck` (container liveness probe) + `vault-rekey` (offline re-key) + `query` (CLI graph query)
- **Trust model** — `suggest_only → approve_each → auto_with_notification → auto_silent` with per-environment defaults
- **HostEndpoint node** — arch-agnostic, optional; SP deployments = zero HostEndpoints
- **PyATS-first bootstrap** — see `docs/architecture/adr_suzieq_vs_pyats.md`
- **Cross-device correlation** — see `docs/architecture/adr_cross_device_correlation.md`

---

## Project structure

```
src/                  Rust codebase (Axum HTTP, gRPC, graph, collectors, signals)
ui/                   Bonsai UI (Svelte 5, served at /)
ui-bonpy/             Bonpy Python/ML dashboard (Svelte 5, served at /bonpy/)
python/               Python SDK, bootstrap agent, collector engine, ML pipeline
proto/                Protobuf definitions (bonsai_service.proto, gnmi.proto)
config/               Path profiles, syslog patterns, SNMP OID patterns, shun seeds (migrated to DB on first boot)
playbooks/            Remediation playbook library
lab/                  ContainerLab topologies (DC, SP, fast-iteration, cloud-DC, signal-test)
docker/               Dockerfiles, compose configs, Grafana/Prometheus dashboards, sidecars
scripts/              Operational scripts (cloud, dev, lab, ops)
docs/                 Architecture docs, ADRs, development guide
tests/                CLI fixtures, chaos harness, API/event drivers
```

---

## Contributing

1. Fork and branch from `main`.
2. `cargo test --workspace` must pass.
3. `cd ui && npm run build` and `cd ui-bonpy && npm run build` must succeed.
4. New API endpoints must update `src/http_server/schema.rs`.
5. New UI routes must be added to `NAV` in `ui/src/App.svelte` and `WORKSPACE_SHORTCUTS` in `CommandPalette.svelte`.
6. New RBAC-protected endpoints must call `required_role()` middleware.

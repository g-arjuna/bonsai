# bonsai

**Network observability and autonomous remediation for engineers who own the infrastructure.**

Bonsai collects streaming telemetry from network devices via gNMI, builds a live graph of your network state, detects anomalies with multi-source correlation, and can autonomously remediate faults — with a human-in-the-loop trust model that graduates to full autonomy only when you are ready.

---

## What bonsai does

```
gNMI   syslog   SNMP   BGP-LS   BMP   OTLP   NetFlow   PCEP
  │       │       │       │       │      │       │        │
  └───────┴───────┴───────┴───────┴──────┴───────┴────────┘
                              │
                 ┌────────────▼────────────┐
                 │  Collector(s)            │  gNMI ON_CHANGE + SAMPLE
                 │  (mode=collector)        │  config-state diff
                 │  per-site, auto-restart  │  parser chain + CorrelationBuffer
                 └────────────┬────────────┘
                              │ gRPC (mTLS optional)
                 ┌────────────▼────────────┐
                 │  Core                    │  lbug embedded graph DB
                 │  (mode=core / all)       │  change detection (priority lane)
                 │                          │  enrichment + HostEndpoint model
                 └──┬──────────┬────────────┘
                    │          │
          ┌─────────▼──┐  ┌───▼──────────────┐
          │  REST/SSE   │  │  AI Agent         │
          │  API + UI   │  │  MCP tools +      │
          │  (Svelte)   │  │  LLM loop         │
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
- **Graph database**: lbug (LadybugDB) — embedded, zero external dependencies
- **Telemetry sources**: gNMI (ON_CHANGE + SAMPLE), syslog, SNMP traps, BGP-LS, BMP, PCEP, OTLP spans, NetFlow v5/v9/IPFIX
- **Graph model**: Device, Interface, BgpNeighbor, VRF, AppFlow, Application, **HostEndpoint**, VLAN, Prefix, Rack, Location nodes
- **Multi-source correlation**: `CorrelationBuffer` deduplicates BGP/BFD/interface/OSPF/ISIS events across gNMI+syslog+SNMP within a 45-second window
- **Change detection**: config-state diff, pyATS/Genie parser chain, native YANG parser — all on a dedicated priority write channel
- **Enrichment**: NetBox (3.x + 4.x auto-detected), ServiceNow CMDB, CLI scraping — managed via unified **Integrations** UI
- **Output adapters**: Prometheus Remote Write, Splunk HEC, Elasticsearch Bulk, ServiceNow Event Mgmt — each with customisable scheme/host/**port**/path
- **Remediation**: playbook-based, trust-graduated, rollback window, auto-proposal
- **AI investigations**: MCP tool server + LLM agent loop (Gemini, Moonshot, Anthropic, OpenAI)
- **GNN**: graph neural network anomaly detection — accumulate → train → calibrate → promote

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

Requires: Rust 1.82+, clang, cmake.

```bash
git clone https://github.com/g-arjuna/bonsai && cd bonsai

# Build
cargo build --release

# Configure
cp bonsai.toml.example bonsai.toml
# Edit bonsai.toml: set graph_path, http_addr, and add [[target]] blocks.

export BONSAI_VAULT_PASSPHRASE="your-passphrase"
./target/release/bonsai --config bonsai.toml
```

The UI is served at `http_addr` (default `0.0.0.0:3000`). The gRPC ingest API is at `api_addr` (default `0.0.0.0:50051`).

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

All configuration lives in `bonsai.toml`. Copy and edit the reference file:

```bash
cp bonsai.toml.example bonsai.toml
```

`bonsai.toml.example` documents every field with its default and when to change it.

### Minimum required fields

```toml
graph_path = "runtime/bonsai.db"
http_addr  = "0.0.0.0:3000"
api_addr   = "0.0.0.0:50051"

[[target]]
address          = "10.0.0.1:57400"
hostname         = "leaf-01"
credential_alias = "lab-gnmi"    # add via UI → Credentials
```

### Key environment variables

| Variable | Required | Description |
|---|---|---|
| `BONSAI_VAULT_PASSPHRASE` | **Yes** | Passphrase for the encrypted credential vault. |
| `BONSAI_AI_API_KEY` | No | API key for the AI investigation engine. Set to a Gemini (Google AI Studio) or Moonshot key. If unset, AI investigations are silently disabled. The env var name is configurable via `[ai] api_key_env`. |
| `BONSAI_YANG_BUNDLE_KEY` | No | Decryption key for the YANG model bundle (enterprise). |
| `BONSAI_COLLECTOR_DIAG_PASSWORD` | No | Auth for the collector diagnostic HTTP server. |
| `RUST_LOG` | No | Log filter — e.g. `info,bonsai=debug`. |

### Streaming receiver ports (configurable via UI or TOML)

All receiver ports are configurable. Change them live in **Settings → Streaming** — no restart required.

| Receiver | Default port | Protocol |
|---|---|---|
| gNMI | `57400` (per device) | gRPC |
| Syslog | `10514` | UDP/TCP |
| SNMP traps | `10162` | UDP |
| BMP | `10179` | TCP |
| BGP-LS | `10179` | TCP (GoBGP sidecar) |
| NetFlow / IPFIX | `2055` | UDP |
| OTLP gRPC | `4317` | gRPC |
| PCEP | `4189` | TCP |

---

## UI — navigation

The sidebar is grouped into three sections. All primary workspaces have `⌘1`–`⌘9` keyboard shortcuts.

### Monitor
| Workspace | Kbd | Description |
|---|---|---|
| **Live** | `⌘1` | 3-panel view: site rail · topology map (auto-tiered) · event stream + active incidents |
| **Incidents** | `⌘2` | Detection events with severity, rule, blast-radius chain, multi-source provenance |
| **Devices** | `⌘3` | Managed devices, per-device gNMI readiness, config history, HostEndpoint graph |
| **Operations** | `⌘4` | Daily health check, GNN calibration dashboard, weekly trend |
| **Collectors** | `⌘5` | Distributed collector status, queue depth, receiver badges |

### Operate
| Workspace | Kbd | Description |
|---|---|---|
| **Integrations** | `⌘6` | Unified: enrichment sources (NetBox, ServiceNow) + output adapters (Prometheus, Splunk, Elastic, ServiceNow EM). Each adapter has dedicated **scheme / host / port / path** fields. |
| **Approvals** | `⌘7` | Pending remediation proposals, trust state per playbook, one-click approve/reject |
| **Explorer** | `⌘8` | Raw Cypher graph query interface |
| **Investigations** | `⌘9` | AI investigation records, reasoning trails, MCP tool calls |

### Configure
| Workspace | Description |
|---|---|
| **Environments** | Logical groupings (data_center, campus, cloud, service_provider) |
| **Profiles** | Per-environment archetype config, path profiles, chaos plans |
| **Sites** | Physical/logical site registry |
| **Credentials** | Encrypted vault — add tokens, passwords, API keys |
| **Settings** | Streaming receiver toggle + port config (hot-reload, no restart) |

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

OpenAPI schema: `GET /openapi.json`

```
GET  /health                                  health + version + git SHA
GET  /api/devices                             list managed devices
POST /api/devices                             add / update a device
GET  /api/devices/{addr}/gnmi-readiness       readiness check
GET  /api/incidents                           detection events
GET  /api/approvals                           remediation proposals
POST /api/approvals/{id}/approve              approve a proposal
POST /api/approvals/{id}/reject               reject a proposal
POST /api/enrichment/{name}/run               trigger enricher immediately
GET  /api/enrichment/{name}/audit             enrichment run history
GET  /api/adapters                            list output adapters
POST /api/adapters                            upsert output adapter
POST /api/adapters/{name}/test                test adapter connectivity
GET  /api/adapters/audit                      adapter push history
POST /api/investigations                      create AI investigation
GET  /api/operations/daily-check              health summary
GET  /api/settings/streaming                  get streaming receiver config
PATCH /api/settings/streaming                 update streaming receiver config
GET  /events                                  SSE stream (live events)
GET  /api/events/history                      paginated event history
```

---

## Enrichment

Managed entirely via **Integrations → Enrichment Sources** in the UI — no TOML editing required.

**NetBox** (3.x and 4.x, version auto-detected from `GET {base_url}/api/`):
- Pulls: devices, interfaces, VLANs, prefixes, rack/location assignments
- Writes: `netbox_*` properties on graph nodes + VLAN, Prefix, Rack, Location, **HostEndpoint** nodes
- Extra options: REST vs MCP transport, endpoint role list (classify APs/servers/phones as HostEndpoints), max concurrent requests

**ServiceNow CMDB**:
- Reads CI records from a configurable table (default `cmdb_ci_netgear`)
- Writes: `snow_*` properties on graph Device nodes

**Stub**: no-op enricher for CI pipelines.

Enrichers run on a configurable schedule (default 3600 s) or on-demand via UI or `POST /api/enrichment/{name}/run`.

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

- Collectors poll `GET /api/settings/streaming` from Core every 60 s — streaming config is owned by Core.
- Optional mTLS on the collector→core channel via `[runtime.tls]`.
- Per-collector path profiles and per-site gNMI targets live in `config/path_profiles/`.

---

## Remediation

Bonsai maps detection events to playbooks via a trust-graduated pipeline:

1. **`suggest_only`** — propose only, never execute (safe default)
2. **`approve_each`** — execute after human approval via UI or API
3. **`auto_with_notification`** — auto-execute, notify operator, rollback window open
4. **`auto_silent`** — fully autonomous (graduate to this deliberately)

```toml
[remediation]
auto_propose = true   # auto-create a proposal for every detection
```

Playbooks live in `playbooks/library/`. Each has a `rule_id`, `steps` (gNMI SET or CLI), `risk_level`, and optional `rollback_steps`.

---

## GNN anomaly detection

Graph Neural Network pipeline for unsupervised anomaly detection:

1. **Accumulate**: run with `[archive] enabled = true` for 7–30 days
2. **Train**: `python python/bonsai_ml/gnn/train_anomaly.py`
3. **Calibrate**: set `[gnn] inference_mode = "calibration"` — review score distribution via **Operations → GNN Calibration**
4. **Promote**: set `inference_mode = "production"` when P95 score is below your threshold

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

# External integrations (Prometheus, Splunk, Elastic, ServiceNow mock, NetBox)
cd docker
docker compose -f compose-signal-test.yml up -d
```

Lab credentials are documented in `lab/signal-test-lab/UBUNTU_TESTING_GUIDE.md` (Phase 17).

See `lab/` for topology YAML files and `scripts/` for seed scripts.

---

## Python SDK

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

See `python/bonsai_sdk/` for the full SDK. `python/bonsai_agent/` contains the AI investigation agent. `python/bonsai_ml/` contains the GNN pipeline.

---

## Development

```bash
# Rust (full suite)
cargo build
cargo test --workspace

# UI — dev server (Svelte 5 + Vite)
cd ui && npm install && npm run dev
# Production build
cd ui && npm run build

# Python SDK
cd python && pip install -e ".[dev]"
pytest

# Svelte compile check (zero-error gate)
cd ui && node -e "
  const s = require('svelte/compiler'), fs = require('fs');
  const f = process.argv[1];
  const r = s.compile(fs.readFileSync(f,'utf8'), {generate:'client',filename:f});
  console.log(r.warnings.filter(w=>w.code!='a11y_autofocus').length,'warnings');
" src/routes/Integrations.svelte
```

See `docs/DEVELOPMENT.md` for full dev environment setup including ContainerLab, NetBox, and the parser sidecar stack.

---

## Architecture decisions

See `DECISIONS.md` for the rationale behind key choices:

- **lbug (LadybugDB)** — embedded graph database, zero runtime deps, Cypher query interface
- **Rust binary + Python sidecar** — telemetry + graph in Rust; ML + rules in Python
- **Priority write channel** — detection/remediation events never queued behind telemetry batches
- **CorrelationBuffer** — 45 s multi-source dedup keyed by `(device, semantic_type, sub_key)`
- **MCP server** — AI agent uses read-only tool calls (graph query, incident fetch, topology)
- **Two-binary model** — `bonsai` (main process) + `healthcheck` (container liveness probe)
- **Trust model** — `suggest_only → approve_each → auto_with_notification → auto_silent`
- **HostEndpoint node** — arch-agnostic, optional; SP deployments = zero HostEndpoints

---

## Contributing

1. Fork and branch from `main`.
2. `cargo test --workspace` must pass.
3. `cd ui && npm run build` must succeed with zero Svelte compile errors.
4. New API endpoints must update `src/http_server/schema.rs`.
5. New UI routes must be added to `NAV` in `ui/src/App.svelte` and `WORKSPACE_SHORTCUTS` in `CommandPalette.svelte`.

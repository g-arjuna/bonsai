# bonsai

**Network observability and autonomous remediation for engineers who own the infrastructure.**

Bonsai collects streaming telemetry from network devices via gNMI, builds a live graph of your network state, detects anomalies, and can autonomously remediate faults — with a human-in-the-loop trust model that graduates to autonomy only when you are ready.

---

## What bonsai does

```
gNMI telemetry   syslog   SNMP traps   BGP-LS   BMP
       │              │         │          │       │
       └──────────────┴─────────┴──────────┴───────┘
                              │
                    ┌─────────▼──────────┐
                    │  Collector(s)       │  gNMI subscribe + GET
                    │  (bonsai binary,    │  config-state diff
                    │   mode=collector)   │  parser chain
                    └─────────┬──────────┘
                              │ gRPC (mTLS optional)
                    ┌─────────▼──────────┐
                    │  Core               │  lbug graph DB
                    │  (bonsai binary,    │  change detection
                    │   mode=core / all)  │  enrichment
                    └──┬──────┬──────────┘
                       │      │
              ┌────────▼─┐  ┌─▼──────────────┐
              │  REST/    │  │  AI Agent       │
              │  SSE API  │  │  (MCP tools +   │
              │  + UI     │  │   LLM loop)     │
              └────────┬──┘  └─────────────────┘
                       │
              ┌────────▼──────────────────────┐
              │  Remediation engine            │
              │  suggest_only → approve_each   │
              │  → auto_with_notification      │
              │  → auto_silent                 │
              └───────────────────────────────┘
```

- **Graph database**: lbug (LadybugDB) — embedded, no external dependencies
- **Telemetry**: gNMI subscribe (ON_CHANGE + SAMPLE), syslog, SNMP traps, BGP-LS, BMP, PCEP, OTLP, NetFlow
- **Change detection**: per-device config diff + pyATS/Genie + native parser chain
- **Enrichment**: NetBox (3.x + 4.x), ServiceNow CMDB, CLI config scraping
- **Remediation**: playbook-based, trust model with graduation, rollback window
- **AI investigations**: MCP tool server + LLM agent loop (Gemini, Moonshot, Anthropic, OpenAI)
- **GNN**: graph neural network anomaly detection, calibration → production pipeline

---

## Quick start — Docker (no lab required)

Requires: Docker 24+, Docker Compose v2.

```bash
git clone https://github.com/your-org/bonsai && cd bonsai

# 1. Create your .env
cp .env.example .env
# Edit .env — at minimum, set BONSAI_VAULT_PASSPHRASE to any strong passphrase.

# 2. Start bonsai
docker compose --profile standalone up -d

# 3. Open the UI
open http://localhost:3000
```

The `standalone` profile uses a plain Docker bridge network — no ContainerLab needed.
Add devices via the **Onboarding** wizard in the UI.

---

## Quick start — native binary (macOS / Linux)

Requires: Rust 1.82+, clang, cmake.

```bash
git clone https://github.com/your-org/bonsai && cd bonsai

# Build
cargo build --release

# Configure
cp bonsai.toml.example bonsai.toml
# Edit bonsai.toml: set graph_path, add [[target]] blocks for your devices.

export BONSAI_VAULT_PASSPHRASE="your-passphrase"
./target/release/bonsai --config bonsai.toml
```

The UI is served at `http://localhost:3000` (port embedded in `api_addr`).

---

## Docker profiles

| Profile | Command | Description |
|---|---|---|
| `standalone` | `docker compose --profile standalone up -d` | Single container, no ContainerLab. Best for first install. |
| `dev` | `docker compose --profile dev up -d` | Local dev with fast-iteration ContainerLab topology. |
| `distributed` | `docker compose --profile distributed up -d` | Core + two collectors on separate containers. |
| `cloud-dc` | `docker compose --profile cloud-dc up -d` | 6-node cloud DC topology. |
| `parsers` | add `--profile parsers` to any above | Enables pyATS + native parser sidecars. |
| `streaming` | add `--profile streaming` | Enables GoBGP BGP-LS sidecar. |

---

## Configuration

All configuration lives in `bonsai.toml`. Copy and edit the reference file:

```bash
cp bonsai.toml.example bonsai.toml
```

`bonsai.toml.example` documents every field with its default value and when to change it.

### Minimum required fields

```toml
graph_path = "runtime/bonsai.db"

[[target]]
address          = "10.0.0.1:57400"
hostname         = "leaf-01"
credential_alias = "lab-gnmi"    # added via UI -> Credentials
```

### Key environment variables

| Variable | Description |
|---|---|
| `BONSAI_VAULT_PASSPHRASE` | **Required.** Passphrase for the encrypted credential vault. |
| `BONSAI_YANG_BUNDLE_KEY` | Optional. Decryption key for the YANG model bundle. |
| `BONSAI_COLLECTOR_DIAG_PASSWORD` | Optional. Auth for the collector diagnostic HTTP server. |
| `RUST_LOG` | Log filter (e.g. `info,bonsai=debug`). |

---

## UI workspaces

| Workspace | Path | Description |
|---|---|---|
| **Live** | `/` | Real-time event stream, topology map, active incidents |
| **Incidents** | `/incidents` | Detection events table with severity, rule, blast radius |
| **Devices** | `/devices` | Managed device list, per-device gNMI status, config history |
| **Operations** | `/operations` | Daily health check, GNN calibration, weekly trend |
| **Collectors** | `/collectors` | Distributed collector status and queue depth |
| **Enrichment** | `/enrichment` | Enricher status, run-on-demand, per-device property inspector |
| **Adapters** | `/adapters` | Output adapter status (ServiceNow, Splunk, Elastic) |
| **Approvals** | `/approvals` | Pending remediation proposals, trust state per playbook |
| **Explorer** | `/explorer` | Raw graph query interface (Cypher) |
| **Investigations** | `/investigations` | AI investigation records and reasoning trails |
| **Credentials** | `/credentials` | Encrypted vault management |
| **Sites / Environments / Profiles** | various | Topology grouping, archetype config, path profiles |

---

## API

The REST API is served on the same port as the UI (`api_addr` in `bonsai.toml`, default `:50051`).

OpenAPI schema: `GET /openapi.json`

Key endpoint groups:

```
GET  /health                               -- health + version info
GET  /api/devices                          -- list managed devices
POST /api/devices                          -- add a device
GET  /api/devices/{addr}/gnmi-readiness    -- gNMI readiness check
POST /api/enrichment/{name}/run            -- trigger an enricher
GET  /api/incidents                        -- detection events
GET  /api/approvals                        -- remediation proposals
POST /api/approvals/{id}/approve           -- approve a proposal
POST /api/approvals/{id}/reject            -- reject a proposal
POST /api/investigations                   -- create an AI investigation
GET  /api/operations/daily-check           -- health summary
GET  /events                               -- SSE stream of live events
```

---

## Python SDK

```bash
pip install bonsai-sdk
```

```python
from bonsai_sdk import BonsaiClient

client = BonsaiClient("http://localhost:50051")
detections = client.list_incidents()
```

See `python/bonsai_sdk/` for the full SDK and `python/bonsai_agent/` for the AI agent.

---

## Distributed deployment

For multi-site or multi-collector setups, run bonsai in split mode:

```toml
# core node (bonsai.toml)
[runtime]
mode = "core"

# each collector node (bonsai.toml)
[runtime]
mode                 = "collector"
collector_id         = "site-a-col-1"
core_ingest_endpoint = "http://core.example.com:50051"
```

Optionally enable mTLS on the collector-core channel via `[runtime.tls]`.

See `config/` for per-collector path profiles and `bonsai.toml.example` for full TLS config.

---

## Enrichment

Enrichers run on a schedule and write properties to graph nodes.

**NetBox** (3.x and 4.x — auto-detected at runtime from `GET {base_url}/api/`):

Configure the NetBox enricher via the UI (Enrichment workspace) or the enricher config file.
The `netbox_version` field accepts `"auto"` (default), `"3"`, or `"4"` to pin the version.

**ServiceNow CMDB**:

```toml
[integrations.servicenow]
enabled          = true
instance_url     = "https://dev12345.service-now.com"
credential_alias = "snow-cmdb"
```

---

## Remediation

Bonsai maps detection events to playbooks and proposes remediations via a trust-graduated pipeline:

1. **suggest_only** — propose only, never execute
2. **approve_each** — execute after human approval (default for production environments)
3. **auto_with_notification** — execute automatically, notify operator, rollback window open
4. **auto_silent** — fully autonomous (graduate to this deliberately)

Playbooks live in `playbooks/library/`. Each has a `rule_id`, `steps` (gNMI SET / CLI), `description`, and `risk_level`.

Enable auto-proposal (create proposals when detections fire):

```toml
[remediation]
auto_propose = true
```

---

## GNN anomaly detection

Bonsai includes a Graph Neural Network pipeline for unsupervised anomaly detection.

1. **Accumulate data**: run with `[archive] enabled = true` for 7-30 days.
2. **Train**: `python python/bonsai_ml/gnn/train_anomaly.py`
3. **Calibrate**: switch `[gnn] inference_mode = "calibration"` and review the score distribution via Operations -> GNN Calibration.
4. **Promote**: switch to `inference_mode = "production"` when P95 is below your threshold.

---

## Lab setup (ContainerLab)

For lab-backed testing with Nokia SRL or Cisco XRd:

```bash
# Deploy a 6-node DC topology
cd lab/dc
make deploy

# Start bonsai against it
docker compose --profile lab-dc up -d bonsai-lab-dc
```

See `lab/` for topology YAML files and `scripts/` for seed scripts.

---

## Development

```bash
# Rust
cargo build
cargo test --workspace

# UI (Svelte + Vite)
cd ui && npm install && npm run dev

# Python SDK
cd python && pip install -e ".[dev]"
pytest
```

See `docs/DEVELOPMENT.md` for full dev environment setup including ContainerLab, NetBox, and the parser sidecar stack.

---

## Architecture decisions

See `DECISIONS.md` for the rationale behind key architectural choices:
- lbug (LadybugDB) as the embedded graph database
- Rust-first binary with Python ML sidecar
- MCP server for AI tool use
- Two-binary model (`bonsai` + `healthcheck`)
- Trust model for autonomous remediation

---

## Contributing

1. Fork and branch from `main`.
2. `cargo test --workspace` must pass.
3. `cd ui && npm run build` must succeed.
4. New API endpoints must update `src/http_server/schema.rs`.

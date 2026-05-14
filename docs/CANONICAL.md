# Bonsai Canonical Reference

> Authored CV7 T5-1 — 2026-05-14. **This is the single document that orients
> you.** Read top-to-bottom once. Every other doc in the repo is reachable
> from the table below.

---

## What is bonsai

A streaming-first, graph-native network state engine for closed-loop autonomous
network operations. Ingests gNMI telemetry from ContainerLab (Nokia SR Linux,
Cisco IOS-XRd, Juniper cRPD, Arista cEOS), writes to an embedded graph database
(LadybugDB), and closes a detect-predict-heal loop. MIT licensed, personal
learning project. Goal: replicate Google's ANO framework architecture at lab
scale using only open source primitives.

**Current phase**: Phase 6 — UI in progress. CV7 sprint is consolidation, not
features. Detail in [`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`](../BONSAI_CONSOLIDATED_BACKLOG_CV7.md).

---

## What environment am I in (FIRST CHECK)

Run `bash scripts/dev/whichenv.sh`. It prints one of:

- `mac-dev` — source editing only. NO build, NO test, NO docker, NO clab.
- `ubuntu-ops` — all testing, smoke, e2e, chaos. NO source edits except via `git pull`.
- `cloud-ops` — long-running chaos accumulation. Same rules as ubuntu-ops.
- `unknown` — stop and ask the operator.

Full rule in [`docs/operations/dev_vs_ops_boundary.md`](operations/dev_vs_ops_boundary.md).

---

## The architecture in one diagram

```
       ┌──────────────────────────────────────────────────────┐
       │                  ContainerLab (lab)                  │
       │   Nokia SR Linux · Cisco XRd · Juniper cRPD · cEOS   │
       │   Holo / FRR (fast iteration)                        │
       └────────────────────────┬─────────────────────────────┘
                                │ gNMI / OpenConfig (only)
                                ▼
       ┌──────────────────────────────────────────────────────┐
       │              Collectors (Rust, tokio)                │
       │  gNMI subscribers · BMP · BGP-LS · syslog adapters   │
       └────────────────────────┬─────────────────────────────┘
                                │ bonsai bus (in-process / gRPC)
                                ▼
       ┌──────────────────────────────────────────────────────┐
       │              write_coordinator (Rust)                │
       └────────────────────────┬─────────────────────────────┘
                                ▼
       ┌──────────────────────────────────────────────────────┐
       │   LadybugDB (lbug) — embedded Cypher graph store     │
       │   Append-only StateChangeEvent log                   │
       └─────┬─────────────────────────────────────┬──────────┘
             │ graph events (gRPC StreamEvents)    │ queries
             ▼                                     ▼
       ┌──────────────────────────┐    ┌──────────────────────────┐
       │  Python rules sidecar    │    │  Bonsai HTTP/gRPC (Axum) │
       │  python/collector_       │    │   • bonsai UI    at /     │
       │  engine.py               │    │   • bonpy  UI    at /bonpy│
       │  (18 rule_ids: bgp, bfd, │    │   • Swagger      /api/docs│
       │   iface, syslog, snmp,   │    │   • /api/sidecars         │
       │   streaming, topology,   │    │   • MCP server            │
       │   + ML inference)        │    └──────────────────────────┘
       └──────────┬───────────────┘
                  │ gRPC CreateDetection
                  ▼
       ┌──────────────────────────────────────────────────────┐
       │  Detection table → output adapters                   │
       │  Splunk · Elastic · Prometheus · ServiceNow grounded │
       └──────────────────────────────────────────────────────┘
```

**Data flow in one paragraph**: gNMI/BMP/BGP-LS/syslog stream into the bus.
write_coordinator applies them to LadybugDB and emits `StateChangeEvent`s on a
broadcast channel. The Python rules sidecar (`python/collector_engine.py`)
subscribes via gRPC `StreamEvents`, evaluates the 18 catalogued rules + any
loaded ML models, and writes Detection rows back via gRPC `CreateDetection`.
The sidecar registers itself with bonsai at startup (`RegisterSidecar` RPC) and
heartbeats every 15s; its status is visible at `/api/sidecars` and on the
Detection Engine UI tab. Output adapters fan Detection rows out to Splunk,
Elastic, Prometheus, and ServiceNow.

> CV7 Tier 4 (2026-05-14 ADR) retires the CV6-era Rust event-detection fastpath
> in favour of sidecar visibility. Architecture: [`sidecars.md`](architecture/sidecars.md).
> Sequencing constraint: `src/event_detection.rs` is deleted only after the
> visibility plumbing + sidecar startup codification land and a 1-hour smoke
> validates the Python path.

---

## Non-negotiable rules

- **No SNMP, no NETCONF.** gNMI only, always.
- **No async runtime other than tokio.**
- **Every architectural decision gets an entry in [`DECISIONS.md`](../DECISIONS.md)** with date and rationale.
- **Never add scope beyond current phase** without flagging it explicitly.
- **Rust code must compile before ending a session.** No broken state.
- **No campus/wireless, no optical transport, no Kubernetes, no RBAC.** Say no politely.
- **Credentials never appear in source or committed files.** Use `bonsai.toml` (gitignored) or env vars.
- **Mac is dev-only. Ubuntu/cloud is ops-only.** See [`docs/operations/dev_vs_ops_boundary.md`](operations/dev_vs_ops_boundary.md).
- **One deployment mode per environment.** Laptop = bonsai-as-process. Cloud = bonsai-as-systemd-service. (CV7 Tier 2 enforces this; do not write multi-mode bash.)

---

## Scope guardrails

**IN scope**:
- DC + SP topologies
- gNMI/OpenConfig only
- Four vendor families: Nokia SR Linux, Cisco IOS-XRd, Juniper cRPD/vJunosEvolved, Arista cEOS
- Holo/FRR as OSS references
- YANG paths: interfaces, BGP, OSPF, IS-IS, LLDP, platform + SP paths (openconfig-mpls, openconfig-segment-routing, openconfig-network-instance)
- Closed-loop healing via gNMI Set
- Single-host deployment for v1

**OUT of scope**:
- SNMP, NETCONF
- Campus/wireless, optical transport
- Kubernetes/HA/clustering
- Multi-tenancy/RBAC/auth beyond TLS
- Production WAL/replication
- Config-writing UI (Phase 6 UI is view-only)
- Any fifth vendor in the first 6 months

---

## Anti-patterns (things that will kill this project)

- Adding SNMP/NETCONF "because a user asked"
- Phase 6 UI growing into a product — it is a demo view; reject admin/config/auth features
- Deploying to Kubernetes before v0.1 runs on one laptop
- Writing blog posts before Phase 2 works
- Chasing more vendors before the current four work vendor-neutrally
- Rewriting from Rust to Go because it's easier
- Accepting scope expansions that add breadth before depth of normalization
- Building a DNAC/NDI replacement — wrong audience, losing position
- Adding controller adapters speculatively — demand-driven only
- Skipping enrichment to jump to GNN — GNN without enriched graph has no business context
- Letting "bonsai should work for every network everywhere" creep in — focus matters

---

## Audience and positioning (ADR 2026-04-24)

**Primary target**: controller-less network environments — SP backbones, DC fabrics
built device-direct, hyperscale/research networks, telco core. For these operators
bonsai replaces the ad-hoc Telegraf+InfluxDB+Grafana+scripts stack with a coherent
graph, detect-heal loop, and ML pipeline.

**Secondary (narrow)**: multi-controller correlation. Individual controller adapters
are demand-driven only.

**Anti-position**: bonsai is NOT a DNAC/NDI/Meraki Dashboard replacement inside their
own fabrics.

**Graph enrichment is the primary business-context mechanism**. NetBox and ServiceNow
enrichment is Tier 4, before controller adapters.

---

## Where to find things

| You want to | Look at |
|---|---|
| Current sprint backlog | [`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`](../BONSAI_CONSOLIDATED_BACKLOG_CV7.md) |
| Earlier sprint backlogs | [`docs/backlog_archive/`](backlog_archive/) |
| Architecture decisions | [`DECISIONS.md`](../DECISIONS.md) |
| Project origin / thesis | [`PROJECT_KICKOFF.md`](../PROJECT_KICKOFF.md) |
| Feature status (single source) | [`docs/testing/FEATURE_INDEX.md`](testing/FEATURE_INDEX.md) |
| Dev vs ops boundary | [`docs/operations/dev_vs_ops_boundary.md`](operations/dev_vs_ops_boundary.md) |
| Lab placement | [`docs/operations/lab_placement.md`](operations/lab_placement.md) |
| 7-day handoff protocol | [`docs/operations/7day_handoff.md`](operations/7day_handoff.md) |
| Resource budgets | [`docs/operations/resource_budgets.md`](operations/resource_budgets.md) |
| SP lab spec | [`docs/operations/sp_lab_spec.md`](operations/sp_lab_spec.md) |
| API reference (when running) | `http://localhost:3000/api/docs` |
| OpenAPI examples | [`docs/openapi/examples/`](openapi/examples/) |
| ServiceNow integration | [`docs/integration/servicenow_aiops_strategy.md`](integration/servicenow_aiops_strategy.md) |
| Syslog fixtures | [`tests/syslog_fixtures/`](../tests/syslog_fixtures/) |
| Ingestion architecture | [`docs/ingestion_architecture.md`](ingestion_architecture.md) |
| Archive format | [`docs/archive_format.md`](archive_format.md) |
| Collector↔core protocol | [`docs/collector_core_protocol.md`](collector_core_protocol.md) |
| Sidecars (detection runtime) | [`docs/architecture/sidecars.md`](architecture/sidecars.md) |
| Bonpy UI (Python/ML status, separate from bonsai UI) | `http://localhost:3000/bonpy/` (when running); code at [`ui-bonpy/`](../ui-bonpy/) |
| Retired docs / older test results | [`docs/archive/`](archive/) |
| Build performance notes | [`docs/build_performance.md`](build_performance.md) |
| Graphify knowledge graph | [`graphify-out/GRAPH_REPORT.md`](../graphify-out/GRAPH_REPORT.md) |

---

## The five things that will trip you up

1. **Mac is dev only. Ubuntu/cloud is ops only.** Cross either line and you create the exact kind of "works on my Mac" / "stale state on the laptop" bugs CV7 is hardening against. See `dev_vs_ops_boundary.md`.
2. **Laptop runs bonsai-as-process. Cloud runs bonsai-as-systemd-service.** Never mix. CV7 Tier 2 enforces this.
3. **Detection runs in Python sidecars.** Their presence is visible at `/api/sidecars` and the UI Detection Engine tab. If detections aren't firing, first check the sidecar registry — the most common cause is "the sidecar isn't running." See [`sidecars.md`](architecture/sidecars.md).
4. **The 7-day hands-off clock is the trust threshold.** Until it completes cleanly, GNN training does not start. See `7day_handoff.md`.
5. **GNN training gates on archive depth + injection count + per-rule examples.** Skipping that gate is a CV-killer; the GNN trains on garbage.

---

## Current phase detail

Phase: 6 — UI (in progress).

Last completed:
- Phase 5.0 hygiene: TRIGGERED_BY edge, Prometheus /metrics, retention/registry seams, PlaybookCatalog, integration smoke test, 3 ADRs.
- Phase 5.1: training data export (Parquet), MLDetector (IsolationForest), features_to_vector contract, wired into RuleEngine with rules-only fallback.
- Phase 5.2: MLRemediationSelector (GBT), `export_remediation_training_set()`, wired into RemediationExecutor.
- Phase 5.3 (Model B LSTM): deferred — requires weeks of failure data.
- Phase 6.0: Axum HTTP server (port 3000) serving REST API + SSE + Svelte SPA.
  - `GET /api/topology`, `GET /api/detections`, `GET /api/trace/:id`, `GET /api/events` (SSE)
  - Svelte SPA: Topology (D3-force), Events (SSE), Trace (timeline)
  - Swagger UI at `/api/docs` (CV6)

Next: Phase 6.1 — Device onboarding UI. DiscoverDevice/AddDevice/RemoveDevice RPCs, runtime mutation via ApiRegistry, credentials via env-var name only.

---

## Build commands (Ubuntu ops only — NEVER run from Mac)

```
cargo build --release          # debug builds can exceed static-lib limits
cargo run --release
cargo test --release
cargo clippy --release -- -D warnings   # must pass before any commit
```

Mac equivalents are **refused**. Use `bash scripts/dev/macdev help` to see Mac-safe operations. On Mac, push to main; the Ubuntu laptop / cloud pull and build (interim) or install pre-built binary (post-CV7 Tier 6).

---

## Graphify knowledge graph

The repo carries a graphify graph at [`graphify-out/`](../graphify-out/).

- Before architecture/codebase questions, read [`graphify-out/GRAPH_REPORT.md`](../graphify-out/GRAPH_REPORT.md) for god nodes and community structure.
- If `graphify-out/wiki/index.md` exists, navigate it instead of raw files.
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<q>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over `grep` — these traverse EXTRACTED + INFERRED edges.
- After modifying code files in a session, run `graphify update .` to keep the graph current (AST-only, no API cost).

---

## What to do RIGHT NOW (3-step quickstart)

**If `whichenv.sh` says `mac-dev`**:
1. `bash scripts/dev/check_mac.sh` — confirm Mac is clean.
2. Edit source / docs as needed.
3. `bash scripts/dev/macdev push` — push to main.

**If `whichenv.sh` says `ubuntu-ops`**:
1. `git pull origin main` — get latest.
2. Build/test/run per `docs/operations/laptop_*.md`.
3. Write daily report under `docs/test_results/daily_runs/`.

**If `whichenv.sh` says `cloud-ops`**:
1. `git pull origin main`.
2. Confirm `bonsai.service`, `bonsai-rules-sidecar.service`, and `bonsai-chaos.service` (post-CV7 T2-2) are healthy via `systemctl status`. Also check `/api/sidecars` shows the rules sidecar registered.
3. Check `docs/test_results/daily_runs/` for today's auto-generated report.

**If `whichenv.sh` says `unknown`**: stop. Ask the operator before proceeding.

---

*This document is the canonical entry point. If you find yourself diving into
files that contradict this one, the contradicting file is wrong — fix it, or
add it to [`docs/archive/`](archive/) if it represents a retired state.*

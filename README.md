# Bonsai

**Network observability and routing intelligence, built around a graph of observed network state.**

Bonsai collects network telemetry, builds a time-aware model of device state and relationships, and makes that evidence available to operators and AI agents. It is intended to answer operational questions: what changed, which routes or services might be affected, what supports a diagnosis, and what remains unknown?

Bonsai is an **integrated development prototype (0.2.0)** with working software, reproducible live routing labs and a browser UI. It is not a production-certified platform or an autonomous network controller. Operational device writes remain disabled.

## Development status

Snapshot: **24 November 2026**, reviewed against main at [`f24422e`](https://github.com/g-arjuna/bonsai/commit/f24422e12af8e9fa071df735267dab15882cc321), with the open-network NOS roadmap clarified in the current backlog. The [canonical backlog](BONSAI_BACKLOG.md) is the execution authority; [state notes](docs/STATUS.md) link to dated evidence.

| Top-level backlog status | Epics | Share |
| --- | ---: | ---: |
| Complete within a bounded acceptance profile | 2 | 7.4% |
| Partial — implementation exists, acceptance remains | 18 | 66.7% |
| Intake — requirements or existing code need qualification | 5 | 18.5% |
| Deferred | 2 | 7.4% |
| Total | 27 | 100% |

Method: count each BN-001–BN-027 top-level summary row once, including the BN-024 status correction in the current backlog. Later research epics and subtask rows are excluded from this specific denominator; see the backlog for their scope. BN-001 and BN-006 remain complete only within their bounded profiles. **7.4% is epic closure within this subset, not product completion or production readiness.** The current implementation focus is BN-024 native network intelligence.

### What works, and what remains

| Area | Current capability and evidence boundary |
| --- | --- |
| Routing / BMP | Canonical BMP receiver, scoped RIB, route history, transport gaps, reconnect/replay and prefix/peer diagnostics. Genuine FRR IPv4 VRF BMP and 12 browser journeys were exercised. [BN-003 evidence](docs/merger/evidence/2026-11-08-bn003-acceptance-genuine/README.md). |
| Multivendor routing | Arista cEOS 4.36.0F IPv4 BMP lifecycle and software-FIB probes; Nokia SR Linux 26.3.1 operator-triggered native BGP collection. Specific tested profiles, not blanket vendor, IPv6 or physical ASIC support. [BN-006 support matrix](docs/merger/evidence/2026-11-07-bn006-live/README.md). |
| gNMI and onboarding | Streaming, inventory, discovery, OpenConfig/native paths and selective acquisition. Automatic/explicit/disabled acquisition intent and explicit TLS/mTLS/plaintext modes are implemented. The merged connectivity slice includes local gNMI server tests. Device-effective YANG catalogue, schema-bound acquisition, selection fencing and remote discovery are implemented; M6 correctness changes passed L0–L3, with bounded NOS coverage and full L4 still outstanding. [YANG catalogue](docs/YANG_CATALOGUE.md), [M6 correction](https://github.com/g-arjuna/bonsai/pull/80). |
| Native network intelligence | BN-024 foundations started: pinned OSS provenance and legacy Genie/TextFSM wire-contract repair merged. Runtime parser correctness and genuine differential execution remain open; the new Rust-native vendor normalizers and typed fact ABI are not yet delivered. SONiC and Cumulus Linux are explicit targets, not supported profiles today. [BN-024 design](docs/architecture/NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md). |
| Other telemetry | Syslog, SNMP traps, NetFlow/IPFIX, sFlow, OTLP and BGP-LS have receiver/adapter code. Identity, normalization, coverage and live qualification vary; code presence is not a support certification. [Streaming](src/streaming/mod.rs), [signals](src/signals/mod.rs), BN-008/016/021. |
| Core and collectors | Core/collector modes, assignment, gRPC forwarding, local graph state, bounded queues and health/resource visibility. Scoped collector authentication exists; provenance, rollout and fleet-scale acceptance remain. [Ingest](src/ingest.rs), [collector auth](src/collector_auth.rs). |
| Investigation and UI | Svelte operator UI, graph Explorer, topology, incidents, routing investigations, read-only MCP tools and LLM investigation runtime. Independently adjudicated accuracy, cross-provider evaluation and external-agent interoperability remain release work. [MCP](src/mcp_server.rs), [investigation runtime](src/investigation_runtime.rs), BN-004/013. |
| Rules, ML and analytics | Python sidecar, rule detections, snapshot/export jobs, embedding and GNN research paths, BonPy UI. Bounded sidecar restart/recovery was exercised. No trained-model efficacy, production forecasting or continuous learning guarantee is claimed. [BN-009 evidence](docs/merger/evidence/2026-11-09-bn009-soak/README.md), BN-010–012. |
| Change assurance | Proposal, approval, audit and executor recovery machinery; bounded crash recovery is evidenced. Live configuration changes, automatic remediation and rollback on real infrastructure are not accepted. [BN-001 evidence](docs/merger/evidence/2026-11-08-bn001-crash-recovery/README.md), BN-002/014. |

## Architecture

Bonsai has one operator/control plane. Routing is a specialized service for BMP/BGP state and history, not a second product.

```text
  Network devices and telemetry exporters
    | gNMI, syslog, SNMP, flows, OTLP       | BMP
    v                                    v
  Bonsai collector(s)                  bonsai-routing
  - subscriptions / receivers           - rbmp-core: protocol decoding
  - local graph / counters / filters    - rbmp-rib: scoped RIB
  - bounded forwarding queue            - rbmp-store: durable journals
    | authenticated gRPC                 | private routing API
    v                                    | + checkpointed projection
  Bonsai core <--------------------------+
  - inventory / assignments / identity
  - LadybugDB state and evidence graph
  - SQLite configuration registry + credential vault
  - correlation, retention, optional archives
    |
    +-- REST / SSE / gRPC / Svelte UI
    +-- read-only MCP / investigation runtime
    +-- optional Python sidecars / BonPy
    +-- optional enrichment and output integrations
```

This is the logical architecture, not a promise that every receiver is deployable in every mode. BMP belongs to the canonical routing service; BGP-LS also has a GoBGP sidecar path. Legacy BMP code remains pending retirement/projection parity (BN-007).

| Component | Responsibility |
| --- | --- |
| `bonsai`, `mode=all` | Local acquisition plus core graph/API/UI in one process. |
| `bonsai`, `mode=core` | Central graph, operator APIs/UI, assignments and remote ingestion; no local gNMI subscribers. |
| `bonsai`, `mode=collector` | Distributed acquisition, local processing and queued forwarding to core. |
| `bonsai-routing` | Canonical BMP ingestion, RIB, durable raw/event journals and internal routing API. Bonsai proxies public routing queries and projects graph summaries. |
| Python sidecars | Optional rule evaluation, parsing, snapshot/export, embeddings and model jobs; health, backpressure and evidence are part of the operational contract. |

### Native network intelligence (in development)

The intended BN-024 pipeline accepts both structured and text observations: native gNMI/YANG, read-only management APIs, CLI/SSH and protocol evidence. Genie and TextFSM remain optional parsing backends; they are not prerequisites for native Rust profiles. The planned normalizers produce versioned typed facts with identity, scope, timestamps, completeness and evidence provenance, then pass through existing authority and reconciliation gates before any graph projection. SuzieQ OSS is a pinned behavioral reference and test oracle, not a required production service.

| NOS family | Planned native sources and interpretation | Current BN-024 status |
| --- | --- | --- |
| SONiC | Native gNMI/YANG and authorized structured management views of CONFIG_DB, APPL_DB, STATE_DB, counters, FRR and Linux; preserve host/NPU namespace, ASIC, VRF and live vs saved configuration. | Dedicated backlog BN-024-08/24; no native profile or live acceptance claimed. |
| NVIDIA Cumulus Linux | NVUE read-only REST/structured output, qualified telemetry, FRR JSON/BMP and Linux kernel/bridge state; separate configured intent, control RIB, software FIB and hardware offload evidence. | Dedicated backlog BN-024-08A; no native profile or live acceptance claimed. |
| Traditional NOS and standalone FRR | Feature-specific native structured data and verified CLI parsing; gradual Rust normalization with existing parsers retained until parity is proven. | Existing collection support is profile-specific; new native intelligence is not yet qualified. |

Both open-network NOS families are required early extension-contract test cases, not postponed until a future plugin SDK. Exact release/image/API/feature qualification is mandatory; software labs do not establish physical ASIC forwarding or optics/queue support. See [BN-024 backlog](BONSAI_BACKLOG.md).

### Storage and evidence

- **LadybugDB (lbug)** is the embedded graph engine for network state, topology, events and evidence relationships. Cypher is available through governed query surfaces, not unrestricted access for every caller.
- **SQLite** (`bonsai_config.db`) holds registry and other configuration records, including trusted identity bindings. Protected authentication storage and legacy migration are implemented; disabled legacy graph tombstones may remain. Bootstrap configuration and the encrypted credential vault are separate.
- **Routing journals** preserve protocol and event history independently of graph summaries. The embedded graph is not the authoritative raw RIB/event log.
- **Parquet/Arrow exports, optional archives and external TSDB integrations** provide additional analytical paths. Not all high-frequency data belongs in the graph; enterprise-scale retention and resource sizing still require measured qualification.

Source entry points: [core startup](src/server_startup.rs), [graph](src/graph/mod.rs), [collector graph](src/collector/graph.rs), [SQLite store](src/sqlite_store.rs), [routing service](src/routing_service/mod.rs), [archive](src/archive.rs).

### Identity and security boundaries

A transport IP address or reported exporter name is evidence, not proof of device identity. The [identity contract](src/telemetry_identity.rs) distinguishes transport endpoint, protocol exporter and canonical inventory device. Credential-pinned bindings and scoped authorization are being rolled out across ingress paths.

- BMP verified mode requires authenticated ingress and a matching trusted binding for authoritative device attribution. Untrusted connections are isolated; plaintext is a deliberate development exception. [BN-021f evidence](docs/merger/evidence/2026-11-11-bn021f-authenticated-bmp/README.md).
- Collector-to-core authentication and device-to-collector gNMI security are separate boundaries. gNMI supports `tls_verify`, `mtls` and explicit lab `plaintext`; legacy inference exists for upgrades, not as a secure deployment recommendation.
- HTTP authentication, roles and graph-read restrictions exist. The ownership policy defaults to `audit_only`, which **does not enforce tenant isolation**. See the [ownership policy](docs/architecture/ownership_mapping.md).
- Protected authentication storage (BN-022) is merged, and migration/upgrade were exercised with packaged images in BN-019. Deployment restrictions and scoped access acceptance remain; this does not establish multi-writer auth storage or tenant isolation. [Upgrade evidence](docs/merger/evidence/2026-11-24-bn019-m6-upgrade/README.md).

## Try it in an isolated development environment

The standalone Docker Compose profile runs the Bonsai application **and** the routing service without a ContainerLab topology. It is a development configuration, not a hardened installation.

Prerequisites: Linux, Docker with Compose v2, and sufficient disk/RAM for the source build. See the [development guide](docs/DEVELOPMENT.md) for build constraints.

```bash
git clone https://github.com/g-arjuna/bonsai.git
cd bonsai
cp .env.example .env
```

Edit `.env`: set a strong, unique `BONSAI_VAULT_PASSPHRASE` and `BONSAI_ROUTING_SERVICE_TOKEN` (generate independent values with `openssl rand -hex 32`). Do not commit the file.

The routing service also requires BMP TLS material or an explicit development-only exception. For an isolated local evaluation with no trusted exporter attribution, set `BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT=1` in `.env`. Keep the verified identity mode; do not enable claim-compatible attribution.

```bash
docker compose --profile standalone config --quiet
docker compose --profile standalone up -d --build
docker compose --profile standalone ps
```

Open `http://localhost:3000` on the host. Start with onboarding and inventory; use health/readiness to inspect the services. There is no live network state until you connect authorized sources.

**Isolation matters:** the checked-in Compose file publishes HTTP 3000, gRPC 50051 and BMP 11019 on host interfaces, and standalone uses `runtime.auth_mode=development-compat`. Run inside an isolated VM/network or restrict published ports and ingress before starting. Do not expose this setup to an untrusted network or send sensitive production telemetry to it.

For a distributed lab, `distributed` starts core plus one collector; `two-collector` adds a second. These profiles need lab networking, TLS material, credentials and appropriate bindings; they are not equivalent to the full standalone routing deployment. See [Compose](docker-compose.yml), [core config](docker/configs/core.toml), [collector config](docker/configs/collector-1.toml) and [bootstrap config](bonsai.toml.example).

## Interfaces

| Surface | Entry point |
| --- | --- |
| Operator UI | `/` — inventory, topology, events, investigations, settings |
| BonPy | `/bonpy/` when its assets are built |
| REST / OpenAPI | `/api/openapi.json`, `/api/docs` |
| Routing API | `/api/routing/v1/*` — speakers, peers, routes, history, coverage and investigations |
| MCP | `POST /mcp` — read-only JSON-RPC tools, subject to authorization and scope |
| gRPC | [BonsaiGraph protobuf](proto/bonsai_service.proto) — ingest, registration, queries and other service RPCs |
| Health | `/health`, `/healthz`, `/readyz` |

Use the running version's OpenAPI and source contracts rather than a static inventory of every endpoint or receiver port. The routing service's `/internal/routing/v1/*` API is private to the application boundary.

## Roadmap and release gates

The current implementation priority is BN-024: native network intelligence on top of the read-only Secure Core. The initial provenance and parser-contract slices are merged; typed facts, native normalization and semantic reasoning remain staged work. Secure ingress, exact acquisition, identity, projection and authorization gates remain mandatory before enabling new authoritative graph writes. A full-length soak, qualified shadow pilot and production-scale HA/fleet deployment remain open. Existing Helm/etcd code does not establish a replicated graph or a proven failover contract.

The expansion areas below are staged development or **research/intake**, not a list of supported features:

| Backlog | Direction |
| --- | --- |
| BN-023 | Kubernetes packaging, single-writer deployment and later fleet/HA proof |
| BN-024 | [Native multivendor state normalization (including SONiC and Cumulus Linux), configuration semantics and deterministic path assurance](docs/architecture/NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md) |
| BN-025 | [AI fabric observability — optics, queues, RDMA and workload correlation](docs/architecture/AI_FABRIC_OBSERVABILITY_ASSESSMENT_2026-11-17.md) |
| BN-026 | [Service-provider transport assurance — IGP, TE, LDP, RSVP-TE, SR, FRR, OAM and capacity](docs/architecture/SERVICE_PROVIDER_TRANSPORT_ASSURANCE_ASSESSMENT_2026-11-17.md) |
| BN-027 | [Wireless and campus — AP/controller telemetry, Wi-Fi 6/7, client journeys, PoE and NAC](docs/architecture/WIRELESS_CAMPUS_INTELLIGENCE_ASSESSMENT_2026-11-17.md) |

All work is tracked in [BONSAI_BACKLOG.md](BONSAI_BACKLOG.md). The intended agent-facing outcome is independently checkable network evidence, not unrestricted access or an LLM's unsupported assertion. Neither a graph path nor a model output alone proves packet delivery.

## Developing Bonsai

The maintained workflow is native Linux, Rust [1.95.0](rust-toolchain.toml), Svelte 5 and Python 3.11+ (see the [Python package](python/pyproject.toml)). Start with the [development guide](docs/DEVELOPMENT.md), [development contract](AGENTS.md) and [build acceleration playbook](docs/BUILD_ACCELERATION_PLAYBOOK_2026-11-11.md).

Use scoped checks and the shared build lane; avoid bare `cargo build` / `cargo test` on the main package during iteration. The canonical [L0–L4 gates](scripts/ci/gates.sh) distinguish source checks, compiled/executed tests and live acceptance. Local validation is authoritative under the current owner policy; remote GitHub Actions CI is deferred. A skipped or unexecuted gate is not a pass.

| Path | Contents |
| --- | --- |
| `src/` | Rust application, collectors, graph, identity, receivers, routing proxy, API and investigations |
| `crates/rbmp-{core,rib,store}/` | RustyBMP-derived routing libraries |
| `proto/` | gNMI and Bonsai gRPC contracts |
| `ui/`, `ui-bonpy/` | Operator and Python/ML dashboards |
| `python/` | SDK, rules, parsing and ML jobs |
| `docker/`, `deploy/`, `lab/` | Packaging, deployment and lab topology |
| `tests/`, `scripts/ci/` | Regression and validation gates |
| `docs/architecture/`, `docs/merger/evidence/` | Design records and dated acceptance evidence |

When updating this README, refresh the snapshot against main and the canonical backlog. Link to evidence for new claims; keep planned, implemented, lab-verified and operationally accepted capabilities distinct.

## License

[Apache-2.0](LICENSE). Routing components derived from RustyBMP are attributed in [NOTICE](NOTICE).

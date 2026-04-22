# BONSAI — Consolidated Backlog v4.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V3.md`. Produced after reviewing the `codex-t1-1-credentials-vault` branch on 2026-04-22. Extensive v3 execution landed: credentials vault, Site as graph entity, 4-step onboarding wizard, archive append-to-hour, MessagePack wire format, UI SSE, zstd compression, disk-backed collector queue, mTLS, live two-process validation with queue-during-outage proof.
>
> **What v4 adds on top of v3's strategic direction:**
> 1. **Containerisation and scale architecture** — two-processes-on-a-laptop is not a real distributed test; bonsai and lab need to share a container plane
> 2. **Graph enrichment layer via MCP / CLI / NETCONF / RESTCONF** — extending the digital twin with IPAM, DCIM, application, and CLI-scraped context
> 3. **Syslog and SNMP trap handling** — thought through carefully given the gNMI-only guardrail
> 4. **Updated UI usability audit** from a network-practitioner perspective
> 5. **Path B GNN and investigation agent carried from v3** with sharpened sequencing

---

## Table of Contents

1. [Progress Since v3](#progress)
2. [TIER 0 — Loose Ends from v3 Review](#tier-0)
3. [TIER 1 — Containerisation and Deployment Plane](#tier-1)
4. [TIER 2 — Scale Architecture](#tier-2)
5. [TIER 3 — Graph Enrichment via MCP and Legacy Protocols](#tier-3)
6. [TIER 4 — Syslog and SNMP Traps (thought-through position)](#tier-4)
7. [TIER 5 — UI Usability Pass for Network Practitioners](#tier-5)
8. [TIER 6 — Path A Graph Embeddings → Path B GNN](#tier-6)
9. [TIER 7 — Investigation Agent](#tier-7)
10. [TIER 8 — Carryover Extensions](#tier-8)
11. [Execution Order](#execution-order)
12. [Merge Plan](#merge-plan)
13. [Guardrails](#guardrails)

---

## <a id="progress"></a>Progress Since v3 — Verified Against the Branch

**Completed and removed** (all verified with code review, not just self-declaration):

| v3 item | Status | Evidence |
|---|---|---|
| T0-1 archive append-to-hour | ✅ Done | `src/archive.rs::HourlyArchiveWriter` holds one `ArrowWriter` per `(target, hour)` partition, closes at hour boundary, `__part-NN` for restart-within-hour |
| T0-2 normalize_address validation | ✅ Done | host:port validation for hostname/IPv4/bracketed-IPv6, release test |
| T0-3 ingest MessagePack wire format | ✅ Done | `value_msgpack: bytes` in proto, `rmp_serde` round-trip, honest annotation that single-field change cannot shrink total per-update 30% because `target`/`path`/`collector_id` dominate |
| T0-4 UI SSE (not polling) | ✅ Done | `/api/events` emits registry lifecycle + subscription_status_change events; Onboarding debounces SSE-driven refreshes and closes EventSource on tab-hidden |
| T0-5 clippy baseline | ✅ Done | clean `cargo clippy --release --all-targets -- -D warnings`; flagged `result_large_err` explicitly allowed with rationale |
| T1-1 credentials vault | ✅ Done (well) | `src/credentials.rs` (411 lines), `age` crate + scrypt, `SecretString` zeroise-on-drop, encrypted `vault.age` + unencrypted `metadata.json`, round-trip + wrong-passphrase tests, alias-wins precedence in `resolve_target_credentials` |
| T1-2 Site as graph entity | ✅ Done | `Site(id, name, parent_id, kind, lat, lon, metadata_json)` node + `LOCATED_AT(Device → Site)` + `PARENT_OF(Site → Site)` edges; `sync_sites_from_targets` migration makes legacy string-sites into `kind: "unknown"` nodes |
| T1-3 onboarding wizard | ✅ Done | 4-step wizard in `ui/src/lib/Onboarding.svelte` (852 lines): address+creds → discovery → profile+path selection → confirm; inline vault and site management cards on Step 1; required paths disabled, optional paths toggleable; bulk toolbar for multi-device ops; edit flow carries saved paths into wizard |
| T1-4a edit via wizard | ✅ Done | Edit triggers pre-populated wizard on Step 1 with saved paths carried forward |
| T1-4c bulk operations | ✅ Done | Bulk toolbar in the managed-devices workspace |
| T2-1 zstd compression | ✅ Done | `send_compressed(CompressionEncoding::Zstd)` on collector client; symbol conflict resolved by dynamically-linked LadybugDB (build.rs `LBUG_SHARED` path) rather than build feature flags |
| T2-2 disk-backed queue | ✅ Done | Append-only `queue.dat` + `queue.ack`, `sync_data()` per record, compaction after ack, size+age caps with warning logs |
| T2-3 mTLS | ✅ Done | `docs/distributed_tls.md` with lab CA layout; cert+key+ca_cert on both sides; `server_name` verification |
| T2-4 live two-process validation | ✅ Done | `docs/distributed_validation.md` — 2026-04-22 run across four lab targets; queue-during-core-outage proof; wrong-client-cert rejection smoke |
| T5-3 training script validity checks | ✅ Done | Shared readiness module + blocking checks in both training scripts with `--force` overrides |
| T5-5 metrics expansion (slice) | ✅ Current slice done | Broadcast drop counter, archive flush logs, collector queue stats logs |

**Discipline observations from this review:**

- **The v3 document itself got annotated with per-item execution updates** before v4 was written. This is a pattern worth keeping. Living documents beat post-hoc retrospectives.
- **Honest accounting** — the MessagePack annotation explicitly flags that the 30% target applies to the value field, not the whole update, because `target`/`path`/`collector_id` dominate scalar payloads. That nuance would be easy to fudge. The fact that it's called out earns trust.
- **Symbol-conflict pragma** — switching LadybugDB to dynamic linking to sidestep zstd duplicate symbols is the right kind of pragmatism. Document as ADR if not already there.
- **One credential-vault concern carried into v4**: every `resolve()` writes metadata to update `last_used_at_ns`, causing disk I/O on every fresh subscribe. Debounce/batch in v4 (see T0 below).

---

## <a id="tier-0"></a>TIER 0 — Loose Ends from v3 Review

### T0-1 (v4) — Debounce credential metadata writes — ✅ Done

**What**: `CredentialVault::resolve` writes the metadata JSON on every resolve to update `last_used_at_ns`. During a 50-device reconnect burst that's 50 disk writes for a field nothing reads in the hot path.

**Where**: `src/credentials.rs:194-216`

**Options**:
- Batch updates — coalesce `last_used_at_ns` writes into a single timer-driven flush (every 60s)
- Skip updates unless the last-used timestamp is older than some window (e.g. 5 minutes). The field's purpose is audit, not fine-grained telemetry.

**Done when**: a burst of 50 sequential `resolve()` calls produces one metadata write, not 50. Unit test asserts this.

**Execution update - 2026-04-22**: `CredentialVault::resolve` now records
`last_used_at_ns` at most once per alias per five-minute audit window. The first
resolve after add/restart is still persisted for operator visibility, but
reconnect bursts no longer rewrite `metadata.json` on every device dial. Focused
tests cover a 50-resolve burst and the window predicate.

### T0-2 (v4) — Credential vault passphrase rotation ADR — ✅ Done

**What**: today, rotating `BONSAI_VAULT_PASSPHRASE` requires export-remove-reimport. No in-place rotation. This is fine for v1 but needs to be documented as deferred so someone doesn't run into it unexpectedly.

**Where**: `DECISIONS.md`

**Done when**: one dated ADR entry stating the deferred scope + the manual workaround.

**Execution update - 2026-04-22**: Added a dated ADR to `DECISIONS.md`
documenting that in-place passphrase rotation is deferred for v1, why that is
acceptable for the local vault threat model, and the manual old-passphrase to
new-vault workaround.

### T0-3 (v4) — Archive Parquet value field documentation — ✅ Done

**What**: the archive Parquet schema still stores `value` as a JSON-serialised string, even though the wire format now uses MessagePack. An analyst reading the Parquet files via pandas/duckdb has to `json.loads(row['value'])`.

**Where**: `docs/` new file `archive_format.md`

**Done when**: a short doc explains the schema (including the JSON-in-value choice), with a pandas example for reading. No code change required — this is an operator-facing decision memo.

**Execution update - 2026-04-22**: Added `docs/archive_format.md` documenting
the collector-local Parquet layout, schema, `__part-NN` restart behavior, why
`value` remains JSON text even though ingest uses MessagePack, and pandas/DuckDB
examples for reading archived telemetry.

### T0-4 (v4) — CLI device commands (`bonsai device add/remove/list`) — ✅ Done

**What**: v3 T1-4d — the CLI commands that mirror the UI onboarding. The UI landed, CLI did not. Useful for headless/CI, automation scripts, and operators who prefer the terminal.

**Where**: new binary target `src/bin/bonsai-device.rs` (or subcommand in the main binary), uses the gRPC client.

**Done when**: `bonsai-device add --addr X --alias Y --role Z` works end-to-end without the UI.

**Execution update - 2026-04-22**: The main binary already had
`bonsai device list|add|remove|stop|start|restart`; this slice upgraded it to
prefer the live gRPC `BonsaiGraph` device API so changes reach the running
registry/subscriber manager without using the UI. If the API is unavailable, it
falls back to the local registry file for offline/headless prep. `device --help`
now prints usage without also listing devices.

### T0-5 (v4) — Site hierarchy depth guard — ✅ Done

**What**: `PARENT_OF` is recursive — nothing prevents a cycle or a 50-deep tree. For lab scale this is fine; for operational sanity add a max-depth sanity check at insert time.

**Where**: `src/graph.rs::upsert_site_record`

**Done when**: inserting a Site whose parent chain is longer than 10 returns a clear error; cycle detection rejects self-referential loops.

**Execution update - 2026-04-22**: `upsert_site_record` now validates the
candidate parent chain before writing. It rejects `parent_id == id`, rejects
cycles that traverse existing `Site.parent_id` links, and rejects chains deeper
than 10 ancestors with a clear error. Focused release tests cover self-parent,
two-node cycle, and depth-limit failures.

---

## <a id="tier-1"></a>TIER 1 — Containerisation and Deployment Plane

This is a new strategic tier. The distributed collector-core work is done at the code level; what's missing is a deployment plane where *core*, *collector(s)*, *ContainerLab devices*, and future *enrichment sidecars* all coexist cleanly.

**Your intuition is exactly right.** Two processes on a laptop does not prove anything about network reachability, DNS, cert expiry, or process-boundary issues. Running bonsai in containers alongside ContainerLab's containers gives you:

- **Real network paths** — collectors talk to the core over a container network, not localhost
- **Reproducible environment** — the same image runs on your laptop, a dev VM, and production
- **Clean process boundaries** — forced separation of config, secrets, state, logs
- **Easy chaos** — `docker kill bonsai-core`, `docker network disconnect` for partition testing
- **Path to shipping** — future operators can deploy one image per role

**Non-negotiable: we do NOT introduce Kubernetes.** Guardrail from v1 stands. Docker + Docker Compose for v4. Operators can pick Kubernetes later; our demo and dev experience does not require it.

### T1-1 — Dockerfiles for core, collector, and combined (all) — foundation started

**What**: three production-grade Dockerfiles — multi-stage build, minimal runtime image, non-root user, explicit healthcheck.

**Where**:
- `docker/Dockerfile.bonsai` — single image that runs in any mode (`--mode all|core|collector` from entrypoint); same binary, different config
- `docker/Dockerfile.ui-build` — multi-stage that builds the Svelte UI and copies dist into the core image

**Design**:
```dockerfile
# Stage 1: Rust build (uses cargo-chef for dependency caching)
FROM rust:1.82-bookworm AS rust-builder
# ... build with release, copy out target/release/bonsai
# Stage 2: UI build
FROM node:20-alpine AS ui-builder
# ... npm ci, npm run build, output ui/dist
# Stage 3: Runtime
FROM debian:bookworm-slim
RUN useradd -r -s /bin/false bonsai
COPY --from=rust-builder /app/target/release/bonsai /usr/local/bin/
COPY --from=ui-builder /ui/dist /app/ui/dist
USER bonsai
ENV BONSAI_CONFIG=/etc/bonsai/bonsai.toml
HEALTHCHECK --interval=30s CMD curl -fsS http://localhost:3000/api/readiness || exit 1
ENTRYPOINT ["/usr/local/bin/bonsai"]
```

**Design principles**:
- **One image, many roles.** Mode is determined by config, not image. Same binary wherever it runs.
- **Non-root.** Bonsai runs as UID 10001, cannot write anywhere outside its volume mounts.
- **Multi-arch.** Build for `linux/amd64` and `linux/arm64` so Apple Silicon devs can run it natively.
- **Small.** Runtime image under 200 MB.
- **Volume contract documented.** `/var/lib/bonsai/graph`, `/var/lib/bonsai/archive`, `/var/lib/bonsai/queue`, `/var/lib/bonsai/credentials`, `/etc/bonsai/bonsai.toml`, `/etc/bonsai/tls`. Lots of small volumes, clearly purposeful.

**Done when**:
- `docker build -f docker/Dockerfile.bonsai .` produces a working image on Linux and macOS (Apple Silicon)
- The image runs cleanly in each of the three modes
- Image is under 200 MB
- Hadolint passes with no warnings
- ADR documents the multi-stage build choices

**Execution update - 2026-04-22**: Added `.dockerignore` and
`docker/Dockerfile.bonsai` as the first containerization foundation. The
Dockerfile uses a cargo-chef Rust builder, Node UI builder, Debian slim runtime,
non-root UID/GID 10001, `/etc/bonsai/bonsai.toml` config convention, Bonsai
state directories under `/var/lib/bonsai`, and an HTTP readiness healthcheck for
core/all roles. Added an ADR documenting the one-image/many-roles decision.
Validation is still pending because Docker is not installed in the current
Windows environment; image size, runtime smoke, multi-arch build, and hadolint
remain open before marking T1-1 done.

### T1-2 — Docker Compose for local dev and lab

**What**: a `docker-compose.yml` that brings up bonsai core, one collector, and ContainerLab devices on a shared Docker network. This becomes the primary local development experience.

**Where**: `docker/docker-compose.yml` + `docker/compose-profiles/` with variants

**Design**:
```yaml
# docker/docker-compose.yml — minimum for local development
services:
  bonsai-core:
    build: { context: .., dockerfile: docker/Dockerfile.bonsai }
    environment:
      BONSAI_MODE: core
      BONSAI_VAULT_PASSPHRASE_FILE: /run/secrets/vault_passphrase
    volumes:
      - bonsai_graph:/var/lib/bonsai/graph
      - bonsai_archive:/var/lib/bonsai/archive
      - bonsai_creds:/var/lib/bonsai/credentials
      - ./config/core.toml:/etc/bonsai/bonsai.toml:ro
      - ./tls:/etc/bonsai/tls:ro
    ports:
      - "3000:3000"   # UI
      - "50051:50051" # gRPC API + Ingest
    secrets: [vault_passphrase]
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://localhost:3000/api/readiness"]
    networks: [bonsai]

  bonsai-collector-1:
    build: { context: .., dockerfile: docker/Dockerfile.bonsai }
    environment:
      BONSAI_MODE: collector
    volumes:
      - collector_1_queue:/var/lib/bonsai/queue
      - ./config/collector-1.toml:/etc/bonsai/bonsai.toml:ro
      - ./tls:/etc/bonsai/tls:ro
    depends_on:
      bonsai-core: { condition: service_healthy }
    networks: [bonsai, clab]  # joined to the ContainerLab network too

  # ContainerLab runs separately and attaches its network as external
  # to avoid lifecycle coupling

networks:
  bonsai: {}
  clab:
    external: true
    name: clab

volumes:
  bonsai_graph:
  bonsai_archive:
  bonsai_creds:
  collector_1_queue:

secrets:
  vault_passphrase:
    file: ./secrets/vault_passphrase
```

**Profiles** (invoked with `docker compose --profile X up`):
- `dev` — core + UI, no collector, local gNMI direct to lab (single-process equivalent)
- `distributed` — one core, one collector, mTLS enabled, queue volumes mounted
- `two-collector` — one core, two collectors pointing at different subsets of lab devices for partition testing
- `chaos` — adds a chaos-runner container that drives faults from inside the network

**Done when**:
- `docker compose --profile distributed up -d` starts bonsai, the collector comes up, authenticates, starts forwarding
- Killing the core container (`docker kill bonsai-core`) causes the collector queue to grow; restarting the core drains it — reproducing T2-4 validation but inside containers
- `docker compose down` cleanly stops everything; volumes persist across restarts
- README has a "Quick Start with Docker" section

### T1-3 — ContainerLab integration

**What**: document and automate the way bonsai coexists with ContainerLab's device containers. Today they're separate universes; for the distributed dev experience they must share a network.

**Where**: `docker/clab-integration/`
- `clab.topology.with-bonsai.yaml` — a ContainerLab topology file that adds a `bonsai-collector` node attached to the management network
- `scripts/deploy-lab-with-bonsai.sh` — brings up ContainerLab, then docker-composes bonsai into the same Docker network

**Design**:
- ContainerLab already creates a Docker network (typically `clab-<topo-name>`)
- bonsai containers join that network via compose `networks.external`
- Collector config uses ContainerLab-assigned IPs or DNS names (containerlab gives each node a DNS name like `clab-bonsai-p4-srl-leaf1`)
- Certs for mTLS are generated once in a shared volume by a helper script

**Done when**:
- A single command brings up the four-device P4 lab + one bonsai collector + bonsai core, with mTLS enabled
- Bonsai UI is reachable from the host browser; onboarding via discovery against `clab-*` names works
- Shutting down with `docker compose down && containerlab destroy` leaves no dangling volumes or networks

### T1-4 — Secret handling without docker-compose secrets dependency

**What**: docker-compose secrets are limited (they're files, which is fine, but the pattern doesn't extend cleanly outside compose). Document the container-side credential flow that works in compose, in bare Docker run, and in future Kubernetes without requiring code changes.

**Where**: `docs/container_secrets.md`

**Design**:
- The credential vault passphrase is read from `BONSAI_VAULT_PASSPHRASE` env var OR from a file path specified by `BONSAI_VAULT_PASSPHRASE_FILE`
- In compose, mount a secret file to `/run/secrets/vault_passphrase` and set `BONSAI_VAULT_PASSPHRASE_FILE=/run/secrets/vault_passphrase`
- In bare Docker run, the operator can pass `-e BONSAI_VAULT_PASSPHRASE=...` (less safe, but valid for dev)
- In a future Kubernetes deployment, the same `_FILE` pattern works with `Secret` volumes

**Done when**: `CredentialVault::open` supports both env var and file-path env var without changing the rest of the code; doc + example compose files.

### T1-5 — Chaos inside containers

**What**: the chaos runner currently runs against a specific ContainerLab topology from WSL. Once bonsai-in-containers exists, chaos should run as a sidecar container on the same network.

**Where**: `docker/Dockerfile.chaos` + `docker-compose.chaos.yaml`

**Done when**: `docker compose --profile chaos up -d` brings up bonsai + lab + a chaos container; the chaos container writes its CSV to a volume the host can inspect.

---

## <a id="tier-2"></a>TIER 2 — Scale Architecture

Now that containers give you a real deployment plane, we can have the scale conversation honestly.

**The core question**: how do bonsai components scale horizontally vs vertically? What are the hard limits? What does the path from a 4-device lab to a 4000-device network look like?

### T2-1 — Document the scale thesis

**What**: a `docs/scale_architecture.md` that answers, explicitly, these questions:

1. **How does the collector scale?** Horizontally. One collector per site/rack/POP. Each collector subscribes to a subset of devices. Collectors are stateless apart from their disk queue. Adding a collector = deploy a new container, register its devices via the onboarding API.

2. **How does the core scale?** Vertically in v1. The core holds the graph (LadybugDB is embedded), serves the UI, runs detectors and playbook executor. Scaling path is: (a) more CPU/RAM for graph and detection, (b) separate graph process later, (c) sharded graph much later. Explicit: no core horizontal scaling in v1.

3. **Where does the graph hit a wall?** LadybugDB is single-writer, embedded. Based on current schema, a 10,000-device fleet with 10 interfaces/device and minute-scale state-change retention fits comfortably in a graph under 10 GB. Past that, we're into sharding or migrating off embedded.

4. **Archive scaling.** Each collector writes its own archive — this is already collector-local per earlier ADR. A central storage plane (S3-compatible) becomes useful when collector fleet > 10 or archives exceed local disk; collectors rsync-to-S3 or write to S3 directly via object-store Parquet crate.

5. **TSDB scaling.** When T8-9 lands (TSDB adapter), collectors emit remote-write to a Prometheus/Mimir endpoint. That's the TSDB's problem, not ours. Our job is to not saturate it with metrics that should be graph state.

**Where**: `docs/scale_architecture.md` with a concrete table of device-count → recommended collector count → expected graph size → expected archive size/day.

**Done when**: an operator reading this doc can estimate resource requirements for their fleet without experimentation.

### T2-2 — Collector-local Parquet archive in distributed mode

**What**: the architecture decision from v2 stated the archive is collector-local in the distributed topology. Today's code makes this true (archive subscribes to the local event bus on whichever process is running the collector role). Document the operational consequences.

**Where**: extend `docs/scale_architecture.md`

**Operational consequences to document**:
- Each collector has its own archive directory. Retrieving multi-collector data for training requires `rsync` or mounting a shared volume.
- GNN training (T6) needs archive data from all collectors. Training can either (a) pull all archives to one host or (b) read S3 directly.
- Backup strategy: collector archives are ephemeral in principle (the graph holds current state) but operationally valuable for training — treat them as "worth backing up but not crisis-critical."

### T2-3 — S3-compatible archive backend (future-ready)

**What**: make the archive writer pluggable so `LocalFileArchive` (today) and `S3Archive` (future) are two implementations of the same trait.

**Where**: refactor `src/archive.rs` to `src/archive/mod.rs` with `LocalFileArchive` and `S3Archive` (stub initially).

**Why now, even as stub**: the refactor is cheap today. When operators need S3, the trait is already there; implementation is dropping in the `object_store` crate.

**Done when**: trait exists, `LocalFileArchive` is the default implementation, an `S3Archive` stub returns "not implemented" with a clear error, ADR documents the plan.

### T2-4 — Core bottleneck profiling

**What**: a one-off exercise to find where the core becomes CPU-bound as the collector count rises. Candidates: graph write lock contention, detection rule evaluation, SSE fan-out.

**Where**: `scripts/profile_core_load.py` — synthetic telemetry generator that saturates the core with TelemetryIngest streams from N simulated collectors.

**Done when**:
- Script exists that can drive 10, 100, 1000 simulated devices' worth of telemetry into the core
- Results documented in `docs/core_bottlenecks.md`: at what load does CPU go to 100%? What's the bottleneck (graph write lock? JSON parsing? broadcast fan-out?)
- If graph write lock is the bottleneck, ADR captures the plan — likely "move graph writer to its own process" is the real answer, deferred to whenever the bottleneck matters

### T2-5 — Multi-collector validation

**What**: one of the docker-compose profiles (T1-2 `two-collector`) sets up two collectors, each subscribing to a subset of the lab. Verify behaviour is consistent: both collectors see their devices, core sees all devices, graph correctly attributes updates to the right collector.

**Where**: `docs/distributed_validation.md` extended

**Done when**: a reproducible multi-collector run is documented, with graph queries proving cross-collector telemetry correctness.

---

## <a id="tier-3"></a>TIER 3 — Graph Enrichment via MCP and Legacy Protocols

This is the most important strategic tier in v4. You've identified exactly where bonsai is weak compared to NetAI and the commercial AIOps tools.

**The core thesis**: bonsai's graph today knows only what gNMI streams tell it. That's a tight, trustworthy loop — but it's a *thin* digital twin. A real digital twin carries:

- **IPAM data** — subnets, VLANs, IP reservations (NetBox, Infoblox, custom IPAMs)
- **DCIM data** — physical location, rack, device model, serial, lifecycle state (NetBox, Nautobot)
- **Application context** — which services run where, which VLANs carry payment traffic, which devices are customer-facing (ServiceNow CMDB)
- **Configuration details** — route-maps, prefix lists, ACLs, BGP communities — data that gNMI operational state does not expose cleanly
- **Ownership and lifecycle** — device owner, escalation path, patch status, vendor support contract

Without these, bonsai can detect "BGP session down" but not "BGP session carrying customer X's prefix-list Y is down." That's the difference between alerting and insight.

**The MCP era changes the economics.** In 2025-2026, every major network tool (NetBox, Nautobot, Infoblox, ServiceNow, pyATS, Netmiko, Ansible) has an MCP server either shipped by the vendor or community-built. NetClaw's strength is that it plugs into 44 of these without writing custom integration code. Bonsai can do the same, at the enrichment layer — without disturbing the hot-path gNMI loop.

**But we do NOT want the detect-heal loop depending on MCP calls.** MCP is async, slow (seconds), and the servers can be down. Enrichment is background — it decorates the graph; it doesn't gate detection.

### T3-1 — `GraphEnricher` trait and enrichment pipeline

**What**: a Rust trait (plus optional Python variant) that any enrichment source implements. Enrichers run on a schedule, read external data, and write decorating properties and relationships onto existing graph nodes. They never replace gNMI-sourced state.

**Where**:
- New module: `src/enrichment/mod.rs`
- Trait definition: `src/enrichment/trait.rs`
- Enricher implementations: `src/enrichment/netbox.rs`, `src/enrichment/servicenow.rs`, etc.

**Trait design**:
```rust
#[async_trait]
pub trait GraphEnricher: Send + Sync {
    /// Unique name for this enricher ("netbox", "servicenow-cmdb", "cli-interface-descriptions")
    fn name(&self) -> &str;

    /// How often to run the enrichment cycle
    fn schedule(&self) -> EnrichmentSchedule;

    /// Run one enrichment pass. Returns a summary for logging.
    async fn enrich(&self, graph: &GraphStore) -> Result<EnrichmentReport>;
}

pub enum EnrichmentSchedule {
    OnStartup,
    Periodic(Duration),
    OnDeviceAdded,         // triggered by RegistryChange::Added
    OnDemand,              // manual trigger via API
}
```

**Design principles**:
- **Enrichers write via a fixed Cypher surface** — they cannot create new node labels or change existing node schemas. They add properties to existing nodes (with a namespace like `netbox_*`) and add relationships of pre-registered types (like `OWNED_BY`, `TAGGED_AS`, `CARRIES_VLAN`).
- **Enrichers are isolated.** One misbehaving enricher cannot block others. Each runs in its own tokio task.
- **Enrichers can be disabled.** Each has a config section with `enabled = false` default; operators opt in.
- **Enrichers are idempotent.** Running twice produces the same graph state. Ideally, they MERGE not CREATE.
- **Enrichers log what they did** — last run timestamp, node count touched, relationships added, warnings. Show this in the UI.

**Done when**: trait is defined with at least one real implementation (NetBox — see T3-2), one stub, documentation for authors of new enrichers.

### T3-2 — NetBox enricher (MCP-backed)

**What**: the canonical first enricher. Connects to NetBox via its MCP server (or direct REST — see design decision below). Pulls:

- Device → Site mapping (if NetBox has it, it's richer than what the operator typed in bonsai onboarding)
- Device serial, model, firmware version
- Interface description, connected endpoint, cable ID
- VLAN assignments per interface
- Prefix/subnet assignments
- Platform tags and lifecycle state

Writes to graph as:
- `Device` gets properties `netbox_serial`, `netbox_model`, `netbox_lifecycle`, `netbox_asset_tag`, etc.
- `Interface` gets properties `netbox_description`, `netbox_cable_id`
- New nodes: `VLAN(id, name, description)`, `Prefix(cidr, role, description)` — these are new labels, whitelisted at the trait level
- New relationships: `ACCESS_VLAN`, `TRUNK_VLAN`, `HAS_PREFIX`, `OWNED_BY`

**Design decision — MCP vs direct REST**:
- MCP is compelling because NetClaw/NetAI have proven the pattern and because the enricher becomes reusable across other bonsai-like projects
- MCP adds a process hop (bonsai → MCP server → NetBox REST)
- **Recommendation: support both via a small adapter.** First implementation uses the NetBox MCP server if configured (URL + token); falls back to direct REST if the MCP server isn't reachable. Operators pick the path that fits their environment.

**Where**:
- `src/enrichment/netbox.rs`
- Config: `[enrichment.netbox]` section
- Optional MCP client library: `rmcp` crate (Rust MCP client) if the MCP path is enabled

**Done when**:
- Enricher runs on startup + every 15 minutes (configurable)
- Graph contains VLAN, Prefix nodes with proper edges for a lab-hosted NetBox
- UI shows "last enrichment: 14 minutes ago, 42 nodes touched" in the device details view
- Integration test uses a containerised NetBox

### T3-3 — ServiceNow CMDB enricher

**What**: second canonical enricher. Pulls CMDB records (CI, Business Service, Application) and creates the business-context edges that bonsai's graph is missing today.

**Writes to graph**:
- `Application(id, name, criticality, owner_group)` nodes
- `Device` gets `snow_ci_id`, `snow_owner_group`, `snow_escalation_path` properties
- Edges: `RUNS_SERVICE`, `CARRIES_APPLICATION`

**Why this matters for detection**: once these edges exist, the detect-heal loop can consult them at remediation-selection time: "this BGP session down affects Application 'payment-frontend' which is priority P1 — escalate, do not auto-remediate." That's a direct bridge from the current dry-run/auto-remediate flag to proper business-aware routing.

**Where**: `src/enrichment/servicenow.rs`

**Done when**: ServiceNow MCP server or direct REST integration works; the detection event rendering in UI shows affected business services; an ADR documents the business-context integration model.

### T3-4 — Infoblox/BlueCat enricher (optional)

**What**: a third enricher for environments where IPAM lives outside NetBox. Covers Infoblox and BlueCat as the big two.

**Scope**: just subnet + DNS record data. Same pattern as NetBox (MCP preferred, REST fallback). Do not duplicate NetBox's richer DCIM fields; those come from NetBox.

**Priority**: build this when an operator with Infoblox asks for it. Until then, the design exists (same trait) but no implementation.

### T3-5 — CLI-scraped enricher (the pragmatic one)

**What**: not every environment has a clean IPAM. Some have "the truth is on the router, we SSH in and read show commands." Accept this reality. Build an enricher that SSHes in, runs a curated set of `show` commands, parses them with pyATS or TextFSM, writes structured properties back.

**Why it matters**: NetAI demos heavily lean on "we pull everything from the device itself." For operators without NetBox, CLI-scraping is the alternative path to richness.

**Where**:
- `python/bonsai_enrichment/cli_enricher.py` — Python, not Rust. pyATS/Netmiko are Python-native and mature.
- gRPC plumbing: Python talks to bonsai via the existing API
- A Rust wrapper task that spawns the Python process on schedule or invokes it via subprocess

**Commands to scrape initially** (per vendor):
- Nokia SR Linux: `info from state interface / brief`, `info from state network-instance default / bgp neighbor`
- Cisco IOS-XR: `show interfaces brief`, `show bgp neighbor brief`, `show running-config interface` (for descriptions)
- Arista cEOS: `show interfaces status`, `show bgp summary`
- Juniper: same pattern

**Parsing**: pyATS has `genie` parsers for most `show` commands. For the rest, TextFSM templates.

**Design principles**:
- **CLI enricher runs per-device on demand or on-onboarding.** Not on schedule. gNMI is the schedule-driven source.
- **Output is structured, same graph surface as NetBox enricher** — same VLAN nodes, same Prefix nodes, different source.
- **CLI enricher honours the vault.** Credentials come from the same alias that the gNMI subscriber uses.

**Done when**: onboarding a device can optionally trigger a CLI scrape that produces initial interface descriptions, VLAN mappings, and route table snapshots. UI shows "CLI scrape succeeded 4 minutes ago — 12 interfaces described, 8 VLANs, 24 BGP neighbours."

### T3-6 — NETCONF / RESTCONF enricher

**What**: for environments with NETCONF-capable devices that don't fully expose data via gNMI, NETCONF can fill gaps. Same pattern as CLI — pulls structured data, writes to graph.

**Why NETCONF is worth having even though we're "gNMI only"**:
- Our **gNMI-only rule** applies to the **hot-path telemetry subscription**. NETCONF as a background enrichment source does not break that rule — it decorates the graph with richer structured context.
- Some vendors (older Juniper, some Cisco IOS) expose better NETCONF data than gNMI.
- RESTCONF is a HTTP/JSON equivalent; useful for some cloud-network devices.

**Design**:
- Extends the same `GraphEnricher` trait. Implementation uses a Rust NETCONF client (e.g. the `yang-rs` ecosystem) or delegates to Python (ncclient).
- Runs on explicit operator action or on-onboarding (like CLI enricher).

**Where**: `src/enrichment/netconf.rs` (Rust) or `python/bonsai_enrichment/netconf_enricher.py`

**Done when**: optional enricher exists. Priority below T3-2 and T3-5 because gNMI plus NetBox covers most modern environments.

### T3-7 — Enrichment run visibility in UI

**What**: every enricher's last run, outcome, and what it touched is visible in the UI.

**Where**: new UI workspace `Enrichment` or extension to the device details page

**Design**:
- List of enrichers with `enabled / disabled / errored` status
- Last run time + duration
- What each enricher added to a given device (when viewing device details)
- Manual "Run now" button for each

**Why**: operators need to see *why* a device has certain properties. Without attribution, "netbox_serial=XYZ" is magic. With attribution, it's auditable.

### T3-8 — MCP client infrastructure

**What**: a thin shared module for talking to any MCP server. Handles authentication, tool discovery, tool invocation, caching.

**Where**: `src/mcp_client.rs` (or Python equivalent if we go that route for simplicity)

**Why consolidate**: each enricher would otherwise reinvent MCP plumbing. One shared module means adding a new MCP-backed enricher is "write the mapping from MCP tool response to graph writes," not "reimplement MCP."

**Done when**: the NetBox and ServiceNow enrichers both use this module; a third operator-added enricher (say, Ansible Automation Platform) can be written in under 200 lines by following the existing pattern.

---

## <a id="tier-4"></a>TIER 4 — Syslog and SNMP Traps — Thought-Through Position

Your question was genuine and deserves a careful answer, not a reflexive "gNMI only, so no."

**The gNMI-only rule from v1 is specifically about the hot-path telemetry subscription — the data that flows into the graph as state.** That's where the rule is binding: we do not want SNMP polling or NETCONF get-loops as state sources, because they are inferior to gNMI streaming on every axis (latency, efficiency, protocol discipline).

Syslog and SNMP traps are different. They are **event signals**, not state. A trap says "something happened." A syslog message says "something happened at this line in my software." They are notifications, not continuous state.

**The question is: do they augment bonsai's value?**

**Yes, in two specific ways:**

1. **They catch things gNMI doesn't model.** A CRC error burst on an interface might not change `oper-status`. A memory pressure event might not surface in any gNMI subscription. Syslog carries these. An environment-aware detector watching syslog for patterns like "%LINEPROTO-5-UPDOWN" alongside the gNMI stream gets more signal than gNMI alone.

2. **They're the only signal from legacy devices.** If bonsai is deployed in a mixed environment with some gNMI-less devices, syslog/trap is the only telemetry path.

**But they must not be state sources.** A trap that says "BGP peer down" does not update the `BgpNeighbor` node — the gNMI stream is authoritative for state. The trap becomes a **detection hint** — an additional input to the rule engine, or a trigger for the investigation agent (T7).

### T4-1 — Syslog + trap collector (separate process)

**What**: a new collector role that listens on standard syslog ports (UDP 514 + TCP 6514 for syslog-TLS) and SNMP trap port (UDP 162), parses incoming messages, and publishes them to a new event-bus category: `ExternalSignal`.

**Where**:
- New binary: `bonsai-signal-collector` (or a runtime mode `signal-collector` on the existing binary)
- New module: `src/signals/mod.rs` with `syslog.rs` and `snmp_trap.rs`
- Proto: new message types for signals, a new `SignalIngest` RPC (similar to `TelemetryIngest`)

**Design**:
- Signal collector is a **separate process from telemetry collector** because it listens on privileged ports (162, 514) and has different failure modes
- Parses into a common `Signal` proto: `{ source_address, severity, facility, message, timestamp, vendor_tag, parsed_fields: Map<string, string> }`
- Forwards signals to core via the new RPC
- Core republishes signals on the event bus

**Do NOT attempt to**:
- Make signals state-like. A trap does not create or modify graph nodes directly.
- Replace gNMI with syslog "because it's easier." That would undermine the architecture.

**Done when**: the signal collector receives syslog from a ContainerLab device in the lab and pushes structured signals to the core, where they are visible in the UI and queryable.

### T4-2 — Signal-aware detectors

**What**: a detector that consumes both gNMI-derived events *and* signals, producing higher-confidence detections when signals confirm what gNMI is saying.

**Example**: BGP session change (gNMI) + BGP-5-ADJCHANGE syslog from same device within 10s = "bgp_session_down_confirmed" — higher confidence than either alone.

**Where**: extension to the existing detector framework in the Python SDK

**Done when**: a BGP flap test produces both a gNMI-only detection (today) and a dual-source detection (new) with a confidence score difference.

### T4-3 — Signal-triggered investigation

**What**: when a syslog signal has no corresponding gNMI state change, it's worth investigating. Route such orphan signals to the investigation agent (T7) as a trigger condition.

**Why**: this is exactly the hybrid model that makes bonsai's architecture coherent. gNMI-only state + signal-enriched detection + agent-driven investigation on anomalies. Each protocol does what it's best at.

**Done when**: a syslog signal arriving without a corresponding gNMI state transition triggers an agent investigation, with human approval gate before any remediation.

### T4-4 — SNMP trap MIB handling (minimal)

**What**: parse SNMP traps using MIB files. Don't try to be a full SNMP manager — just decode common vendor MIBs (Cisco, Juniper, Arista, Nokia) and map key trap OIDs to structured signals.

**Design**:
- Bundle a small set of common MIBs in `config/mibs/`
- Use an SNMP library like `snmp` or `netsnmp-rs` for OID decoding
- Unknown OIDs get logged as raw OID → hex pairs; operator can add MIBs later

**Done when**: an `linkDown` trap from Cisco becomes a structured `{ type: "link_down", interface: "GigabitEthernet0/1", timestamp: ... }` signal.

### T4-5 — Syslog format discipline

**What**: syslog is a mess. RFC 3164, RFC 5424, vendor-specific formats. Use an established parser (e.g. `syslog-loose` or the `rsyslog` parsing library) and accept that some messages will have `parsed_fields: {}` and only a raw `message`. That's fine.

**Done when**: the syslog collector handles the top 3 formats (RFC 3164, RFC 5424, Cisco CLI-style) and gracefully degrades for anything else.

---

## <a id="tier-5"></a>TIER 5 — UI Usability Pass for Network Practitioners

The v3 wizard landed beautifully for the address-and-credentials flow. But the broader UI still reflects its origin as a "demo view." Network practitioners have specific workflows and mental models that the UI should match.

### T5-1 — Workflow-centric navigation

**What**: today the nav is `Topology / Onboarding / Events`. Operators think in workflows: "I need to see what's broken right now," "I need to bring up a new device," "I need to see what changed recently," "I need to investigate an incident." Reshape navigation around these.

**Proposed navigation**:
- **Live** — replaces Topology. A split view: interactive topology on the left, detection feed on the right. This is what an operator stares at during an incident. Health-coloured nodes. Click a node, see its details + recent detections + current subscription status in a drawer. This is the default landing page.
- **Incidents** — replaces the trace drill-down. A list of open and recent detection events with their remediation state, time-to-healed (if closed), and links to the full trace. Sort and filter by severity, rule, device, site.
- **Devices** — the current onboarding + managed-devices workspace, but renamed. Wizard stays Step-1 through Step-4 for adding/editing; managed list stays for inventory.
- **Enrichment** — new workspace from T3. Shows enricher status and what each has contributed to the graph.
- **Investigations** — the agent's workspace from T7 (when it lands).

### T5-2 — Network-practitioner topology view improvements

**What**: the current topology is an abstract force-directed graph. That's fine for visualisation but not for operational reasoning. Add the things network practitioners actually use.

**Specific items**:
- **Layer filter** — show Layer 3 only (BGP + interfaces), Layer 2 only (LLDP + interfaces), or all. Toggle.
- **Site scope** — filter topology to one site or one parent-site's subtree, leveraging the new Site graph. "Show me just lab-london."
- **Role colouring** — leaves, spines, PEs, P-routers visually distinguishable by shape or hue in addition to health colour.
- **Link utilisation heatmap** (the NetAI screenshot feature #02) — colour links by recent traffic delta. Data is already there in interface counters.
- **Path tracing** — click device A, shift-click device B, see the graph-derived path between them highlighted. Cypher shortest-path is a one-line query.
- **Sticky node details** — clicking a node opens a side panel with recent state changes and subscription status, not a modal that blocks the view.

### T5-3 — Incident-centric UI

**What**: network engineers during an incident want to see the blast radius. Detection events are displayed linearly today. Render them as a grouped view.

**Design**:
- Events within 30 seconds and reachable through graph traversal group into an incident card
- Incident card shows root detection (highest-upstream in topology), cascading detections, affected devices, affected sites, affected applications (if T3-3 ServiceNow enricher is enabled), ongoing remediation status
- Click into the incident to see the full trace tree (matches NetAI screenshots #10-12)

**Done when**: a BGP session flap that triggers a cascade of three downstream detections renders as one incident card with a causal tree, not three separate rows.

### T5-4 — Credential and vault UX improvements

**What**: small polish on the vault UX in the wizard.

**Items**:
- **Vault lock/unlock banner** — when locked, a clear banner explains "Vault locked — restart bonsai with `BONSAI_VAULT_PASSPHRASE` to add aliases." With a copy-to-clipboard button for the instruction.
- **Test credential** button on the alias — dial the discovery endpoint against a test device to confirm the credential works, before the operator saves a real device.
- **Credential usage view** — "this alias is used by 4 devices" shown next to each alias. Removing an alias while devices use it prompts explicitly.

### T5-5 — Site UX improvements

**What**: the wizard lets operators pick or create sites, but site management is minimal. Expand it.

**Items**:
- **Site tree view** — visualise the PARENT_OF hierarchy. Regions → DCs → racks. Drag-drop reparenting.
- **Site details page** — list of devices in the site, aggregate health, recent detections scoped to that site, site-wide subscription status summary.
- **Geographic map view** (optional, defer to T8-11) — sites with lat/lon render as markers on a world map. Matches NetAI screenshot #05.

### T5-6 — Accessibility and responsiveness

**What**: pragmatic UI basics.

**Items**:
- Keyboard navigation for the wizard (Tab progresses, Enter advances, Esc goes back)
- Dark mode respects `prefers-color-scheme` (it's dark-only today)
- Mobile-ish responsiveness for the incidents view — on-call engineers check incidents on phones
- Copy-friendly selectors on code/config/path fields

### T5-7 — Operator observability

**What**: what is bonsai doing *right now*? The current UI answers "what's the network state" — not "what's bonsai doing."

**Items**:
- **System status panel** — event bus backpressure, archive lag, subscriber count, active SSE client count, enricher run times
- **Audit log view** — who added/removed what device, when, with what changes (driven by registry change events)
- **Health dashboard** — expected subscription paths observed vs silent, per-device

**Done when**: an operator troubleshooting "bonsai feels slow" has a single place to look.

---

## <a id="tier-6"></a>TIER 6 — Path A Graph Embeddings → Path B GNN

Unchanged from v3's strategic direction. Carried forward.

**Sequencing discipline remains binding**: Path A before Path B. Operational infrastructure — archive running for months, chaos continuing, enrichers feeding the graph — before starting Path B. GNN work must not eat operational or UI work.

### T6-1 — Path A: Graph embeddings stepping stone

Same design as v3 T3-A. Node2vec/GraphSAGE over the graph, embeddings as additional features on existing tabular ML. Now with a crucial amplification: the enrichment from Tier 3 means the graph has richer structure (VLANs, Applications, etc.), which makes embeddings more informative.

Done when: embeddings-augmented Model A beats baseline on chaos-run evaluation, especially on cascading failures. ADR documents hyperparameters.

### T6-2 — Path B: Proper GNN with message passing

Same design as v3 T3-B. PyTorch Geometric or DGL. Node-level task (score Device nodes for anomaly). Trained on months of archive data from all collectors. Coexists with rules and MLDetector.

Done when: GNN catches at least one cascading-failure class that rules and MLDetector miss. Model card exists. ADR documents the architecture.

### T6-3 — GNN-aware enrichment gate

**What**: some enriched properties are structural (VLAN membership), some are categorical (lifecycle state, owner group). For the GNN, categorical properties need embedding. Build a small schema that tags each enrichment property with its type (numeric, categorical, text, timestamp) so the GNN data loader can do the right thing.

**Where**: `src/enrichment/schema.rs` — a registry of expected property types

**Done when**: the GNN training loader handles all enrichment properties without hand-coding per-property feature extraction.

---

## <a id="tier-7"></a>TIER 7 — Investigation Agent

Unchanged in spirit from v3. Sharpened sequencing: depends on Tier 3 enrichment because the agent is far more useful when it has business-context graph edges to reason over.

### T7-1 — Agent scaffolding

LangGraph-based investigation agent outside the detect-heal hot path. Triggered by: (a) unmatched detection after 60s, (b) orphan syslog signal (T4-3), (c) explicit operator `/investigate <id>`.

Tools — with the enrichment tier, the toolset becomes richer:
- `query_graph(cypher: str)` — read-only
- `get_topology_context(device_address: str)` — neighbours, role, site, recent states
- `get_business_context(device_address: str)` — applications, services, owners, escalation path (enabled by T3-3 ServiceNow enricher)
- `get_recent_detections(window_seconds: int)`
- `get_playbook_library()`
- `suggest_playbook(detection_id, rationale)` — writes proposal, does not execute
- `summarise(text: str)`

Mandatory human approval gate before any agent-proposed action executes.

### T7-2 — Agent UI workspace

Same as v3 T4-2. List of investigations, pending proposals, completed history.

### T7-3 — Agent memory

Same as v3 T4-3. PastInvestigation nodes retrieved as context for future investigations.

### T7-4 — Agent cost controls

**What**: new item — token budget per investigation (fail-closed if exceeded), daily token budget per operator, visible cost per investigation in the UI. Anthropic API token usage reported to Prometheus metrics.

**Why**: agent work is genuinely non-trivial in cost terms. Without budget controls it's easy to rack up a surprise bill.

**Done when**: running 10 investigations in a day, the operator can see cumulative cost and per-investigation cost.

---

## <a id="tier-8"></a>TIER 8 — Carryover Extensions

v3 T5 items that remain.

- **T8-1** NL query layer (v3 T5-1)
- **T8-2** ML feature schema versioning (v3 T5-2)
- **T8-3** Remainder of training script validity checks (v3 T5-3) — some done, some remaining
- **T8-4** Bitemporal schema (v3 T5-4) — deferred until forced
- **T8-5** Metrics remainder (v3 T5-5)
- **T8-6** Schema migration path (v3 T5-6) — deferred
- **T8-7** Grafeo migration readiness (v3 T5-8)
- **T8-8** TSDB integration adapter (v3 T5-9) — see note below
- **T8-9** Bulk onboarding CSV (v3 T5-10)
- **T8-10** Geographic map UI (v3 T5-11) — implied by T5-5 now
- **T8-11** Multi-layer topology UI (v3 T5-12) — implied by T5-2 now
- **T8-12** Expanded lab topologies (v3 T5-13)

**TSDB adapter rethink given v4 enrichment layer**: with enrichment landing, the TSDB-enriched-labels thesis from v2 becomes much more powerful. Instead of just `{device, role, site}` labels, the TSDB metric now carries `{device, role, site, application, owner_group, criticality, vendor_lifecycle}`. That's Grafana dashboards filtered by business service — which is genuinely differentiated. Keep TSDB adapter in backlog; its value increased.

---

## <a id="execution-order"></a>Recommended Execution Order

### Sprint 1 — Loose ends + containerisation prep (1-2 weeks)
1. T0-1 credential metadata write debounce
2. T0-2 vault passphrase rotation ADR
3. T0-3 archive format doc
4. T0-4 CLI device commands
5. T0-5 site depth guard
6. T1-1 Dockerfiles (foundation)

### Sprint 2 — Containerised dev experience (1-2 weeks)
7. T1-2 Docker Compose profiles
8. T1-3 ContainerLab integration
9. T1-4 container secrets handling
10. T1-5 chaos in containers
11. T2-1 scale architecture doc
12. T2-2 collector-local archive operational doc

### Sprint 3 — Enrichment foundation (2-3 weeks)
13. T3-1 `GraphEnricher` trait
14. T3-8 MCP client infrastructure
15. T3-7 enrichment visibility in UI
16. T3-2 NetBox enricher — the flagship implementation

### Sprint 4 — Second-wave enrichment + scale validation (2 weeks)
17. T3-3 ServiceNow CMDB enricher
18. T3-5 CLI-scraped enricher
19. T2-4 core bottleneck profiling
20. T2-5 multi-collector validation

### Sprint 5 — Signals and detection richness (2 weeks)
21. T4-1 syslog + trap collector
22. T4-5 syslog format discipline
23. T4-4 SNMP trap MIB handling
24. T4-2 signal-aware detectors

### Sprint 6 — UI pass for practitioners (2 weeks)
25. T5-1 workflow-centric navigation
26. T5-2 topology improvements (layer filter, site scope, role colouring, link heatmap, path tracing)
27. T5-3 incident-centric UI
28. T5-4 vault UX polish
29. T5-5 site UX
30. T5-7 operator observability

### Sprint 7 — Path A (1-2 weeks)
31. T6-1 graph embeddings, augmented with enrichment features
32. T8-2 ML feature schema versioning (required before embeddings ship)

### Sprint 8 — NL query (1 week)
33. T8-1 NL query layer

### Sprint 9 — Investigation agent (2-3 weeks)
34. T7-1 agent scaffolding
35. T7-4 agent cost controls
36. T7-2 agent UI workspace
37. T4-3 signal-triggered investigations (depends on agent)

### Sprint 10 — Path B (3-4 weeks)
38. T6-2 GNN implementation
39. T6-3 enrichment-aware GNN data loader
40. T7-3 agent memory across investigations

### Longer horizon
41. T3-4 Infoblox enricher (when demanded)
42. T3-6 NETCONF enricher (when demanded)
43. T2-3 S3-compatible archive
44. T5-6 accessibility pass
45. T8-8 TSDB adapter (now with enriched labels)
46. T8-9 bulk onboarding CSV
47. T8-10 geographic map
48. T8-11 multi-layer topology
49. T8-12 expanded lab topologies

### Defer until forced by pain
- T8-4 bitemporal schema
- T8-6 schema migration path
- T8-7 Grafeo evaluation

---

## <a id="merge-plan"></a>Branch Merge Plan — `codex-t1-1-credentials-vault` → `main`

The branch is sizeable but cohesive. Merge in stages, biased toward earlier-merging the less-risky pieces.

### Stage 1 — Small fixes directly to main (separate PR per small item)
- T0-1 credential metadata debounce (can be started before branch merge)
- Any clippy fixes picked up in the review

### Stage 2 — Archive + ingest + queue (1 PR)
- `src/archive.rs` updates (append-to-hour)
- `src/ingest.rs` MessagePack + disk queue + mTLS (cohesive)
- Proto updates
- `docs/distributed_tls.md`
- `docs/distributed_validation.md`

Rationale: these three are conceptually "distributed transport hardening" and should ship together.

### Stage 3 — Credentials vault (1 PR, careful review)
- `src/credentials.rs`
- Vault wiring in `api.rs`, `main.rs`, `config.rs`
- `bonsai.toml.example` updates
- ADRs

Rationale: isolated module with clear boundary. Cryptographic code earns its own review.

### Stage 4 — Site as graph entity (1 PR)
- `src/graph.rs` schema additions
- `sync_sites_from_targets` migration
- API surface for sites
- HTTP endpoints

Rationale: schema change, needs care but the migration path is clean.

### Stage 5 — Onboarding wizard (1 PR)
- `ui/src/lib/Onboarding.svelte` rewrite
- HTTP endpoints that back it
- SSE lifecycle events from the server side

Rationale: UI changes are low-risk to merge; known-good once build + Lighthouse audit pass.

### Stage 6 — Build plumbing (1 PR)
- `build.rs` dynamic-LBUG handling
- Cargo.toml zstd + age + rmp_serde additions
- Python generated stubs

Rationale: last because it's least risky once everything depending on it is in.

### After all stages
- Tag `v0.4.0`
- Delete branch
- Release note summarising T0-* v3 items, T1-1/2/3 v3 items, T2-1/2-2/2-3/2-4 v3 items

---

## <a id="guardrails"></a>Guardrails — Binding Through v4

### Architectural invariants
- gNMI only for hot-path telemetry **state**. Syslog and traps are allowed as **signals** (Tier 4), never as state sources.
- tokio only for async Rust.
- Credentials never leave the Rust process except on the outbound gNMI/NETCONF/SSH connection.
- No Kubernetes in v0.x. Docker + docker-compose are fine; Kubernetes is a v1.x conversation.
- No fifth vendor until the four vendor families work vendor-neutrally end-to-end.
- Every non-trivial decision gets an ADR at commit time.

### Hot-path determinism
- The detect-heal loop does not call an LLM. Ever.
- The detect-heal loop does not call MCP, NETCONF, CLI, or any enrichment source synchronously. Enrichment is background.
- Detection latency target stays sub-second.
- If the Anthropic API or any MCP server is unreachable, bonsai still detects and heals.

### UI discipline
- Phase 6 UI is view + onboarding + enrichment-visibility. No arbitrary config push, no auth/RBAC, no admin panels.
- Workflow-centric navigation serves network practitioners, not dashboards for management.

### Enrichment discipline (new in v4)
- Enrichers write via a restricted graph surface — they cannot invent node labels outside the registered whitelist.
- Enrichers are idempotent, isolated, and opt-in.
- Enrichers never gate the hot-path.
- Enricher output is namespaced (`netbox_*`, `snow_*`) so source attribution stays clear.

### Scale discipline (new in v4)
- Collectors scale horizontally. Core scales vertically in v1.
- Graph sharding is a v2 conversation forced by real data volume.
- Archive is collector-local; central storage is an add-on, not a requirement.
- Stat with Docker Compose; Kubernetes-ready image but not Kubernetes-ready deployment in v0.x.

### ML discipline
- Tabular ML remains the production path until the GNN has honest validation.
- GraphML work does not eat operational or enrichment work.
- GNN training requires months of real data; no synthetic-data shortcuts.

### Anti-patterns to reject
- "Let's use SNMP polling for state" — no, traps as signals only.
- "The UI could grow into a management product" — no.
- "Let's deploy this on Kubernetes now" — no.
- "A fifth vendor would be cool" — no.
- "Let's skip ADRs for the small stuff" — no.
- "Let's have the agent run without human approval" — no.
- "Enrichers can also run in the hot path, it's fast enough" — no, ever.

---

## What v4 Explicitly Excludes

For scope discipline, do not start:
- Auth/RBAC of any kind
- Multi-tenancy in the graph
- Production HA for the core (leader election, graph replication)
- Universal vendor playbook coverage outside the four vendor families
- A competing source-of-truth product (the NetBox replacement)
- Online/continual ML learning
- Multi-GPU GNN training
- A fifth vendor before the existing four are vendor-neutral end-to-end
- Agent-driven autonomous remediation without human approval
- Kubernetes deployment manifests (stay with Compose)

---

*Version 4.0 — authored 2026-04-22 after reviewing the `codex-t1-1-credentials-vault` branch. Reflects genuine execution progress on v3 strategic direction (credentials vault, Site as graph entity, 4-step onboarding wizard, archive append-to-hour, MessagePack wire, zstd + mTLS + disk queue + live validation all landed). Adds containerisation/scale architecture, MCP-driven enrichment, syslog/trap handling, UI usability pass, and sharpens sequencing for GNN and investigation-agent work that remains.*

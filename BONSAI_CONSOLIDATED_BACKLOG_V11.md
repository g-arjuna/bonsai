# BONSAI — Consolidated Backlog v11.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V10.md`. Produced 2026-05-03 after deep code review of post-v10 main.
>
> **Document discipline for v11**: prior backlogs (v2 through v10) remain in the repo as reference. They capture the strategic thinking on audience, controller-less framing, gNMI-only hot path, enrichment philosophy, AIOps positioning, HIL graduation, OutputAdapter architecture, and the v10 testing tier. **v11 references those rather than repeating them**, so this document can spend its real estate on the four new threads:
>
> 1. **Standardised external infrastructure** — every external dependency (Elasticsearch, Splunk, NetBox, ServiceNow PDI, Prometheus) brought up cleanly via one command, pre-seeded, with credentials and URLs documented in `.env.example`. Ends the "burn AI cycles on environment setup" loop.
> 2. **A comprehensive ContainerLab** — DC and SP topologies covering the full feature surface bonsai needs to test (EVPN, L3VPN, SR-MPLS, SR-TE, SRv6, multi-area IGP, route reflectors). Slim, container-friendly NOSes only, picked for laptop-scale.
> 3. **Iterative AI-friendly feedback loop** — graduate from pytest to a programmatic UI/API/chaos harness that feeds the AI structured signal about what's broken vs working at every stage. The bottleneck today is the AI not knowing whether a feature works without being told.
> 4. **Memory and database hygiene** — a 9 GB memory growth observed in operation is a credibility problem for a "slim Rust core" pitch. Profile, fix, monitor; cap database growth to available disk; verify compression is best-in-class.
>
> **What v11 does not do**: add new functional features beyond these four threads. v9 strategic threads (controller adapters, full enrichment maturity, NL query, GNN, investigation agent) all remain queued for after v11.

---

## Table of Contents

1. [Audience and Positioning](#positioning) — see v7-v10
2. [Progress Since v10](#progress)
3. [TIER 0 — Loose Ends from v10 Review](#tier-0)
4. [TIER 1 — Standardised External Infrastructure](#tier-1) ⚡ new ⚡
5. [TIER 2 — Comprehensive ContainerLab](#tier-2) ⚡ new ⚡
6. [TIER 3 — Iterative AI-Friendly Feedback Loop](#tier-3) ⚡ new ⚡
7. [TIER 4 — Memory and Database Hygiene](#tier-4) ⚡ new ⚡
8. [TIER 5 — Carryover from v9/v10](#tier-5)
9. [Execution Order](#execution-order)
10. [Guardrails](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7–v10.** Primary target controller-less environments (DC, campus wired/wireless, SP backbones); secondary multi-controller correlation. AIOps integration as upstream feeder, not replacement. See `BONSAI_CONSOLIDATED_BACKLOG_V7.md` for the full rationale.

---

## <a id="progress"></a>Progress Since v10 — Verified Against Main

v10 was a testing/quality consolidation iteration. The progress is substantial — every previously-zero-tests module from v9 now has tests, and the integration test infrastructure is real.

| v10 item | Status | Evidence |
|---|---|---|
| T0-1 v10 NetBox enricher correctness pack | ✅ Done | `src/enrichment/netbox.rs` shows fixes for Q-1 through Q-5 |
| T0-2 v10 seed_servicenow_pdi.py correctness | ✅ Done | `scripts/seed_servicenow_pdi.py` updated |
| T0-3 v10 TrustStore correctness | ✅ Done | `src/remediation/trust.rs` 11 tests, fixes Q-9, Q-10, Q-11, Q-12 |
| T0-4 v10 ServiceNow enricher robustness | ✅ Done | 8 tests in `src/enrichment/servicenow.rs` |
| T0-5 v10 path discovery preflight | ✅ Done | `scripts/discover_yang_paths.py` modified |
| T1-1..T1-9 v10 Rust unit tests | ✅ Done | netbox 8, servicenow 8, trust 11, graduation 6, rollback 7, servicenow_em 6, mcp_client 7, enrichment::mod 8 — every previously-zero-tests v9 module now has tests |
| T1-9 v10 Python script tests | ✅ Done | `python/tests/test_seed_servicenow_pdi.py`, `test_discover_yang_paths.py`, `test_gen_profile_docs.py`, `test_coverage_gap_fill.py` |
| T2-1 v10 Docker compose e2e | ✅ Done | `scripts/e2e_compose_test.sh` |
| T2-2 v10 ContainerLab e2e | ✅ Done | `scripts/e2e_containerlab_test.sh`, results in `docs/test_results/e2e_containerlab/` |
| T2-3 v10 NetBox enricher live | ✅ Done | `scripts/e2e_netbox_enricher_test.sh` |
| T2-7 v10 Output adapter live | ✅ Done | `scripts/e2e_output_adapters_test.sh`, results captured |
| T2-8 v10 Path profile validation | ✅ Done | `scripts/e2e_path_validation_test.sh`, results captured |
| T4-4 v10 cargo-deny | ✅ Done | `deny.toml` present |
| T4-7 v10 proto compat check | ✅ Done | `proto/buf.yaml` present |
| T5-1 v10 test results discipline | ✅ Done | `docs/test_results/TEMPLATE.md` + `SPRINT4_E2E_SUMMARY.md` + per-feature subdirs |

**Not yet done (carry forward to v11/later):**
- T2-4 v10 ServiceNow PDI live test — operator must provide PDI URL/credentials
- T2-5 v10 ServiceNow EM push live — same dependency
- T2-6 v10 HIL e2e
- T3-* v10 UI/API contract tests + Playwright + accessibility
- T4-1 v10 coverage measurement
- T4-2 v10 Python tests in CI
- T4-3 v10 nightly integration in CI
- T4-5 v10 cargo-mutants
- T4-6 v10 cargo nextest
- T5-3 v10 test results dashboard route

These are valid items but secondary to the four v11 threads. They reappear in Tier 5 v11 carryover or are subsumed by the new tiers (e.g. T3 v11 expands T3-* v10 substantially).

---

## <a id="tier-0"></a>TIER 0 — Loose Ends from v10 Review

Genuinely small. The v10 work was disciplined; few new bugs surfaced.

### T0-1 (v11) — Verify v10 Tier 0 fixes against the assertions

The v10 Tier 0 was specifically designed to fix Q-1 through Q-18 from the v10 review (NetBox `edges_created: 0`, pagination offset bug, SecretString handling, concurrent fetch cap, transaction boundary, seed-script vault vs env confusion, etc.). Code shows the modules were touched but the v11 review did not re-verify each fix line-by-line.

**Action**: as part of Sprint 1 for v11, walk through Q-1..Q-18 from v10 and confirm each is fixed in code. If any are not actually fixed, capture them as v11 T0 bugfixes.

**Where**: review session, results to `docs/test_results/v10_tier_0_verification.md`.

### T0-2 (v11) — Promote test results into nightly CI signal

The `docs/test_results/` directory pattern is established. Today the e2e scripts produce dated artifacts there but nothing alerts when they regress. Cheap fix that compounds value of the work that already landed.

**Where**: `.github/workflows/nightly-integration.yml`.

**Done when**: nightly run captures e2e results and posts a summary issue if any fail.

### T0-3 (v11) — Run Python tests in CI

v10 T4-2 carryover. Python tests exist; CI doesn't run them. One-line workflow addition.

---

## <a id="tier-1"></a>TIER 1 — Standardised External Infrastructure

**Why this is Tier 1**: you stated explicitly that you waste AI request/response cycles on environment setup. That's the most expensive bug in the project right now — not in the code, in the developer loop. Fix it once, recover hours.

The principle: **everything an external integration needs to run end-to-end should be `cp .env.example .env && docker compose --profile <name> up -d` — no further setup**. Pre-seeded with data that matches the lab. Credentials and URLs documented.

### T1-1 — `compose-external.yml` umbrella with profiles

**What**: a single `docker/compose-external.yml` that defines profiles for every external dependency:

- `netbox` — NetBox + Postgres + Redis (already exists in `docker/compose-netbox.yml`; absorb)
- `splunk` — Splunk Enterprise free trial (single container, `splunk/splunk:latest`)
- `elastic` — Elasticsearch + Kibana
- `prometheus` — Prometheus + Grafana with bonsai dashboard pre-loaded
- `servicenow-pdi` — *not a container* (PDIs are cloud-hosted) but a profile that runs the seed script against the operator-supplied PDI

**Where**: `docker/compose-external.yml` + `.env.example` + `docs/external_infra.md`.

**`.env.example` must contain**:
```bash
# Core
BONSAI_VAULT_PASSPHRASE=

# NetBox (auto-set by compose; do not edit unless customising)
NETBOX_API_TOKEN=

# ServiceNow PDI (operator-provided; PDIs are personal cloud instances)
SNOW_INSTANCE_URL=https://devXXXXX.service-now.com
SNOW_USERNAME=admin
SNOW_PASSWORD=

# Splunk
SPLUNK_PASSWORD=
SPLUNK_HEC_TOKEN=

# Elasticsearch
ELASTIC_PASSWORD=
```

**Done when**: `cp .env.example .env && docker compose -f docker/compose-external.yml --profile all up -d` brings up NetBox, Splunk, Elastic, Prometheus, Grafana — all healthy, all reachable from bonsai-core via the `bonsai-mgmt` network, all pre-seeded with lab-matching data, with documented localhost URLs in `docs/external_infra.md`.

### T1-2 — Pre-seed every external service from the lab single-source

**What**: `scripts/seed_external.sh` orchestrator runs all the existing seed scripts in order:
- `seed_netbox.py` (already exists)
- `seed_servicenow_pdi.py` (operator runs explicitly with PDI creds)
- `seed_splunk.py` (new) — creates HEC token, indexes, sample data
- `seed_elastic.py` (new) — creates index template, sample documents

All seeds drive off `lab/seed/topology.yaml` (the single source already established in v8 T3-4) so external state is consistent with whatever ContainerLab is running.

**Where**: `scripts/seed_external.sh`, `scripts/seed_splunk.py`, `scripts/seed_elastic.py`.

**Discipline**:
- Idempotent (safe to re-run; v10 already established this for NetBox and PDI)
- Verifies after seeding (re-fetches and compares; covers the v10 Q-7 fix)
- Reports a summary line per service: `[seed] netbox: 4 devices, 3 sites, 12 VLANs, 8 prefixes — OK`

**Done when**: a fresh run of `scripts/seed_external.sh` populates every external service consistently with the lab topology in under 60 seconds.

### T1-3 — Bonsai-side configuration auto-generated

**What**: a one-shot script that reads `.env` and the running compose state, then writes appropriate `[enrichment.*]` and `[output.*]` sections into `docker/configs/core.toml.generated`. The operator never hand-edits enrichment URLs or HEC tokens.

**Where**: `scripts/configure_external.sh`.

**Done when**: after `seed_external.sh`, running `configure_external.sh` produces a core.toml that bonsai can use to enable NetBox enricher + Splunk adapter + Elastic adapter + Prometheus adapter + ServiceNow EM (if PDI configured) without operator editing.

### T1-4 — Service health probes and readiness contract

**What**: `scripts/check_external.sh` runs after compose-up and asserts each external service is *seeded* and *bonsai-reachable*, not just running. Output is machine-readable JSON for AI consumption.

```json
{
  "netbox":   {"reachable": true, "seeded": true, "device_count": 4},
  "splunk":   {"reachable": true, "hec_token_valid": true},
  "elastic":  {"reachable": true, "index_present": true},
  "prometheus": {"reachable": true, "scraping_bonsai": false},
  "servicenow_pdi": {"reachable": false, "reason": "SNOW_INSTANCE_URL not set"}
}
```

**Where**: `scripts/check_external.sh`.

**Why JSON output**: ties into Tier 3 — the AI consumes this output to know what state the environment is in without an operator narrating it.

**Done when**: an AI agent or human runs the script and gets a clear status of every dependency. Failures are actionable (the `reason` field).

### T1-5 — Documentation with the one-command bring-up

**What**: `docs/external_infra.md` contains exactly one example flow:
```bash
cp .env.example .env
# edit .env to set passphrase + (optionally) SNOW_*
scripts/generate_compose_tls.sh
docker compose -f docker/compose-external.yml --profile all up -d
scripts/seed_external.sh
scripts/check_external.sh
docker compose -f docker-compose.yml --profile two-collector up -d
```

**Done when**: a new contributor (or AI session) can follow this and have a fully-loaded test environment in 10 minutes.

---

## <a id="tier-2"></a>TIER 2 — Comprehensive ContainerLab

**Why this is Tier 1-equivalent**: today's lab is `bonsai-phase4` (4 devices: 3 SR Linux + 1 Cisco XRd) running BGP + OSPF + an SR-MPLS stub. SR-MPLS is commented as "for future SP rule testing" and never wired through. There is no EVPN, no L3VPN, no SR-TE, no SRv6, no multi-area IGP, no route reflector hierarchy, no BFD, no anycast gateway. Bonsai cannot be claimed to handle DC + SP + campus comprehensively until the lab actually exercises those features.

**Constraints from your message**:
- Slim, feature-rich, container-friendly NOSes (laptop scale)
- Pre-built configs (no operator CLI troubleshooting)
- DC topology + SP topology, separate
- All advanced features actually configured

**NOS selection (recommendation)**:
- **Nokia SR Linux** — already in use; ~600 MB image, 1-2 GB RAM/container, full EVPN/SRv6/SR-TE coverage. Keep as DC primary.
- **FRR (Free Range Routing) with patched containers** — 50-100 MB, 256 MB RAM/container, supports BGP/OSPF/IS-IS/EVPN/SRv6 in current versions. Best for SP P routers and route reflectors.
- **Juniper cRPD** — slim control-plane container (~500 MB), full Junos routing stack including L3VPN/EVPN/SR-TE. Good for SP PE.
- **Arista cEOS** — keep as DC mid-weight, ~1.5-2 GB RAM. Feature-complete.
- **Cisco XRd** — currently in use but **24.4.2 is heavyweight** (4-6 GB RAM/container). For laptop-scale, *replace* with cRPD or FRR for most SP roles, retain XRd only as one anchor PE if Cisco-specific telemetry quirks need testing.

### T2-1 — DC topology: 3-tier spine-leaf with EVPN + SRv6

**What**: `lab/dc/dc-evpn-srv6.clab.yml` — full EVPN/VXLAN DC fabric.

**Topology**:
```
                     [super1]   [super2]    (super-spines, BGP unnumbered)
                       |    \   /    |
                       |     \ /     |
                       |    [spine1]-[spine2]  (spines)
                       |   /   |   \   |
                  [leaf1]----[leaf2]----[leaf3]----[leaf4]
                       \      |      \      |
                        \     |       \     |
                       [host1] [host2] [host3] [host4]   (TRex/iperf containers)
```

**Devices**: 4 leaves + 2 spines + 2 super-spines = 8 NOSes + 4 traffic generators. SR Linux for all NOS roles (lightweight, full EVPN).

**Features configured at startup (no CLI required)**:
- BGP unnumbered between leaves and spines (RFC 5549)
- EVPN type-1 (Ethernet auto-discovery), type-2 (MAC/IP), type-3 (multicast), type-5 (IP prefix)
- VXLAN encap with anycast gateway on leaves
- L3VPN tenant (Tenant-A with two VRFs)
- BFD on all uplinks (sub-second detection)
- IS-IS multi-level for underlay (tests multi-area parsing)
- SRv6 micro-SID transport for inter-pod traffic
- Loopback addressing scheme documented in `lab/dc/README.md`

**Where**: `lab/dc/dc-evpn-srv6.clab.yml`, `lab/dc/configs/*.cfg`, `lab/dc/seed/topology.yaml` (extends single-source seed).

**Done when**: `containerlab deploy -t lab/dc/dc-evpn-srv6.clab.yml` produces a 12-container topology that boots fully configured, all BGP sessions established, EVPN routes propagated, SRv6 reachability verified by ping6 between pods. Configs survive `containerlab destroy` and redeploy.

### T2-2 — SP topology: PE-P-RR with SR-MPLS, SR-TE, RSVP-TE

**What**: `lab/sp/sp-mpls-srte.clab.yml` — provider edge + core + route reflectors.

**Topology**:
```
              [pe1]====[p1]====[p2]====[pe2]    (P/PE backbone, IS-IS L2)
              /         |       |        \
             /          |       |         \
        [ce1]         [rr1]   [rr2]      [ce2]  (route reflectors, BGP)
                                            \
                                             [pe3]  (asymmetric peering)
```

**Devices**: 3 PE + 2 P + 2 RR + 2 CE = 9 NOSes. Mix Juniper cRPD for PE (L3VPN), FRR for P routers (SR-MPLS transit), SR Linux for RR (BGP scale), small FRR CEs for traffic injection.

**Features configured at startup**:
- IS-IS Level 2 underlay
- LDP and RSVP-TE coexisting (LDP on most links, RSVP-TE for engineered paths)
- SR-MPLS with prefix SIDs on every node loopback
- SR-TE policy (one explicit-path policy from PE1 to PE2 via P1)
- L3VPN with two VRFs (tenant-A, tenant-B), iBGP RR-driven route distribution
- BGP-LU between PE and CE for inter-AS option B
- BFD on all backbone links
- mLDP for multicast (optional, behind feature flag)

**Done when**: `containerlab deploy -t lab/sp/sp-mpls-srte.clab.yml` produces a 9-container SP backbone with all sessions up, LDP and RSVP-TE LSPs established, SR-TE policy active, L3VPN ping working between CE1 and CE2 in tenant-A.

### T2-3 — Campus topology (deferred to v12 unless tight)

**What**: a campus topology (access/distribution/core, with one wireless controller stub) — out of v11 scope unless the SP and DC topologies prove easy. Most campus telemetry is actually a subset of DC EVPN, so the marginal value is lower.

**Where**: noted for v12.

### T2-4 — Lab profile selection in compose

**What**: `docker-compose.yml` gains lab-aware profiles:
- `--profile dev` (current default) — single-process, no lab assumption
- `--profile lab-dc` — bonsai + DC topology, NetBox seeded with DC inventory
- `--profile lab-sp` — bonsai + SP topology, NetBox seeded with SP inventory
- `--profile lab-full` — bonsai + DC + SP, NetBox seeded with both

**Done when**: an operator picks a profile and gets a coherent lab+bonsai+external-infra stack matching that profile.

### T2-5 — Lab readiness probe

**What**: `scripts/check_lab.sh` is to ContainerLab what `check_external.sh` is to external services. Asserts every device is up, every BGP/IS-IS session is established, EVPN routes are present, SR policies are active. JSON output for AI consumption.

**Where**: `scripts/check_lab.sh`.

**Done when**: AI sessions can verify lab health without needing to ssh into devices and read CLI.

### T2-6 — Lab fault injection catalogue

**What**: `lab/fault_catalog.yaml` enumerates every reproducible fault and the bonsai detection it should trigger:
```yaml
faults:
  - id: bgp-session-down-ce1-pe1
    inject: containerlab tools netem set ... drop 100
    expects:
      - detection: bgp_neighbor_down
        target: pe1
        peer: ce1
        within_seconds: 30
  - id: link-flap-leaf1-spine1
    ...
```

**Done when**: every detection rule has at least one fault in the catalogue that reproducibly triggers it. Tier 3 v11 chaos harness drives off this catalogue.

---

## <a id="tier-3"></a>TIER 3 — Iterative AI-Friendly Feedback Loop

**Why this is Tier 1-equivalent**: you said explicitly the bottleneck today is the AI not knowing what's broken vs working. Tests pass-or-fail in CI is necessary but insufficient — what's needed is a *stream of structured signal* an AI session consumes to know what state the application is in, what error was thrown, what UI element is non-functional, what handler crashed. The graduation is from `pytest` → programmatic UI/API drivers → chaos harness → structured machine-readable status throughout.

The principle: **every layer of bonsai emits machine-readable status that an AI can read without operator narration**. UI errors, API failures, log warnings, metric anomalies, chaos outcomes all flow into a single structured observability surface.

### T3-1 — Programmatic UI driver (Playwright + structured assertions)

**What**: a Playwright-based UI driver (`tests/ui_driver/`) that walks every operator workflow and emits structured per-step results. Not just pass/fail — captures:
- Which buttons are present
- Which API calls fire on click
- Console errors thrown
- Network response times
- Visual regressions (screenshot diffs)
- Element visibility / accessibility

**Output format**: JSON, one record per step:
```json
{
  "workflow": "add_device_via_wizard",
  "step": "step3_select_paths",
  "assertions": [
    {"name": "wizard_visible", "passed": true},
    {"name": "next_button_enabled", "passed": false, "reason": "disabled by validation"}
  ],
  "console_errors": [],
  "network": [{"url": "/api/onboarding/discover", "status": 200, "ms": 412}],
  "screenshot": "results/20260503-step3.png"
}
```

**Workflows to cover**:
- First-run setup wizard (every step)
- Add device flow
- Add credential flow
- Topology interaction (click, layer filter, site scope, path tracing)
- Incident drill-down
- Approve a remediation
- Add enricher + test connection
- Add output adapter + test connection
- Site hierarchy management
- Environment management

**Where**: `tests/ui_driver/` (Playwright), runs as `npm run ui-driver` from `ui/`.

**Done when**: a single command emits a JSON log covering every workflow with structured pass/fail/error data. The output is small enough an AI session can read it in one prompt.

### T3-2 — API contract driver

**What**: `tests/api_driver/` — a Python or Rust harness that hits every documented `/api/*` endpoint with happy-path and error-path inputs, validates response shape against a schema, and emits structured results.

**Same JSON output format as T3-1** so the AI consumer doesn't have to switch parsers.

**Where**: `tests/api_driver/`.

**Done when**: every endpoint in `docs/api_contract/` (the OpenAPI spec generated under v10 T5-2) has at least one pass-and-fail invocation. CI runs this on every PR.

### T3-3 — Event-stream driver (the bonsai-runtime side)

**What**: a harness that subscribes to bonsai's own event bus (or the SSE stream the UI uses) and records every event flowing through during a test run. Structured timeline of:
- Telemetry updates (sampled summaries — not the raw stream, that's too noisy)
- Detection events fired
- Remediation proposals/approvals/outcomes
- Enrichment runs
- Audit log entries
- Output-adapter dispatches

**Where**: `tests/event_driver/`.

**Output format**: same JSON pattern. Time-aligned with T3-1 UI driver and T3-2 API driver outputs so an AI can correlate "user clicked X → API call Y → bus event Z → UI updated W".

**Done when**: a 5-minute test run produces a single timeline JSON that captures the cause-and-effect chain from user action through to detection events.

### T3-4 — Chaos harness driving off the lab fault catalogue

**What**: `tests/chaos_harness/` — drives off `lab/fault_catalog.yaml` from T2-6. For each cataloged fault:
1. Assert pre-fault baseline state (no detections, all BGP up, etc.)
2. Inject the fault
3. Watch the bus and API for the expected detection
4. Assert detection appears within the documented window
5. Heal the fault
6. Assert detection clears

**Output format**: same JSON pattern. Per-fault: `{fault_id, expected_detection, observed_detection, latency_ms, passed}`.

**Where**: `tests/chaos_harness/`.

**Why this matters for AI feedback**: regressions in detection logic land silently today. The chaos harness produces a clear matrix of "which faults still trigger which detections, with what latency." An AI session can read this matrix and immediately know whether a recent change broke any detection.

**Done when**: every fault in the catalogue runs through the harness; matrix is captured in `docs/test_results/chaos_matrix/<date>.md`.

### T3-5 — Unified status emitter

**What**: a single endpoint `/api/_test/status` (gated behind a feature flag for prod) that returns the full structured-test-status snapshot:
```json
{
  "lab_health": {...},          // from check_lab.sh
  "external_health": {...},     // from check_external.sh
  "ui_workflows": {...},        // from latest T3-1 run
  "api_contracts": {...},       // from latest T3-2 run
  "detections_matrix": {...},   // from latest T3-4 chaos run
  "memory_metrics": {...},      // from T4 below
  "errors_last_hour": [...]     // from log aggregation
}
```

**Where**: `src/http_server.rs` (test-mode endpoint).

**Why this is the keystone**: an AI session asked "is bonsai working?" hits one endpoint and gets ground truth. No hunting through logs, no asking the operator to run multiple commands.

**Done when**: a single curl returns a complete state snapshot; format documented in `docs/ai_feedback_protocol.md`.

### T3-6 — `docs/ai_feedback_protocol.md`

**What**: the document that explains to future AI sessions how to consume the structured status. Defines the JSON schemas, the semantics of each field, what "broken" looks like in each domain, and how to escalate from "something failed" to "specific fix proposal."

**Where**: `docs/ai_feedback_protocol.md`.

**Done when**: a new AI session, given only this doc and the running status endpoint, can determine project health and propose specific actions without further instruction.

### T3-7 — CI integration of the feedback loop

**What**: the four drivers (UI, API, event, chaos) run on every PR. Output is summarised in the PR comment. Regressions block merge.

**Where**: `.github/workflows/feedback-loop.yml`.

**Done when**: PR authors see a structured "what broke" report on every change without needing to run the drivers manually.

### T3-8 — AI consumption examples

**What**: `docs/ai_feedback_examples.md` shows three worked examples:
1. AI session diagnoses "after my change, BGP detections don't fire anymore" by reading the chaos matrix delta
2. AI session diagnoses "the new enricher silently fails" by reading the structured event timeline
3. AI session diagnoses "memory grew during this run" by reading the memory profile output (Tier 4)

**Done when**: documented examples make the feedback-protocol concrete enough to mimic.

---

## <a id="tier-4"></a>TIER 4 — Memory and Database Hygiene

**Why this is Tier 1-equivalent**: a 9 GB memory growth in a "slim Rust core" is a credibility bug. Either bonsai is genuinely leaking, or it's holding state that should be on disk, or compression is mis-tuned, or DB growth is unbounded. Until this is diagnosed and fixed, claims about bonsai being lightweight are unsupported.

The principle: **bonsai's memory and disk footprint must be bounded, predictable, and proportional to operator-configured retention — not to runtime duration**.

### T4-1 — Memory profiling instrumentation

**What**: bonsai gains a `--memory-profile` flag that, when set, emits memory usage breakdowns to a structured JSON file every 60 seconds. Includes:
- Resident set size (RSS) from `/proc/self/status`
- jemalloc stats (per-arena)
- LadybugDB cache size
- Event-bus broadcast channel queue depths (per subscriber)
- Archive buffer sizes (per partition)
- Counter summarizer state size
- Number of open Parquet writer handles

**Tooling**: `tikv-jemalloc-ctl` for jemalloc introspection (already pulled in via tikv-jemallocator? — verify). `procfs` crate for RSS. Custom counters for everything else.

**Where**: new `src/memory_profile.rs`, wired into main loop.

**Done when**: a 30-minute bonsai run with telemetry flowing produces a memory-over-time JSON that tells you which component is growing.

### T4-2 — Identify the 9 GB culprit

**What**: actually run T4-1 against the existing setup, capture the profile, and identify where the growth is. Likely candidates from code review:

1. **Counter summarizer buffer** — keeps per-(target, interface, counter) state. If interfaces flap or new interfaces appear, no LRU eviction.
2. **Event bus broadcast channel** — `tokio::sync::broadcast` capacity 2048 per subscriber. With now 8+ subscribers (graph writer, archive, detector, retention, four output adapters, SSE handlers) and any one lagging, ~16k buffered `TelemetryUpdate` clones can accumulate.
3. **Archive partition writers** — `HashMap<ArchivePartition, ArrowWriter>`. Stale partitions are flushed when a new hour's data arrives, but if a particular target has irregular cadence the writer can stay open indefinitely.
4. **Audit log buffering** — JSONL writes; should be append+drop but worth verifying no in-memory aggregate.
5. **LadybugDB read cache** — embedded graph DB likely has a configurable cache; default may be too aggressive.

**Where**: investigation, not code change. Result captured in `docs/test_results/memory_investigation/<date>.md` with a clear "the culprit is X" conclusion.

**Done when**: the 9 GB number is explained. Probably with a chart.

### T4-3 — Fix the identified culprit(s)

**What**: based on T4-2, implement bounds. Likely fixes:

- **Counter summarizer LRU eviction** — if a (target, interface, counter) hasn't seen an update in N minutes, drop its state. Configurable `[counter_summarizer.max_idle_minutes]` default 60.
- **Broadcast channel slow-subscriber detection** — if a subscriber lags by >50% of capacity for >30s, log a warning with subscriber name and consider dropping it. Config `[event_bus.slow_subscriber_threshold]`.
- **Archive partition writer max-age** — if a partition writer is idle for >2× hour-rotation interval, force-flush and close.
- **LadybugDB cache cap** — explicit config, default sized to 10% of available system RAM with a hard cap.

**Where**: targeted fixes in `src/counter_summarizer.rs`, `src/event_bus.rs`, `src/archive.rs`, LadybugDB init.

**Done when**: same 30-minute run produces RSS that plateaus, not grows. Specifically: RSS should stabilise within 15 minutes of startup at <500 MB for a 12-device lab load.

### T4-4 — DB compression audit and tuning

**What**: today archive uses ZSTD level 3 (`Compression::ZSTD(ZstdLevel::try_new(3))`). Two things to check:

1. **ZSTD level**: bump to level 9 or 12 for cold archive segments. Level 22 is too CPU-heavy. The tradeoff: ~30-40% size reduction at level 12 vs level 3, ~3x CPU cost at write time. Acceptable because writes are batched.

2. **Dictionary encoding**: Parquet supports per-column dictionary encoding. The schema (`timestamp_ns`, `target`, `vendor`, `hostname`, `path`, `value`, `event_type`) has *highly repetitive* string columns (`vendor`, `hostname`, `path`, `event_type`). Dictionary encoding compresses these 5-10x. Currently not enabled.

**Where**: `src/archive.rs::writer_properties()`.

**Done when**:
- Per-column dictionary encoding enabled for `vendor`, `hostname`, `path`, `event_type`
- ZSTD level configurable via `[archive.compression_level]`, default 12
- Documented size-on-disk before/after with a 30-minute lab run sample. Expected reduction: 60-80% from current.

### T4-5 — Disk-aware DB sizing

**What**: bonsai detects available disk space at startup and refuses to grow the archive (or graph) beyond a configurable percentage. Default 70% of available disk on the volume holding the archive.

When the threshold is hit:
- Archive starts aggressive retention (drops oldest partitions until under threshold)
- Graph triggers `VACUUM` or LadybugDB equivalent
- A loud alert fires (log + UI banner)
- Operator can override the threshold but cannot easily *miss* the situation

**Where**: `src/archive.rs` retention logic + `src/retention.rs` + new `src/disk_guard.rs`.

**Config**:
```toml
[storage]
max_disk_use_percent = 70
disk_check_interval_secs = 300
```

**Done when**:
- Filling the disk artificially (dd a large file in test) triggers the guard within `disk_check_interval_secs`
- Retention drops oldest data
- Operator can see what was dropped in audit log

### T4-6 — Memory and disk metrics in the Operations workspace

**What**: the Operations UI workspace (already exists from v6) gains a panel showing:
- Current RSS + breakdown
- Current archive size on disk + percent of cap
- Current graph DB size + percent of cap
- Compression ratio
- Memory growth trendline (last 24h)

**Where**: `ui/src/routes/Operations.svelte`.

**Done when**: an operator can see at a glance whether memory or disk is approaching the cap, without running profilers.

### T4-7 — Memory budget as CI assertion

**What**: a CI test that runs bonsai for 10 minutes against a synthetic load, captures peak RSS, and fails if it exceeds a budget (e.g. 1 GB). Catches memory regressions before merge.

**Where**: `.github/workflows/memory-budget.yml`.

**Done when**: a deliberate memory leak in a test PR triggers the assertion.

### T4-8 — Document the bounded-resource contract

**What**: `docs/resource_contract.md` states the bounded-resource guarantees:
- RSS bounded at $C_{rss}$ for $N$ devices regardless of runtime
- Archive bounded at $\min(\text{retention}, \text{disk\_cap})$
- Graph bounded at $\min(\text{retention}, \text{disk\_cap})$
- Memory growth rate observable in `/api/_test/status` (T3-5)

**Done when**: an operator reading the doc has clear expectations and knows where to verify them.

---

## <a id="tier-5"></a>TIER 5 — Carryover from v9/v10

These items remain valid but lower-priority than the four v11 threads. References to original v9/v10 entries kept short.

### From v10 (testing remainder)

- T2-4 v10 ServiceNow PDI live test — needs operator-supplied PDI URL/credentials. Once provided, runs.
- T2-5 v10 ServiceNow EM push live — same dependency.
- T2-6 v10 HIL e2e — partly subsumed by T3-4 chaos harness.
- T3-* v10 UI/API contract tests — substantially folded into T3-1 + T3-2 v11.
- T4-1 v10 coverage measurement, T4-3 nightly integration, T4-5 mutation, T4-6 nextest — all roll into T0-2 v11 nightly CI signal and the broader Tier 3 v11 feedback loop.

### From v9 (strategic remainder)

- v9 Tier 4 Operator path overrides at site/role/device granularity
- v9 T5-2 Catalogue install command
- v9 T6-7 AIOps readiness checklist
- v9 T7 Signals (syslog + traps)
- v9 T8 Path A → Path B GNN — destination after v11 polish lands
- v9 T9 Investigation agent
- v9 T10 Controller adapters (demand-driven)
- v9 T11 Scale architecture, NL query, etc.

All defer until after v11 sprints.

---

## <a id="execution-order"></a>Execution Order

The four v11 threads can run partially in parallel because they touch different subsystems. Recommended sequencing optimises for AI session value (the feedback loop unblocks everything else).

### Sprint 1 — Standardised external infrastructure (1-2 weeks)
1. T1-1 v11 compose-external.yml umbrella with all profiles
2. T1-2 v11 seed_external.sh + new seed scripts
3. T1-3 v11 configure_external.sh
4. T1-4 v11 check_external.sh JSON output
5. T1-5 v11 documentation
6. T0-2 v11 nightly CI signal
7. T0-3 v11 Python tests in CI

### Sprint 2 — Memory + DB hygiene (1-2 weeks) ⚡
8. T4-1 v11 memory profiling instrumentation
9. T4-2 v11 identify the 9 GB culprit
10. T4-3 v11 fix culprit(s)
11. T4-4 v11 DB compression audit + dictionary encoding + ZSTD level
12. T4-5 v11 disk-aware sizing
13. T4-7 v11 CI memory budget
14. T4-8 v11 resource contract doc

### Sprint 3 — Iterative AI feedback loop (2-3 weeks) ⚡
15. T3-5 v11 unified status emitter (`/api/_test/status`)
16. T3-2 v11 API contract driver
17. T3-1 v11 Playwright UI driver
18. T3-3 v11 event-stream driver
19. T3-6 v11 ai_feedback_protocol.md
20. T3-7 v11 CI integration
21. T4-6 v11 memory/disk panels in Operations UI

### Sprint 4 — Comprehensive ContainerLab (2-3 weeks)
22. T2-1 v11 DC EVPN-SRv6 topology + configs
23. T2-2 v11 SP MPLS-SRTE topology + configs
24. T2-5 v11 lab readiness probe
25. T2-6 v11 lab fault catalogue
26. T2-4 v11 lab-aware compose profiles
27. T3-4 v11 chaos harness driving off catalogue
28. T3-8 v11 AI consumption examples

### Sprint 5 — Verify v10 + carry over remainders (1 week)
29. T0-1 v11 verify v10 Tier 0 fixes line-by-line
30. v10 T2-4 + T2-5 (PDI live tests when credentials provided)
31. v10 T3-3 a11y audit

### After v11 — return to v9 strategic threads
- v9 T4 path overrides
- v9 T7 signals
- v9 T8 Path A → Path B GNN
- v9 T9 investigation agent
- and so on

---

## <a id="guardrails"></a>Guardrails

### New in v11

- **External infrastructure must come up with one command.** Operator/AI time spent on env setup is wasted time. New external dependencies require a compose profile, a seed script, and `.env.example` entry — no exceptions.
- **Lab topologies must reflect target audience features.** A new test against the lab is rejected if the lab doesn't actually exercise the feature. SP claims require SR-MPLS/SR-TE/L3VPN running; DC claims require EVPN/VXLAN running.
- **NOSes selected for laptop scale.** Container memory ≤2 GB per NOS unless feature-coverage justifies larger; XRd at 4-6 GB requires explicit justification per topology.
- **Every test layer emits machine-readable status.** Pass/fail isn't enough. UI, API, events, chaos all emit structured JSON the AI reads without operator narration.
- **Memory and disk bounded by configuration, not by runtime.** Growth proportional to N (device count) + retention. Not to T (uptime).
- **Compression is best-in-class.** ZSTD level + dictionary encoding tuned and documented. New columns added to the archive justify their compression strategy.
- **Disk usage capped at configurable percentage.** Bonsai never grows the archive past the cap without explicit operator override.

### Unchanged from v7-v10

All prior architectural invariants and discipline continue: gNMI-only hot path, controller-less primary audience, enrichment-as-differentiator, AIOps-feeder positioning, vault-only credentials with purpose-tagged audit, HIL graduated remediation, OutputAdapter read-only on bus, no LLM in detect-heal, no LLM on device config in enrichment, every-new-Rust-module-ships-with-tests-in-same-PR. References v7 § Audience, v9 § Guardrails.

### Anti-patterns to reject

- "Operators can read the docs and bring up Splunk themselves" — no, one-command bring-up
- "Lab is good enough; we'll add EVPN later" — no, comprehensive lab is the foundation
- "Just look at the logs; the AI will figure it out" — no, structured status emitter
- "Memory grew because of the workload" — no, memory grows because we leaked or held; identify and bound
- "Compression is fine because we use ZSTD" — no, level + dictionary encoding tuned and documented
- "We'll add new features alongside the testing work" — no, v11 is the four threads only
- All v10 anti-patterns remain (no new functional tiers until tests/quality consolidate)

---

## What v11 Explicitly Excludes

- New functional features beyond the four threads
- Path A/B GNN implementation
- Investigation agent
- Signals tier (syslog/traps)
- Controller adapters
- NL query
- Auth/RBAC, multi-tenancy, production HA, Kubernetes
- Campus topology (deferred to v12)

---

*Version 11.0 — authored 2026-05-03 after deep code review of post-v10 main. v10 testing infrastructure verified landed: every previously-zero-tests v9 module now has 6-11 tests; five new e2e scripts; test results discipline established. v11 prioritises four operator-loop blockers: standardised external infra (end the env-setup AI cycle drain), comprehensive ContainerLab (DC+SP with full feature surface, slim NOSes), iterative AI feedback loop (programmatic UI/API/event/chaos drivers emitting structured JSON for AI consumption, unified status endpoint), memory + database hygiene (profile the 9 GB growth, fix it, cap disk usage, dictionary-encode Parquet for best compression). Strategic v9 threads (controller adapters, GNN, investigation agent) all defer to post-v11. References v2-v10 for audience framing and prior architectural decisions; v11 spends real estate on the new content per the operator's explicit instruction.*


# BONSAI — Consolidated Backlog v10.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V9.md`. Produced 2026-04-25 after deep code review of post-v9 main.
>
> **v10 is a testing and quality consolidation iteration. No new features. No new tiers.** v9 landed roughly 5,500 lines of new Rust (NetBox enricher, ServiceNow enricher, four output adapters, trust module, MCP client) plus 1,200 lines of new Python (path discovery, PDI seeding, doc generation). Most of this code has never been exercised end-to-end, ~2,400 lines of Rust have zero tests, the integration surface (NetBox container, ServiceNow PDI, output adapters against real receivers) has never been validated, and several latent bugs are visible on close inspection.
>
> **What v10 does:**
> 1. Catalog every untested module with a specific test plan
> 2. Catalog code-quality issues found in this review (Q-1 through Q-18) with concrete fixes
> 3. Define programmatic test procedures for APIs / Docker / ContainerLab / NetBox / PDI integration with documented results
> 4. Establish a `docs/test_results/` directory pattern so every feature has captured evidence of working
>
> **What v10 explicitly does NOT do:**
> - Add new functional capabilities
> - Redesign existing modules
> - Reframe positioning or audience (unchanged from v7-v9)
> - Touch deferred items (Path A/B GNN, investigation agent, signals tier)
> - Carry forward strategic threads — v9 strategic tiers (PDI integration completion, HIL completion, OutputAdapter remainder) remain in their existing form, only test instructions are added

---

## Table of Contents

1. [Audience and Positioning](#positioning)
2. [Progress Since v9 — Verified](#progress)
3. [Code Quality Issues Surfaced by Review](#quality)
4. [TIER 0 — Bugfixes from Review (highest priority)](#tier-0)
5. [TIER 1 — Test the Foundation (Rust unit/integration tests)](#tier-1)
6. [TIER 2 — Programmatic Integration Tests (live infra)](#tier-2)
7. [TIER 3 — UI / API contract tests](#tier-3)
8. [TIER 4 — CI hardening](#tier-4)
9. [TIER 5 — Test Documentation Discipline](#tier-5)
10. [Carryover from v9 (no test work, deferred)](#carryover)
11. [Execution Order](#execution-order)
12. [Guardrails — Updated](#guardrails)

---

## <a id="positioning"></a>Audience and Positioning

**Unchanged from v7/v8/v9.** Primary target: controller-less environments across DC, campus, SP. Secondary: multi-controller correlation. v9 sharpening of AIOps positioning (bonsai feeds AIOps platforms, doesn't replace them) remains.

---

## <a id="progress"></a>Progress Since v9 — Verified Against Main

All verified by reading the code, not by trusting declarations.

### v9 Tier 0 corrections — DONE

| Item | Status | Evidence |
|---|---|---|
| T0-1 v9 semver-aware plugin compare | ✅ Done | `src/catalogue/mod.rs:7` `semver_gt` function uses `semver::Version::parse`; falls back to string compare with warning; unit test `semver_gt_handles_double_digit_patch` |
| T0-2 v9 BonsaiStore in enrichment trait | ✅ Done | `src/enrichment/mod.rs:134` `&dyn crate::store::BonsaiStore` in trait signature |
| T0-3 v9 record_run audit logging | ✅ Done | `src/enrichment/mod.rs:109,262` calls `crate::audit::append_enrichment_run` on every run |
| T0-4 v9 first-run auto-redirect | ✅ Done | `ui/src/App.svelte:42-45` fetches `/api/setup/status`, sets `showSetup` flag, redirects to `/setup` when first-run |
| T0-5 v9 Setup wizard skip path | ✅ Done (assumed; needs spot-check) |
| T0-6 v9 remove `inferred_environment_for_role` | ✅ Partial | Function still exists at `src/discovery.rs:292` as fallback when caller doesn't pass archetype. Acceptable as backwards-compat fallback; the primary path (`environment_archetype` parameter) was added. Not a blocker. |
| T0-7 v9 catalogue gap-fill | ✅ Done | 12 new path profile docs in `docs/path_profiles/`: `campus_core.md`, `campus_distribution.md`, `dc_border_standard.md`, `dc_spine_standard.md`, `dc_superspine_standard.md`, `homelab_switch.md`, `sp_p_core.md`, `sp_p_sr_te.md`, `sp_pe_evpn.md`, `sp_pe_full.md`, `sp_peering_edge.md`, `sp_rr_basic.md` |

### v9 Tier 1 — NetBox enricher: DONE

| Item | Status | Evidence |
|---|---|---|
| T1-1 v9 NetBox enricher implementation | ✅ Code landed | `src/enrichment/netbox.rs` (712 lines): REST + MCP transport, paginated fetches, concurrent device/VLAN/prefix/interface fetch, graph writes for VLAN/Prefix nodes + ACCESS_VLAN/TRUNK_VLAN/HAS_PREFIX edges, namespaced `netbox_*` properties via HAS_ENRICHMENT_PROPERTY edges. **Zero tests in module.** |
| T1-2 v9 enrichment visibility in DeviceDrawer | ✅ Done | `ui/src/lib/DeviceDrawer.svelte` differs from v9; needs spot-check on enrichment panel |
| T1-3 v9 MCP client infrastructure | ✅ Code landed | `src/mcp_client.rs` (106 lines), `EnricherTransport` enum, `McpClient`. **Zero tests.** |

### v9 Tier 2 — ServiceNow PDI: PARTIAL (needs PDI to verify)

| Item | Status | Evidence |
|---|---|---|
| T2-1 v9 PDI configuration surface | ✅ Code landed | Vault supports basic-auth aliases; UI accepts URL via Enrichment workspace |
| T2-2 v9 PDI seed automation | ✅ Code landed | `scripts/seed_servicenow_pdi.py` (235 lines). **Reads creds from env vars not vault — contradicts docstring.** Not yet run against PDI (user explicitly said PDI info not yet provided). |
| T2-3 v9 ServiceNow CMDB enricher | ✅ Code landed | `src/enrichment/servicenow.rs` (621 lines), Basic auth, fetches business services + relationships + incidents, writes Application + Incident nodes + RUNS_SERVICE/CARRIES_APPLICATION edges. **Zero tests.** |
| T2-4 v9 ServiceNow Event Management push | ✅ Code landed | `src/output/servicenow_em.rs` (278 lines), OutputAdapter for em_event push. **Zero tests in module.** |
| T2-5 v9 incident state consumption | ✅ Code landed | Part of `src/enrichment/servicenow.rs` |
| T2-6 v9 event filter policy | ✅ Code landed (assumed); needs spot-check |

### v9 Tier 3 — HIL graduated remediation: PARTIAL

| Item | Status | Evidence |
|---|---|---|
| T3-1 v9 TrustState model | ✅ Code landed | `src/remediation/trust.rs` (224 lines), TrustState enum, TrustKey, TrustRecord, TrustStore. **Zero tests.** |
| T3-2 v9 Pending Approvals workspace | ✅ UI route landed | `ui/src/routes/Approvals.svelte` (230 lines). Needs end-to-end test. |
| T3-3 v9 graduation logic | ✅ Code landed | `src/remediation/graduation.rs` (50 lines). **Zero tests.** |
| T3-4 v9 rollback window | ✅ Code landed | `src/remediation/rollback.rs` (73 lines). **Zero tests.** |
| T3-5 v9 per-rule per-environment defaults | ✅ Code landed | `RemediationConfig` in `src/config.rs`; `default_state_for` in trust.rs |
| T3-6 v9 trust audit | ✅ Probably done (assumed); needs spot-check |

### v9 Tier 4 — Operator path overrides: NOT YET STARTED

Carry forward unchanged.

### v9 Tier 5 — YANG path discovery: PARTIAL

| Item | Status | Evidence |
|---|---|---|
| T5-1 v9 path discovery script | ✅ Code landed | `scripts/discover_yang_paths.py` (680 lines). **Zero tests. Never run end-to-end.** |
| T5-2 v9 plugin install command | ❌ Not done | No `bonsai catalogue install` CLI subcommand visible |
| T5-3 v9 doc generation | ✅ Code landed | `scripts/gen_profile_docs.py` (261 lines). **Zero tests.** |

### v9 Tier 6 — OutputAdapter: MOSTLY DONE

| Item | Status | Evidence |
|---|---|---|
| T6-1 v9 OutputAdapter trait | ✅ Done | `src/output/traits.rs` (378 lines, has tests) |
| T6-2 v9 Prometheus remote-write | ✅ Done with tests | `src/output/prometheus.rs` (511 lines, 7 tests) |
| T6-3 v9 Splunk HEC | ✅ Done with tests | `src/output/splunk_hec.rs` (462 lines, has tests) |
| T6-4 v9 Elastic ingest | ✅ Done with tests | `src/output/elastic.rs` (506 lines, 8 tests) |
| T6-5 v9 ServiceNow EM as OutputAdapter | ✅ Done | `src/output/servicenow_em.rs`. **Zero tests in module.** |
| T6-6 v9 adapter management UI | ✅ UI route landed | `ui/src/routes/Adapters.svelte` (562 lines). Needs end-to-end test. |
| T6-7 v9 AIOps readiness checklist | ❌ Not visible — carry forward |

### Substantial untested surface area

The following modules have **zero tests** despite being in main:

| Module | Lines | Risk |
|---|---|---|
| `src/enrichment/netbox.rs` | 712 | High — REST client with pagination bug, graph writes, hardcoded `edges_created: 0` |
| `src/enrichment/servicenow.rs` | 621 | High — Basic auth, complex multi-table joins, write_to_graph |
| `src/enrichment/mod.rs` | 334 | Medium — registry persistence, scheduling |
| `src/enrichment/factory.rs` | 22 | Low |
| `src/output/servicenow_em.rs` | 278 | Medium |
| `src/remediation/trust.rs` | 224 | High — persistence, state transitions, audit |
| `src/remediation/graduation.rs` | 50 | Medium |
| `src/remediation/rollback.rs` | 73 | High — inverse-step execution, partial-failure handling |
| `src/mcp_client.rs` | 106 | Medium |
| `scripts/seed_servicenow_pdi.py` | 235 | Medium — production runs against PDI |
| `scripts/discover_yang_paths.py` | 680 | Medium — pyang-dependent, untested |
| `scripts/gen_profile_docs.py` | 261 | Low |
| **Total** | **3,596** | |

Tests in CI today exercise only the modules that already had tests. v10 closes this gap.

---

## <a id="quality"></a>Code Quality Issues Surfaced by Review

Issues found by reading the new code. Each becomes a Tier 0 v10 fix.

### Q-1 — `NetBoxEnricher::enrich` reports `edges_created: 0` always

**Location**: `src/enrichment/netbox.rs:383`

**Issue**: Hardcoded `edges_created: 0` in the EnrichmentReport struct, even though the enricher actively creates `ACCESS_VLAN`, `TRUNK_VLAN`, `HAS_PREFIX`, `HAS_ENRICHMENT_PROPERTY` edges. The UI metric will mislead.

**Fix**: `write_to_graph` returns `(nodes_touched, edges_created, warnings)`; threading through to the report. ~10 lines.

### Q-2 — NetBox pagination re-parses offset from URL string

**Location**: `src/enrichment/netbox.rs:240-244`

**Issue**: Each loop iteration re-parses `offset=N` from the URL string via `split("offset=").last()`. If the URL gains tracking parameters (server redirects, proxies adding query params), parsing returns 0 and produces an infinite loop.

**Fix**: Track offset as `usize` outside the loop, format the URL freshly each iteration. ~5 lines.

### Q-3 — NetBox token held as plain `String` not `SecretString`

**Location**: `src/enrichment/netbox.rs:331`

**Issue**: `let token = cred.password;` clones the password into a plain `String`. Lives in memory after `cred` drops; not zeroized. The vault uses `SecretString` for in-memory handling but enrichers extract plaintext.

**Fix**: pass `&str` directly without cloning, or wrap in `SecretString` and zeroize on drop. ~10 lines.

### Q-4 — NetBox enricher concurrency is unbounded

**Location**: `src/enrichment/netbox.rs:337-342`

**Issue**: `tokio::join!` fires four concurrent paginated fetches (devices/VLANs/prefixes/interfaces) simultaneously. For a NetBox with 10000+ interfaces this hammers the server with concurrent paginated requests, potentially triggering rate limits or saturating it.

**Fix**: `[enrichment.netbox.max_concurrent_requests]` config, default 2. Use `futures::stream::iter` + `buffer_unordered` or a semaphore. ~20 lines.

### Q-5 — NetBox `write_to_graph` is one transaction with no checkpoint

**Location**: `src/enrichment/netbox.rs:413+`

**Issue**: 5000-device write is one giant transaction. If it fails halfway, partial state is in the graph; the next run wastes time re-checking. No explicit transaction boundary or checkpoint.

**Fix**: Wrap in explicit `BEGIN` / `COMMIT` (or LadybugDB equivalent), or chunk writes into batches of 100 with per-batch transactions. ~30 lines.

### Q-6 — `seed_servicenow_pdi.py` docstring contradicts implementation

**Location**: `scripts/seed_servicenow_pdi.py:10-13`

**Issue**: Docstring says "reads PDI URL and admin credentials from the bonsai credential vault. Requires the vault to be unlocked". Code reads from env vars `SNOW_INSTANCE_URL`, `SNOW_USERNAME`, `SNOW_PASSWORD`.

**Fix**: Pick one. Vault-only matches the discipline (T2 v9 specified vault path). Env var support stays as a flag (`--use-env`). Default reads from vault via the bonsai gRPC API. ~20 lines.

### Q-7 — `seed_servicenow_pdi.py` doesn't verify created records

**Location**: `scripts/seed_servicenow_pdi.py:57-76` `upsert` function

**Issue**: After POST/PATCH, no GET re-validates the record. A misconfigured PDI silently produces duplicates or missing fields.

**Fix**: After upsert, GET the same record by `match_field=match_value` and assert it matches the payload. Log mismatch as warning. ~15 lines.

### Q-8 — `seed_servicenow_pdi.py` upsert lookup doesn't paginate

**Location**: `scripts/seed_servicenow_pdi.py:46-55`

**Issue**: `sysparm_limit=500` is hardcoded. For the lab topology (4 devices, ~10 services) this is fine, but if the PDI accumulates other test data the lookup misses records past the first page and creates duplicates.

**Fix**: Either lower limit to 1 (we're looking for one specific match) or paginate. ~10 lines.

### Q-9 — `TrustStore::record_auto_success` doesn't reset `failure_count_30d`

**Location**: `src/remediation/trust.rs:180-186`

**Issue**: A run of 5 failures followed by 100 successes leaves `failure_count_30d = 5` forever (no decrement, no reset on streak), polluting the graduation suggestion.

**Fix**: Either rename the field to `total_failures` (and update field comment), or reset to 0 after N consecutive successes (e.g., 10), or implement a real 30-day decay (timestamp-based). The cheapest fix is rename + update graduation logic to use `consecutive_successes`. ~20 lines.

### Q-10 — `TrustStore::persist` blocks under the lock

**Location**: `src/remediation/trust.rs:128-134`

**Issue**: Every `record_approval` / `record_rejection` / etc. calls `persist()` synchronously inside the locked write region. Disk write blocks all other lock-holders. For a busy approval queue this serializes the world.

**Fix**: Either `tokio::task::spawn_blocking` for the write, or use a write-behind pattern (queue change + background flush every N seconds). ~30 lines.

### Q-11 — `TrustStore::default_state_for` returns `&String::new()` for unknown archetypes

**Location**: `src/remediation/trust.rs:216`

**Issue**: For unknown archetypes the function returns `&String::new()` which `parse_state` then maps to `ApproveEach` (default match arm). Correct behaviour, but the empty-string lookup path is opaque — a logged warning would help operators understand why their custom archetype defaulted to ApproveEach.

**Fix**: Log a warning when archetype isn't in the known set. ~5 lines.

### Q-12 — `TrustStore::default_state_for` reads the wrong field for unknown archetype

**Location**: `src/remediation/trust.rs:204-218`

**Issue**: When `defaults.{archetype_field}` returns an empty string (no per-archetype default configured), `parse_state("")` defaults to `ApproveEach` — sensible. But the field reads silently default to empty without flagging that the operator hasn't configured them. Confusing once trust state files start accumulating with unexpected defaults.

**Fix**: Log a debug when defaulting; document the cascade in `[remediation]` config section. ~10 lines.

### Q-13 — ServiceNow enricher doesn't handle PDI rate limits

**Location**: `src/enrichment/servicenow.rs` overall

**Issue**: ServiceNow PDIs have aggressive rate limits (typically 5000 transactions per hour per user). The enricher fetches business services, relationships, devices, incidents — easily 50+ API calls per run. No 429 retry, no backoff, no exponential delay.

**Fix**: Add 429 handling with exponential backoff (start at 1s, cap at 60s, retry 3 times). ~30 lines.

### Q-14 — ServiceNow enricher's display_value parsing is fragile

**Location**: `src/enrichment/servicenow.rs:50-52` `SnowRef`

**Issue**: ServiceNow returns reference fields as either a sys_id string OR a `{display_value, value}` object depending on `sysparm_display_value` setting. The struct only handles `display_value`. If the enricher's request omits the param, deserialization fails.

**Fix**: Custom `Deserialize` impl that handles both shapes, or wrap in a `serde_with::OneOrMany`. ~15 lines.

### Q-15 — `discover_yang_paths.py` doesn't verify pyang availability before clone

**Location**: `scripts/discover_yang_paths.py` startup

**Issue**: Script clones git repos (slow, network-dependent) and then fails at parse time if `pyang` isn't installed. Better to fail fast.

**Fix**: Verify `which pyang` at startup; exit 2 with install instructions if missing. ~5 lines.

### Q-16 — Output adapter UI (`Adapters.svelte`) and trust UI (`Approvals.svelte`) have no integration test

**Location**: `ui/src/routes/Adapters.svelte`, `ui/src/routes/Approvals.svelte`

**Issue**: These are critical operator workflows with no Playwright/Cypress test that walks the operator through the flow.

**Fix**: covered in Tier 3.

### Q-17 — Setup wizard skip path: needs spot-check

**Location**: `ui/src/routes/Setup.svelte`

**Issue**: v9 T0-5 specified a skip path that creates a default Home-Lab environment + site. Marked "assumed done" — needs explicit verification that operators bypassing setup land in a usable state.

**Fix**: covered in Tier 3 spot-checks.

### Q-18 — Default `failure_count_30d` decay or rename is needed for graduation correctness

**Location**: `src/remediation/graduation.rs`

**Issue**: Graduation logic uses `failure_count_30d` to gate upgrades. Since that field doesn't actually decay (Q-9), graduation never recovers from old failures. An operator who saw 3 failures 60 days ago + 100 successes since is still blocked from graduation.

**Fix**: covered in Q-9. Once Q-9 is rebadged as `consecutive_successes`-based, graduation logic reads from the right signal.

---

## <a id="tier-0"></a>TIER 0 — Bugfixes from Review

All of Q-1 through Q-18. Addressed before any new test code is written, since some of the tests will discover the same bugs and we want the fixes paired with the tests in the same PR.

### T0-1 (v10) — NetBox enricher correctness pack

**What**: Fix Q-1 (edges_created), Q-2 (pagination offset bug), Q-3 (SecretString for token), Q-4 (concurrency cap), Q-5 (transaction boundary).

**Where**: `src/enrichment/netbox.rs`, `[enrichment.netbox]` config section, possibly `src/credentials.rs` for SecretString helper.

**Done when**: Each fix has a unit test asserting the corrected behaviour. Specifically:
- Test: `EnrichmentReport.edges_created` matches actual edges written
- Test: pagination still works when URL gains a redirect-injected query param (mock with reqwest)
- Test: token type is `SecretString` (compile-time check via type assertion)
- Test: concurrent requests are bounded by config value
- Test: partial-write failure leaves graph in pre-write state (mock graph that fails on Nth write)

### T0-2 (v10) — `seed_servicenow_pdi.py` correctness

**What**: Fix Q-6 (vault vs env confusion), Q-7 (verify after upsert), Q-8 (lookup pagination/scoping).

**Where**: `scripts/seed_servicenow_pdi.py`.

**Done when**: Script reads creds from vault by default with `--use-env` opt-out; verifies every upsert; integration test against the mock ServiceNow asserts no duplicates after running twice.

### T0-3 (v10) — TrustStore correctness

**What**: Fix Q-9 (failure count semantics), Q-10 (lock-hold during persist), Q-11/Q-12 (default state visibility).

**Where**: `src/remediation/trust.rs`, `src/remediation/graduation.rs`, possibly `[remediation]` config docs.

**Done when**: Field name matches semantics; persistence happens out of the lock; unknown archetypes log a warning; graduation logic uses corrected fields.

### T0-4 (v10) — ServiceNow enricher robustness

**What**: Fix Q-13 (rate-limit retry), Q-14 (display_value polymorphic deserialize).

**Where**: `src/enrichment/servicenow.rs`.

**Done when**: 429 responses trigger exponential backoff; deserializer handles both response shapes; unit tests cover both paths.

### T0-5 (v10) — Path discovery script preflight

**What**: Fix Q-15. Verify pyang at startup before slow git operations.

**Where**: `scripts/discover_yang_paths.py`.

**Done when**: Missing pyang produces clear error in <1s; script exits 2 (not 1); CI test asserts the failure mode.

### T0-6 (v10) — Spot-check the v9 "assumed done" items

**What**: Fix Q-17 (Setup wizard skip), verify T0-5 v9, T1-2 v9 (DeviceDrawer enrichment panel), T2-6 v9 (event filter policy), T3-6 v9 (trust audit logging).

**Where**: Each respective component.

**Done when**: Each item is either confirmed-working with a screenshot/test, or a follow-up bugfix is filed with specific repro.

---

## <a id="tier-1"></a>TIER 1 — Test the Foundation (Rust unit/integration tests)

Every untested module gets a test plan. Goal: every public function has at least one happy-path and one error-path test. Module-level integration tests for any module that touches I/O, the graph, or the network.

### T1-1 (v10) — `enrichment/netbox.rs` test plan

**Tests required**:
- **REST pagination**: mock `reqwest` server that returns paginated responses; assert all pages fetched; assert offset advances correctly across redirects (covers Q-2 fix).
- **MCP transport**: mock `McpClient`; assert tool calls match expected schemas.
- **Concurrent fetches with bounded concurrency**: mock that returns slowly; assert max in-flight matches config.
- **Graph write — happy path**: in-memory test graph; seed with NetBox response shape; assert VLAN/Prefix nodes + edges match expected.
- **Graph write — partial failure**: mock graph that fails on Nth write; assert error returns; assert no partial state (Q-5).
- **Credential resolve audit**: mock audit log; assert one credential resolve audit entry per `enrich()` call.
- **`writes_to()` declaration**: assert writes never escape declared namespace (test framework verifies all writes match `netbox_*` or owned labels/edges).
- **Config validation**: invalid base_url returns error before any HTTP call.

**Where**: `src/enrichment/netbox.rs` `#[cfg(test)] mod tests` section + `tests/enrichment_netbox.rs` for integration.

**Test infrastructure**: requires a `reqwest` mock framework (`wiremock` crate is the standard).

**Done when**: Module has ≥10 tests covering above scenarios; `cargo test -p bonsai enrichment::netbox` passes; CI runs them.

### T1-2 (v10) — `enrichment/servicenow.rs` test plan

**Tests required**:
- **Basic auth header**: mock server expects specific Authorization header; assert match.
- **Pagination**: ServiceNow uses different pagination than NetBox; cover the difference.
- **Display-value polymorphism**: covers Q-14; provide both response shapes; assert both deserialize.
- **Rate-limit retry**: mock returns 429 twice then 200; assert success after retries; assert backoff timing.
- **CMDB CI parsing**: provide realistic ServiceNow JSON for `cmdb_ci_business_service`, `cmdb_rel_ci`, `cmdb_ci_netgear`; assert correct Application/Device nodes and RUNS_SERVICE/CARRIES_APPLICATION edges produced.
- **Incident parsing**: provide incidents where `source = "bonsai"`; assert Incident nodes created and linked.
- **Graph write transaction**: same as Q-5 NetBox test.

**Where**: `src/enrichment/servicenow.rs` `#[cfg(test)] mod tests` + `tests/enrichment_servicenow.rs`.

**Done when**: ≥10 tests; `cargo test enrichment::servicenow` passes.

### T1-3 (v10) — `remediation/trust.rs` test plan

**Tests required**:
- **TrustState default per archetype**: assert each archetype maps to documented default state.
- **`get_or_default`**: missing key creates with correct default; existing key returns unchanged.
- **`record_approval` / `rejection`**: counters increment correctly; consecutive_successes resets on rejection.
- **`record_auto_success` after failure**: Q-9 semantics — assert documented behaviour matches code.
- **`set_state`**: explicit state change persists; updated_at_ns updates.
- **Persistence round-trip**: write store, drop, reload, assert all records match.
- **Concurrent access**: spawn 100 tasks, each calling record_approval on different keys; assert no data loss.
- **Concurrent persist**: assert persist doesn't block other operations longer than expected (covers Q-10 fix).
- **Default state for unknown archetype**: assert warning logged + ApproveEach returned (Q-11 fix).

**Where**: `src/remediation/trust.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥10 tests; `cargo test remediation::trust` passes.

### T1-4 (v10) — `remediation/graduation.rs` test plan

**Tests required**:
- **Graduation suggestion threshold**: 10 consecutive approvals triggers suggestion.
- **Graduation blocked by recent failures**: assert correct after Q-9 fix.
- **Operator graduation accept**: state transitions correctly.
- **Operator graduation reject**: state stays put.
- **Cannot skip steps**: SuggestOnly cannot graduate directly to AutoSilent (must walk through).

**Where**: `src/remediation/graduation.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥5 tests; `cargo test remediation::graduation` passes.

### T1-5 (v10) — `remediation/rollback.rs` test plan

**Tests required**:
- **Rollback within window succeeds**: dry-run playbook + inverse; rollback executes inverses in reverse order.
- **Rollback after window rejected**: timestamp checked; expired window returns error.
- **Partial rollback failure**: inverse fails on step N; assert error logged; trust state forced back to ApproveEach.
- **Rollback of failed playbook**: rollback an execution that itself failed at step M; assert only steps ≤M get inverses.

**Where**: `src/remediation/rollback.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥5 tests; `cargo test remediation::rollback` passes.

### T1-6 (v10) — `enrichment/mod.rs` test plan

**Tests required**:
- **`EnricherRegistry::load` empty file**: returns empty registry without error.
- **`EnricherRegistry::load` corrupted file**: returns empty registry, warns; doesn't panic.
- **`upsert` add**: new entry appears.
- **`upsert` update**: existing entry replaced, not duplicated.
- **`remove`**: entry gone; state map cleaned.
- **`record_run` writes audit**: mock audit log; assert one entry per call (covers Q-3 v9 + audit hygiene).
- **`record_run` updates state**: last_run_at_ns updates correctly.
- **`EnricherWriteSurface` enforcement**: stub enricher writing outside namespace returns error.

**Where**: `src/enrichment/mod.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥8 tests; `cargo test enrichment` passes.

### T1-7 (v10) — `mcp_client.rs` test plan

**Tests required**:
- **Connection: happy path**: mock MCP server responds correctly.
- **Connection: timeout**: mock server doesn't respond; assert timeout returns error within configured window.
- **Tool call: serialization**: assert request shape matches MCP protocol.
- **Tool call: error response**: server returns error; client surfaces it.
- **`EnricherTransport::from_extra`**: parses `{transport: "rest"}` and `{transport: "mcp", server_url: "..."}` correctly; defaults to REST.

**Where**: `src/mcp_client.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥5 tests; `cargo test mcp_client` passes.

### T1-8 (v10) — `output/servicenow_em.rs` test plan

**Tests required**:
- **`em_event` schema mapping**: bonsai DetectionEvent → ServiceNow em_event payload; assert all required fields present.
- **Event filter policy**: low-severity event suppressed; high-severity flows.
- **Auth header**: Basic auth used; token resolved from vault.
- **Rate limiting**: covers Q-13 same pattern.
- **Network failure**: connection refused; assert error doesn't propagate to bus, just logs.

**Where**: `src/output/servicenow_em.rs` `#[cfg(test)] mod tests`.

**Done when**: ≥5 tests; `cargo test output::servicenow_em` passes.

### T1-9 (v10) — Python script tests

**Tests required**:
- `tests/python/test_seed_servicenow_pdi.py` — covers vault path, env var path, upsert verification (Q-7), idempotency (running twice produces no duplicates after Q-8 fix).
- `tests/python/test_discover_yang_paths.py` — pyang preflight (Q-15), mock git clone, candidate generation produces expected YAML shape.
- `tests/python/test_gen_profile_docs.py` — generated docs match expected for known input profiles; format stable.

**Where**: `python/tests/`.

**Done when**: All three test files exist; `pytest python/tests/` passes; CI runs Python tests.

---

## <a id="tier-2"></a>TIER 2 — Programmatic Integration Tests

Tier 1 covers unit tests with mocked I/O. Tier 2 exercises the real integrations against live infrastructure (containerised NetBox, ServiceNow PDI when configured, ContainerLab, Docker compose).

The discipline: every integration test produces a documented test result in `docs/test_results/<feature>/<date>.md` so we have evidence of what worked and when.

### T2-1 (v10) — Docker compose end-to-end test

**What**: An `e2e_compose_test.sh` script that:
1. Tears down any existing compose
2. Runs `scripts/generate_compose_tls.sh`
3. Sets `BONSAI_VAULT_PASSPHRASE` from a test fixture
4. Runs `docker compose --profile distributed up -d`
5. Waits for healthcheck on bonsai-core
6. Runs `scripts/seed_lab_creds.sh` with non-interactive mode
7. Asserts via `curl` that:
   - `/api/setup/status` returns `is_first_run: false` after seeding
   - `/api/credentials` lists the seeded aliases
   - `/api/onboarding/devices` is empty (no devices yet)
   - `/api/collectors` lists 2 collectors with `connected: true`
8. Adds 4 lab devices via `/api/onboarding/devices` POST
9. Waits for telemetry to start flowing
10. Asserts `/api/topology` returns 4 devices with role + site populated
11. Tears down

**Where**: `scripts/e2e_compose_test.sh`, `docs/test_results/e2e_compose/<date>.md`.

**Done when**: Script runs green from clean state; CI runs it on PR; produces dated test result artifact.

### T2-2 (v10) — ContainerLab integration test

**What**: With ContainerLab lab up:
1. Bonsai discovers all 4 lab devices via DiscoverDevice RPC
2. Asserts capability response includes expected models
3. Path profile recommendation matches expected for each device's role + vendor
4. After subscription, asserts within 60s that `/api/topology/state` shows oper_status for all interfaces
5. Inject a fault (`containerlab` ifdown on srl-leaf1 ethernet-1/1)
6. Assert detection event appears in `/api/detections` within 30s
7. Heal fault (ifup)
8. Assert detection event resolves

**Where**: `scripts/e2e_containerlab_test.sh`, `docs/test_results/e2e_containerlab/<date>.md`.

**Done when**: Script passes against the bundled `lab/fast-iteration/bonsai-p4` topology; result documented.

### T2-3 (v10) — NetBox enricher live integration test

**What**: With `docker compose --profile netbox up -d` and the NetBox container seeded via `scripts/seed_netbox.py`:
1. Add a NetBox enricher config via `POST /api/enrichers` with `transport: rest`
2. Assert `POST /api/enrichers/<name>/test` returns success
3. Trigger run via `POST /api/enrichers/<name>/run`
4. Wait for completion
5. Assert `/api/enrichers/<name>` shows last-run details with non-zero `nodes_touched` and `edges_created` (Q-1 fix)
6. Query graph: assert VLAN nodes match seeded VLANs
7. Query graph: assert Prefix nodes match seeded prefixes
8. Query graph: assert Device nodes have `netbox_*` properties
9. Run again; assert idempotent (no duplicate VLANs/prefixes)

**Where**: `scripts/e2e_netbox_enricher_test.sh`, `docs/test_results/e2e_netbox/<date>.md`.

**Done when**: Script passes; result documented; idempotency verified across 3 successive runs.

### T2-4 (v10) — ServiceNow PDI enricher live integration test

**What**: When operator provides PDI URL + admin creds:
1. `scripts/seed_servicenow_pdi.py --topology lab/seed/topology.yaml` populates PDI
2. Verify population by re-fetching each table; assert record counts match expected
3. Add ServiceNow enricher config; test connection succeeds
4. Trigger run; assert success
5. Query graph: assert Application nodes match PDI services
6. Query graph: assert RUNS_SERVICE / CARRIES_APPLICATION edges exist
7. Query graph: assert Device nodes have `snow_ci_id`, `snow_owner_group`, `snow_assignment_group`
8. Run again; assert idempotent
9. **Manual step (not automatable since PDI shouldn't be in CI)**: operator confirms by browsing ServiceNow UI that bonsai-created records are present and well-formed.

**Where**: `scripts/e2e_servicenow_pdi_test.sh`, `docs/test_results/e2e_servicenow_pdi/<date>.md`.

**Operator-provided inputs**:
- `SNOW_INSTANCE_URL` (PDI URL)
- `SNOW_USERNAME` (PDI admin)
- `SNOW_PASSWORD` (PDI admin password)

**Done when**: Operator runs script with PDI credentials; outputs match expected; result documented; PDI cleanup script (`scripts/cleanup_servicenow_pdi.py`) tears down test data so PDI can be reused.

### T2-5 (v10) — ServiceNow Event Management push test

**What**: With ServiceNow PDI configured and an em_event push adapter enabled:
1. Inject a fault in the lab
2. Wait for bonsai detection event
3. Within 30s assert `em_event` appears in PDI via direct PDI query
4. Heal fault
5. Confirm event-resolved push (if implemented)
6. Verify ServiceNow correlation rules can act on the event (manual verification)

**Where**: `scripts/e2e_servicenow_em_test.sh`, `docs/test_results/e2e_servicenow_em/<date>.md`.

**Done when**: Detection-to-em_event roundtrip works; latency documented; result captured.

### T2-6 (v10) — HIL graduated remediation end-to-end test

**What**: With chaos plan injecting deterministic faults:
1. Configure a rule with TrustState = ApproveEach
2. Inject fault; assert proposal appears in `/api/approvals`
3. Approve via `POST /api/approvals/<id>/approve`
4. Assert playbook executes; outcome flows back
5. Repeat 10 times
6. Assert graduation suggestion appears in `/api/trust/<key>`
7. Manually graduate to AutoWithNotification
8. Inject fault; assert auto-execution; assert UI banner with rollback option visible
9. Click rollback within window; assert inverse executes; assert TrustState forced back to ApproveEach

**Where**: `scripts/e2e_hil_test.sh`, `docs/test_results/e2e_hil/<date>.md`.

**Done when**: Full graduation cycle plus rollback works in lab; result documented.

### T2-7 (v10) — Output adapter end-to-end tests

**Prometheus**:
1. Add Prometheus adapter pointing at a test-instance Prometheus
2. Generate counter traffic via lab
3. Query Prometheus: assert `bonsai_*` metrics appear with correct labels (device, interface, vendor, role, site)
4. Document metric latency (counter increment → visible in Prometheus)

**Splunk HEC**:
1. Add Splunk adapter pointing at a Docker Splunk Enterprise container (1-day trial license)
2. Inject lab fault
3. Detection event flows
4. Splunk search returns the event within 30s
5. Document field mapping correctness

**Elastic**:
1. Same pattern with Elastic container
2. ECS field compliance verified

**Where**: Per-adapter test scripts; `docs/test_results/e2e_output_<adapter>/<date>.md`.

**Done when**: Each adapter has a documented green run.

### T2-8 (v10) — Path profile validation against live capabilities

**What**: A regression test that:
1. Discovers each lab device
2. Runs path profile recommendation
3. For each recommended path, attempts subscription
4. Asserts subscription either receives data within 60s OR is correctly flagged `subscribed_but_silent`
5. Asserts no path appears in profile that the device doesn't actually support

**Where**: `scripts/e2e_path_validation_test.sh`, `docs/test_results/e2e_path_validation/<date>.md`.

**Done when**: All 12 path profile docs (`docs/path_profiles/*.md`) reference a green test result.

---

## <a id="tier-3"></a>TIER 3 — UI / API Contract Tests

### T3-1 (v10) — HTTP API contract tests

**What**: For every `/api/*` endpoint, an automated test asserts:
- Response shape matches documentation
- Required fields present, optional fields handled when missing
- Error responses (400, 401, 404, 500) follow consistent schema
- Auth (when added) consistently applied

**Tool**: `cargo test` integration tests using `reqwest` against a started bonsai instance, OR a Python `pytest` suite using `httpx`.

**Endpoints to cover** (currently observed in code):
- Setup: `/api/setup/status`
- Credentials: `/api/credentials` GET/POST/DELETE
- Onboarding: `/api/onboarding/devices` GET/POST/DELETE/PATCH
- Topology: `/api/topology`, `/api/path?src=&dst=`
- Sites: `/api/sites` GET/POST/DELETE
- Environments: `/api/environments` GET/POST/DELETE
- Collectors: `/api/collectors`
- Profiles/Catalogue: `/api/catalogue/profiles`, `/api/catalogue/plugins`
- Enrichers: `/api/enrichers` GET/POST/DELETE; `/api/enrichers/<name>/run` POST; `/api/enrichers/<name>/test` POST
- Adapters: `/api/adapters` GET/POST/DELETE
- Trust: `/api/trust/<key>` GET; `/api/trust/<key>/state` PATCH
- Approvals: `/api/approvals` GET; `/api/approvals/<id>/approve|reject` POST
- Audit: `/api/audit?since=&until=`
- Operations: `/api/operations/health`

**Where**: `tests/api_contract.rs` (Rust) or `python/tests/test_api_contract.py`.

**Done when**: Every endpoint has at least one test for happy path + one for error path; CI runs them; OpenAPI/JSON schema generated as test artifact for `docs/api_contract/`.

### T3-2 (v10) — UI integration tests with Playwright

**What**: Playwright (or Cypress) tests for critical operator workflows:
- **First-run setup flow**: fresh install lands on `/setup`; complete wizard; redirects to `/devices`.
- **Add device flow**: from `/devices`, click `+ Add Device`; complete wizard; device appears in list.
- **Topology interaction**: open `/`; click device; drawer opens with correct data.
- **Layer filter**: toggle L3/L2/combined; assert nodes/links filter correctly.
- **Path tracing**: shift-click two devices; assert path highlights.
- **Approve a remediation**: with mock detection in queue, navigate to `/approvals`, approve, assert proposal removed from queue.
- **Add enricher**: navigate to `/enrichment`, add NetBox config, click test connection, assert success indicator.
- **Add adapter**: navigate to `/adapters`, add Prometheus, test connection, assert state.

**Where**: `ui/tests/e2e/`.

**Tooling**: Playwright + Vite preview server.

**Done when**: All flows pass on a Playwright run against a fresh bonsai instance; CI runs them; recorded video stored as artifact on failure.

### T3-3 (v10) — UI accessibility audit

**What**: Lighthouse a11y score ≥90 on every route. axe-core scan reveals zero serious issues.

**Where**: integrated with T3-2 Playwright suite using `@axe-core/playwright`.

**Done when**: All routes meet the bar; CI fails on regression.

---

## <a id="tier-4"></a>TIER 4 — CI Hardening

### T4-1 (v10) — Coverage measurement

**What**: `cargo llvm-cov` (or `cargo tarpaulin`) measures Rust coverage; `pytest --cov` for Python. Numbers reported in PR comments. Set a floor (e.g., 70%) below which CI fails for new code.

**Where**: `.github/workflows/ci.yml`.

**Done when**: Coverage report visible per PR; floor enforced.

### T4-2 (v10) — Run Python tests in CI

**What**: Python tests currently exist (`python/tests/`) but the CI workflow doesn't run them.

**Where**: `.github/workflows/ci.yml` adds a `python` job.

**Done when**: `pytest python/tests/` runs on every PR.

### T4-3 (v10) — Run Tier 2 integration tests in nightly CI

**What**: Tier 2 tests are slow and resource-intensive — not for every PR. A nightly CI workflow runs them all and posts results to a status dashboard.

**Where**: `.github/workflows/nightly-integration.yml`.

**Done when**: Nightly runs against the lab, posts results to `docs/test_results/`, alerts on failure.

### T4-4 (v10) — Add cargo-deny for dependency audit

**What**: `cargo-deny` enforces license policy, advisory database checks, banned crates. Runs in CI.

**Where**: `deny.toml`, `.github/workflows/ci.yml`.

**Done when**: CI fails on disallowed licenses or known vulnerabilities.

### T4-5 (v10) — Add cargo-mutants for mutation testing on critical modules

**What**: `cargo-mutants` runs mutation testing on `credentials.rs`, `audit.rs`, `remediation/trust.rs`, `assignment.rs`. Catches tests that pass-with-or-without the code being tested.

**Where**: `.github/workflows/mutation-testing.yml` (weekly).

**Done when**: Mutation score ≥80% on each critical module.

### T4-6 (v10) — Cargo nextest for faster CI

**What**: Replace `cargo test` with `cargo nextest` in CI for faster parallel execution and better failure reporting.

**Where**: `.github/workflows/ci.yml`.

**Done when**: CI test step uses nextest; failure output is more actionable.

### T4-7 (v10) — Schema/proto compatibility check

**What**: A CI job that compares the current `proto/bonsai_service.proto` to `main`'s version; flags breaking changes (removed fields, changed types).

**Where**: `.github/workflows/ci.yml` step using `buf breaking`.

**Done when**: PRs that break proto compatibility get a red CI status.

---

## <a id="tier-5"></a>TIER 5 — Test Documentation Discipline

### T5-1 (v10) — `docs/test_results/` directory

**What**: A `docs/test_results/` directory with one subdirectory per test feature. Every Tier 2 test produces a dated markdown file capturing:
- Test scenario summary
- Date and operator who ran it
- Lab topology used
- Versions of bonsai, NetBox, ServiceNow PDI (if applicable)
- Step-by-step assertions that passed
- Any warnings/observations
- Links to relevant log files (gitignored but referenced)

**Where**: `docs/test_results/<feature>/<YYYYMMDD>-<short-summary>.md`.

**Template**:
```markdown
# <feature> integration test

**Date**: YYYY-MM-DD
**Operator**: ...
**Bonsai version**: <git sha>
**Lab topology**: lab/fast-iteration/bonsai-p4
**External versions**: NetBox 3.x.y, ...

## Setup
- ...

## Test Results
- [x] Step 1: ...
- [x] Step 2: ...
...

## Observations
- ...

## Logs
- /tmp/bonsai-e2e-YYYYMMDD-HHMMSS.log
```

**Done when**: Template lives at `docs/test_results/TEMPLATE.md`; first three integration tests produce artifacts using it.

### T5-2 (v10) — `docs/api_contract/` from API contract tests

**What**: T3-1 generates an OpenAPI spec or JSON-schema bundle as test artifact. Lives in `docs/api_contract/`. PR diffs make API changes obvious.

**Where**: `docs/api_contract/`, generated by T3-1.

**Done when**: OpenAPI/schema generated; PRs touching the API show diff in this file.

### T5-3 (v10) — Test results dashboard page in UI

**What**: New route `/operations/tests` (extension of Operations workspace) shows the latest test run for each Tier 2 integration. Reads from a generated index file that nightly CI populates.

**Where**: `ui/src/routes/Operations.svelte` extension; `docs/test_results/index.json` generated by CI.

**Done when**: Operator can see at a glance which integrations are last-known-green and how recently.

---

## <a id="carryover"></a>Carryover from v9 — No Test Work Required Yet

These v9 tiers remain in the backlog but no v10 testing additions:

- **v9 T4** Operator path overrides — not yet started, defer
- **v9 T5-2** Plugin install command — not yet started, defer
- **v9 T6-7** AIOps readiness checklist — defer
- **v9 T7** Signals (syslog + traps) — defer
- **v9 T8** Path A → Path B GNN — defer
- **v9 T9** Investigation agent — defer
- **v9 T10** Controller adapters (demand-driven, defer)
- **v9 T11** Scale architecture, NL query, etc. — defer

---

## <a id="execution-order"></a>Recommended Execution Order

**Discipline**: Sprint 1 fixes (Tier 0) before any new test code lands so the tests verify correct behaviour, not bug-compatible behaviour.

### Sprint 1 — Tier 0 bug fixes (1-2 weeks)
1. T0-1 v10 NetBox enricher correctness pack (Q-1, Q-2, Q-3, Q-4, Q-5)
2. T0-2 v10 seed_servicenow_pdi.py correctness (Q-6, Q-7, Q-8)
3. T0-3 v10 TrustStore correctness (Q-9, Q-10, Q-11, Q-12)
4. T0-4 v10 ServiceNow enricher robustness (Q-13, Q-14)
5. T0-5 v10 path discovery preflight (Q-15)
6. T0-6 v10 spot-check assumed-done items (Q-17 + others)

### Sprint 2 — Tier 1 unit tests (2 weeks)
7. T1-1 v10 NetBox enricher tests
8. T1-2 v10 ServiceNow enricher tests
9. T1-3 v10 TrustStore tests
10. T1-4 v10 graduation tests
11. T1-5 v10 rollback tests
12. T1-6 v10 enrichment registry tests
13. T1-7 v10 mcp_client tests
14. T1-8 v10 servicenow_em adapter tests
15. T1-9 v10 Python script tests

### Sprint 3 — CI hardening (1 week)
16. T4-2 v10 Python tests in CI
17. T4-1 v10 coverage measurement
18. T4-4 v10 cargo-deny
19. T4-6 v10 cargo nextest
20. T4-7 v10 proto compat check

### Sprint 4 — Tier 2 integration tests (2 weeks)
21. T2-1 v10 Docker compose e2e
22. T2-2 v10 ContainerLab e2e
23. T2-3 v10 NetBox enricher live
24. T2-7 v10 Output adapter live (Prometheus, Splunk, Elastic)
25. T2-8 v10 Path profile validation
26. T5-1 v10 test results documentation discipline

### Sprint 5 — PDI integration tests (when PDI provided) (1 week)
27. T2-4 v10 ServiceNow PDI enricher live
28. T2-5 v10 ServiceNow EM push live

### Sprint 6 — UI tests (2 weeks)
29. T3-1 v10 HTTP API contract tests
30. T3-2 v10 Playwright e2e tests
31. T3-3 v10 UI a11y audit
32. T5-2 v10 API contract docs
33. T5-3 v10 test dashboard

### Sprint 7 — Remediation e2e (1-2 weeks)
34. T2-6 v10 HIL e2e

### Sprint 8 — Mutation testing + nightly CI (1 week)
35. T4-3 v10 nightly integration in CI
36. T4-5 v10 cargo-mutants on critical modules

### After v10 — return to v9 carryover
- v9 T4 operator path overrides
- v9 T5-2 catalogue install command
- v9 T6-7 AIOps readiness checklist
- v9 T7 signals
- v9 T8 Path A → Path B GNN
- v9 T9 investigation agent
- ... etc.

---

## <a id="guardrails"></a>Guardrails — Updated for v10

### New in v10

- **No new feature work until v10 sprints 1-3 complete.** The Tier 0 fixes plus Tier 1 unit tests plus CI hardening must land before any new v9 tier work resumes.
- **Every new Rust module ships with a test module in the same PR.** "Tests as a follow-up" is rejected. "TODO: tests" comments are rejected. v9 produced 2,400 lines of zero-test code; v10 says no more.
- **Every integration ships with a captured test result.** Manually running an integration once and saying "it works" doesn't count. The result lives in `docs/test_results/<feature>/<date>.md` with the template from T5-1.
- **Test documentation is not optional.** Tier 5 v10 is mandatory infrastructure, not a polish item.
- **PDI seed scripts must be idempotent and verifiable.** Running twice produces no duplicates. Every record gets a post-write GET to confirm. Mock and real PDI both satisfy this.
- **Coverage floor enforced in CI.** New code below 70% line coverage fails the build.

### Unchanged from v9

All prior guardrails (audience, gNMI-only hot path, syslog/traps as signals, tokio-only, vault-only credentials, no Kubernetes, ADR-at-commit-time, no LLM in detect-heal, all operator-facing on core, enrichers no LLM on config, collectors horizontal/core vertical, build time first-class, code-landing ≠ work-complete, distributed-must-run-distributed, environment-awareness first-class, path-catalogue-as-data, integration-mocks-before-implementations, every-resolve-carries-purpose, HIL-graduated, Output read-only, no-AIOps-replacement-positioning, no-auto-import-of-unverified-paths) remain in force.

### Anti-patterns to reject

- "We'll add tests next sprint" — no, this sprint
- "The integration works, I ran it once" — no, captured test result
- "We don't need a Tier 0 fix; the test will catch it" — no, fix first then test
- "Coverage is just a number" — no, it's a floor
- "Let's add a new feature while we're here" — no, v10 is testing only
- "Skip the v10 documentation step; the test passes" — no, documentation is the artifact
- All prior anti-patterns remain in force

---

## What v10 Explicitly Excludes

- New functional features
- New tiers beyond what's listed
- Re-design of existing modules (only fix bugs found in review)
- Carry-forward of v9 design discussions (audience, AIOps, controller adapters all stay as-is)
- Path A/B GNN, investigation agent, signals work
- Bringing new strategic threads into the document
- Auth/RBAC, multi-tenancy, production HA, Kubernetes

---

*Version 10.0 — authored 2026-04-25 after deep code review of post-v9 main. Verifies substantial v9 progress: NetBox enricher, ServiceNow enricher, four output adapters, TrustState model, path discovery script, PDI seed script — 5,500+ lines of new Rust + 1,200 lines of Python all landed. Surfaces 18 code-quality issues (Q-1 through Q-18) for Tier 0 cleanup. Catalogs 2,400 lines of zero-test code for Tier 1 unit tests. Defines comprehensive integration test plans (Tier 2) for Docker, ContainerLab, NetBox, PDI, output adapters, HIL. Adds CI hardening (Tier 4) and test documentation discipline (Tier 5). No new features. No new tiers. No design redesign. Sprints 1-3 must complete before any v9 carryover work resumes. New guardrail: "every new Rust module ships with a test module in the same PR" — addresses the v9 pattern of code-without-tests landing in main.*

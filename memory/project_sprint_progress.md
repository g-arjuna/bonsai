---
name: Sprint Progress
description: Backlog sprint completion status — v8 through v12 sprint progress
type: project
---

## Backlog v12 (current — supersedes v11)

**v12 Sprint 1 — Memory + binary fixes — COMPLETE 2026-05-03 (commit 2421332)**
- T0-1: LadybugDB buffer pool capped at min(2 GiB, 25% RAM) for core, min(256 MiB, 10%) for collector. Configured via [graph] buffer_pool_bytes in bonsai.toml. Root cause of 9 GB memory bug fixed.
- T0-2: Debounce HashMap replaced with lru::LruCache(1024) — bounded by config not runtime.
- T0-3: Default event bus capacity reduced from 2048 to 512.
- T0-4: Release binary self-contained (RUNPATH=$ORIGIN + auto-copy liblbug.so.0 in build.rs). No LD_LIBRARY_PATH needed.
- T0-8: Startup phase timing logs added (config_load, graph_open, schema_init, backfill, ready).
- T1-2/T1-3: Memory-budget CI updated to 10-min run / 1.5 GiB budget; resource_contract.md updated.

**Next: v12 Sprint 2 — Always-on infrastructure**
- T0-5: restart: unless-stopped in compose-external.yml / docker-compose.yml
- T0-6: --reset flag on all seed_*.py scripts
- T3-2: lab/dc and lab/sp Makefiles (up/down/status/reset)
- T3-3: scripts/reset_for_test.sh wrapper
- T0-9: trap cleanup EXIT in e2e scripts
- T2-2: Release artefact GitHub Actions pipeline
- T2-3: bonsai self-test subcommand

## Backlog v8 (prior sessions)

Sprint 1 (T0-1 through T0-6) complete 2026-04-24.
Sprint 2 (T1-1 through T1-6) complete 2026-04-24.

**Why:** v8 Sprint 2 scope was the Environment model — graph entity + API, Environments UI, Sites UI, Onboarding wizard, first-run setup wizard.

**How to apply:** v8 sprints are complete. Project moved to v10 backlog.

## Backlog v10 (current)

**v10 is testing + quality consolidation — no new features. 18 code-quality issues Q-1 through Q-18.**

### Sprint 1 (Tier 0 bug fixes) — COMPLETE 2026-05-02

All T0-1 through T0-6 complete.

| Task | What changed | Files |
|---|---|---|
| T0-1 / Q-1 | `edges_created` now counts actual edges (was hardcoded 0) | `src/enrichment/netbox.rs` |
| T0-1 / Q-2 | Pagination offset tracked as local counter, not re-parsed from URL | `src/enrichment/netbox.rs` |
| T0-1 / Q-3 | Token no longer cloned into extra binding; `&cred.password` used directly | `src/enrichment/netbox.rs` |
| T0-1 / Q-4 | `max_concurrent_requests` from `config.extra` (default 2) caps concurrent NetBox fetches via `Semaphore` | `src/enrichment/netbox.rs` |
| T0-1 / Q-5 | Device writes chunked in batches of 100 with debug checkpoint logging | `src/enrichment/netbox.rs` |
| T0-2 / Q-6 | Docstring corrected to say env vars; `--use-vault` stub added with exit 2 | `scripts/seed_servicenow_pdi.py` |
| T0-2 / Q-7 | Verify GET after every upsert POST/PATCH; warn if record not found | `scripts/seed_servicenow_pdi.py` |
| T0-2 / Q-8 | Lookup query uses `limit=1` (was 500); no duplicate risk from missed pages | `scripts/seed_servicenow_pdi.py` |
| T0-3 / Q-9 | `failure_count_30d` renamed to `total_failures`; resets to 0 after 10 consecutive successes; `#[serde(alias)]` for compat | `src/remediation/trust.rs` |
| T0-3 / Q-10 | `persist()` now fire-and-forgets via `std::thread::spawn`; disk I/O no longer holds the lock | `src/remediation/trust.rs` |
| T0-3 / Q-11/12 | Unknown archetype in `default_state_for` logs `warn!` with archetype + rule_id | `src/remediation/trust.rs` |
| T0-4 / Q-13 | `snow_get` retries on 429 with exponential backoff (1s→2s→4s→bail, cap 60s) | `src/enrichment/servicenow.rs` |
| T0-4 / Q-14 | `SnowRef` has custom `Deserialize` handling both `{display_value}` object and plain string | `src/enrichment/servicenow.rs` |
| T0-4 / Q-1 | `edges_created` fixed in ServiceNow enricher (same pattern as NetBox) | `src/enrichment/servicenow.rs` |
| T0-5 / Q-15 | `discover_yang_paths.py` checks for pyang at startup, exits 2 before any git clone if missing | `scripts/discover_yang_paths.py` |
| T0-6 / Q-17 | Setup wizard skip confirmed working (lines 163-210 of Setup.svelte) | verification only |
| T0-6 / T1-2v9 | DeviceDrawer enrichment panel confirmed present and wired to `/api/devices/{addr}/enrichment` | verification only |
| T0-6 / T2-6v9 | ServiceNow EM event filter policy confirmed (`severity_passes()` + `min_severity` config) | verification only |
| bonus | Pre-existing clippy `collapsible-if` warnings fixed in `elastic.rs` and `splunk_hec.rs` | `src/output/{elastic,splunk_hec}.rs` |

### Sprint 2 (Tier 1 unit tests) — PENDING

Next: T1-1 through T1-9 (unit tests for all zero-test modules).

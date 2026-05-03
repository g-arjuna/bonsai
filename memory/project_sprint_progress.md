---
name: Sprint Progress
description: Backlog sprint completion status — v8 through v10 sprint progress
type: project
---

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

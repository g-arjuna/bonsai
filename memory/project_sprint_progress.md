---
name: Sprint Progress
description: Backlog sprint completion status — v8 through Bv1 sprint progress
type: project
---

## Backlog Bv1 (Bravo series — current, supersedes v12)

**Bv1 Sprint 3 — Graph embeddings + Feature schema — COMPLETE 2026-05-05**

- T2-1: `python/bonsai_ml/embeddings.py` — spectral graph embedding (Laplacian eigenmaps via sklearn) using existing `ml` deps (no new packages). `fetch_adjacency` reads `/api/topology`, `compute_spectral_embedding` builds precomputed adjacency matrix + sklearn SpectralEmbedding, `push_embeddings` posts to new API. `run_embedding_pipeline` is the end-to-end entry point. CLI: `python -m bonsai_ml.embeddings`. Model card at `python/bonsai_ml/model_cards/spectral_v1.md`.
- T2-1 backend: `DeviceEmbedding` graph node (id = `"{address}:{version}"` composite key) added to `init_schema()`. `write_device_embeddings` (batch upsert via MERGE) and `list_device_embeddings` (newest first) added to GraphStore. Routes: `POST /api/graph/embeddings/upsert`, `GET /api/graph/embeddings/:address`.
- T7-11: `python/bonsai_ml/feature_schema.py` — `FeatureSchema` dataclass with `save`/`load`/`matches`. Schema hash = SHA-256 of canonical JSON (excludes `created_at` and prior hash, so re-exports don't drift). `SPECTRAL_V1_SCHEMA` canonical instance; hash frozen in test to catch accidental hyperparameter changes. Canonical JSON saved to `python/bonsai_ml/schemas/spectral_v1.json`.
- T5-3: 24 Python tests in `python/tests/test_embeddings.py` (adjacency parsing, embedding shape/reproducibility/star-topology sanity, push payload, pipeline e2e, schema hash determinism/drift, save/load roundtrip). 4 Rust tests for DeviceEmbedding roundtrip, multi-version, upsert-overwrite, empty-query.
- BonsaiClient extended: `push_device_embeddings`, `get_device_embeddings`.
- Total: 166 Rust tests pass; 24 Python tests pass.

**Bv1 Sprint 2 — Graph algorithms + Explorer — COMPLETE 2026-05-05**

- T1-4: `src/graph/algorithms.rs` — device centrality, site dependency depth, detection correlation, subscription health by tier, orphan count. `graph_insights` bundles all five. 8 tests. Key finding: all connected devices in 2-spine/4-leaf fixture have undirected degree=2 so land in "aggregation" tier (spine threshold is ≥4).
- T1-5: `src/graph/explorer.rs` — Cypher sanitiser (word-boundary keyword search banning 8 mutation keywords), column extractor (RETURN clause parser, AS alias preference), executor (500-row cap, returns ExplorerResult). 11 tests.
- T1-6: `SavedQuery` graph node + CRUD GraphStore methods (`list_saved_queries`, `create_saved_query`, `delete_saved_query`, `mark_saved_query_run`). Added to `init_schema()`.
- HTTP: `/api/graph/insights`, `/api/explorer/query` (POST), `/api/explorer/saved-queries` (GET/POST), `/api/explorer/saved-queries/:id/delete` (POST) wired in `src/http_server.rs`.
- UI: `ui/src/routes/Explorer.svelte` — two tabs (Query + Insights), 12 curated queries, saved queries sidebar, save modal, Ctrl+Enter shortcut, device centrality/site deps/detection co-fire/tier health display. Wired into App.svelte nav.
- Bug fix: `backfill_remediation_trust_marks` migration marker only recorded when remediations exist — prevents marker from blocking future backfill when store is opened on empty DB. Also: eager `.collect()` closes read cursor before writes (lbug constraint: no concurrent read cursor + write on same connection).
- All 162 tests pass (exit 0).

**Bv1 Sprint 1 — Graph foundation — COMPLETE 2026-05-05**

- T1-1: `src/graph/queries.rs` created with 13 multi-hop production queries (neighbors_of_device, shortest_topology_path, blast_radius, devices_in_environment, detections_in_environment, applications_on_site, devices_missing_enrichment, orphan_devices, detections_without_remediation, subscription_health_for_device, co_firing_detections, device_enrichment_context, topology_edges). All 17 tests pass.
- T1-2: `path_handler` in `src/http_server.rs` replaced — BFS now uses per-device `neighbors_of_device` queries instead of loading all CONNECTED_TO edges at once. Key finding: lbug 0.15.3 `LIMIT 1` with `*1..N` variable-length patterns does NOT guarantee shortest path (Kuzu uses join-based materialization, not BFS). Undirected `[:CONNECTED_TO]-` needed because edges are stored directionally.
- T1-3: `/api/blast-radius/:address?max_hops=N` endpoint added. Capped at max_hops=5 (hop_depth=15 edges).
- T1-7: `src/graph/test_fixtures.rs` created. 2-spine/4-leaf DC + SP pair + isolated device. All fixtures use parameterized queries with `ts()` — inline `timestamp_ns()` function does not exist in lbug Cypher.
- T4-1 (partial): `.cargo/config.toml` changed — `LBUG_SHARED=1` now commented out; static linking is the default. Full static binary verification (ldd assertion) pending a clean static build (~15–30 min first time).

**Still pending for Sprint 1 completion**: static build verification (T4-1 done-when condition).

## Backlog v12 (current — supersedes v11)

**v12 Sprint 1 — Memory + binary fixes — COMPLETE 2026-05-03 (commit 2421332)**
- T0-1: LadybugDB buffer pool capped at min(2 GiB, 25% RAM) for core, min(256 MiB, 10%) for collector. Configured via [graph] buffer_pool_bytes in bonsai.toml. Root cause of 9 GB memory bug fixed.
- T0-2: Debounce HashMap replaced with lru::LruCache(1024) — bounded by config not runtime.
- T0-3: Default event bus capacity reduced from 2048 to 512.
- T0-4: Release binary self-contained (RUNPATH=$ORIGIN + auto-copy liblbug.so.0 in build.rs). No LD_LIBRARY_PATH needed.
- T0-8: Startup phase timing logs added (config_load, graph_open, schema_init, backfill, ready).
- T1-2/T1-3: Memory-budget CI updated to 10-min run / 1.5 GiB budget; resource_contract.md updated.

**v12 Sprint 2 — Always-on infrastructure — COMPLETE 2026-05-03 (commit b5162de)**
- T0-5: restart:unless-stopped on all services in compose-external.yml + servicenow-mock
- T0-6: --reset flag on seed_netbox.py, seed_splunk.py, seed_elastic.py, seed_servicenow_pdi.py
- T3-2: lab/dc/Makefile + lab/sp/Makefile (up/down/status/reset via containerlab --reconfigure)
- T3-3: scripts/reset_for_test.sh — canonical pre-test reset (wipes data, restarts bonsai, services stay up)
- T0-9: trap cleanup EXIT in all 6 e2e scripts
- T2-2: .github/workflows/release.yml — bonsai-linux-amd64/arm64 tarballs + SHA256 on v* tags
- T2-3: bonsai self-test subcommand — 4 checks with [✓]/[✗] output

**v12 Sprint 3 — UI liveness — COMPLETE 2026-05-04**
- T0-7 / T4-1: SSE event broadcasting — detection_fired (write_detection), remediation_outcome (write_remediation), collector_status_change (CollectorManager connect/disconnect) published; CollectorManager wired to GraphStore event_sender in main.rs
- Collectors.svelte: SSE subscription on collector_status_change + 60s poll fallback
- Incidents.svelte: SSE subscription on detection_fired / incident_grouped / remediation_outcome + 60s poll fallback
- T0-10: Operations.svelte: 5s live polling + RSS/archive/graph sparklines (12-sample ring buffer)
- T4-2: Playwright screen-level assertions — tests/ui_driver/collectors.spec.js + incidents.spec.js
- T4-3: Chaos harness extended — /api/incidents check (wait_for_incident), --write-matrix flag, Markdown matrix report to docs/test_results/chaos_matrix/<date>.md
- T4-4: .github/workflows/screenshot-diff.yml + tests/ui_driver/screenshots.spec.js (@screenshot tag, 2% pixel tolerance)
- T4-5: docs/ui_audit_2026-05-04.md — full per-route audit; 4 open issues identified for v13

**v12 Sprint 4 — Startup polish — COMPLETE 2026-05-05 (commit e9f537a)**
- T5-3: MigrationMarker node table; backfill_remediation_trust_marks skips on repeat starts via marker 'backfill_trust_v1'.
- T5-2: --once-and-exit exits after phase=ready; .github/workflows/startup-time.yml CI fails if >25% over 3000ms baseline.
- T1-4: buffer_pool_bytes on GraphStore + InProcessBus::capacity(); /api/operations adds memory_budget_bytes + memory_rss_pct_of_budget; Operations.svelte shows RSS % of budget.
- T3-4: check_external.sh --watch polls every 30s → runtime/external_status.json; --interval=N and --output=FILE flags.
- T3-5: check_lab.sh emits summary block: bgp_sessions_established, bgp_sessions_total, evpn_routes_present, srv6_reachability_verified, warnings[], overall_passed; exits 1 on failure.

**v12 complete. Next: v13 (open issues from UI audit: Topology/Devices SSE liveness, Approvals SSE, enricher/adapter event publishing)**

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

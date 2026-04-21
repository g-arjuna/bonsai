# BONSAI — Consolidated Backlog v3.0

> Supersedes `BONSAI_CONSOLIDATED_BACKLOG_V2.md`. Produced after reviewing the `codex-backlog-priority-v2` branch on 2026-04-21, including `BACKLOG_V2_EXECUTION_LOG.md`, the Rust source under `src/`, Python SDK updates, path profile templates, chaos plans, and the Svelte UI onboarding workspace.
>
> **What changed from v2**: the vast majority of v2 Tier 0 and Tier 1 items landed. Registry, discovery, path profiles, subscription verification, Parquet archive, distributed runtime seam (first slice), trust marks, readiness checks, chaos plans — all in. What remained is specific: onboarding UX is too thin for operators, credentials are env-var-only (impractical), site hierarchy is a string attribute not a first-class concept, distributed collector needs hardening, Model C needs data volume. This backlog layers the strategic evolution on top — Path A graph embeddings, Path B GNN, and an investigation agent — with explicit guardrails so the ML work does not starve the operational work.

---

## Table of Contents

1. [Progress Since v2 — Verified Against the Branch](#progress-since-v2)
2. [TIER 0 — Code Quality Fixes](#tier-0)
3. [TIER 1 — Onboarding and Credentials Redesign](#tier-1)
4. [TIER 2 — Distributed Hardening and Remaining Carryovers](#tier-2)
5. [TIER 3 — Strategic Evolution: Path A → Path B GraphML](#tier-3)
6. [TIER 4 — Investigation Agent (Agentic Layer)](#tier-4)
7. [TIER 5 — Original Backlog Carryovers and Extensions](#tier-5)
8. [Recommended Execution Order](#execution-order)
9. [Branch Merge Plan](#merge-plan)
10. [Guardrails](#guardrails)

---

## Progress Since v2 — Verified Against the Branch

**Completed and removed from this backlog** (all verified in the branch with code review):

| v2 item | Status | Evidence |
|---|---|---|
| T0-6-cont shared extractor migration | ✅ Done | `rules/bgp.py`, `rules/interface.py`, `rules/bfd.py` delegate to `extract_features_for_event`; `rules/topology.py` remains poll-based by design; regression in `python/tests/test_t0_fixes.py` |
| T0-7 ADR debt | ✅ Done | Six 2026-04-20 ADRs added to `DECISIONS.md` covering event bus seam, debounce scope, retention tie-breaking, verification field aliasing, shared extractor split, typed defaults |
| T0-8 retention tie-breaking | ✅ Done | `retention.rs::prune_events_by_count` now deletes exact oldest IDs; same-timestamp collision unit test |
| T1-1c Parquet archive | ✅ Done | `src/archive.rs` (310 lines), `[archive]` config, `scripts/archive_stats.py`, ADR entry, zstd compression, hour+device partitioning, live lab smoke verified |
| T1-3a/b ApiRegistry + lifecycle | ✅ Done | `src/registry.rs` (338 lines) with JSON persistence at `bonsai-registry.json`, broadcast→mpsc bridge, seed-from-TOML merge; `main.rs` subscriber manager starts/stops/restarts on `RegistryChange` events; full test coverage |
| T1-3c DiscoverDevice RPC | ✅ Done | `src/discovery.rs` (782 lines), `CapabilitySummary`, vendor/encoding detection, env-var-only credential interface, structured `DiscoveryReport`, 3 tests |
| T1-3d path profiles | ✅ Done | Four templates in `config/path_profiles/` with required model gating, dropped-path warnings, built-in fallback |
| T1-3e subscription verification | ✅ Done | `src/subscription_status.rs` (313 lines), 30s window, pending/observed/subscribed_but_silent states, event-family-aware matching, graph-backed |
| T1-3f HTTP onboarding endpoints | ✅ Partially done | Endpoints exist, UI exists, but **UX is incomplete** — this becomes T1 in v3 (see below) |
| T1-2a distributed runtime seam | ✅ Done (first slice) | Runtime modes (`all`/`core`/`collector`), `TelemetryIngest` gRPC, `src/ingest.rs` (187 lines), collector forwarder with reconnect, core-side republish; zstd/mTLS/queue/validation deferred |
| T2-4 playbook validation script | ✅ Done | `scripts/validate_playbooks.py` + `python/tests/test_validate_playbooks.py` |
| T2-5 data hygiene (graph-level) | ✅ Done | Cutoff ADR, `RemediationTrustMark` nodes, startup backfill, trusted-remediation filter in Model C export — cleaner than the timestamp-filter approach originally proposed |
| T3-2-cont chaos plans | ✅ Done | `chaos_plans/baseline_mix.yaml`, `bgp_heavy.yaml`, `gradual_only.yaml`, README, WSL helper scripts; live 1h `baseline_mix` completed with 17 injections recorded |
| T3-3 training readiness check | ✅ Done | `scripts/check_training_readiness.py` with gRPC+HTTP fallback, validated live 2026-04-20 |

**Still outstanding from v2 (carried into v3 under new groupings):**

- T1-2 remainder (compression, disk queue, mTLS, live two-process validation) → T2 in v3
- T2-2 (ML feature schema versioning) → T5 in v3
- T2-3 remainder (training script validity checks — partial) → T5 in v3
- Model C volume — operational, not code → T3 dependency, tracked passively

**Discipline observations from reviewing the branch:**

- **Execution log is honest.** The `BACKLOG_V2_EXECUTION_LOG.md` flags blockers (Windows lbug zstd conflict) rather than papering over them. Keep this pattern.
- **ADR hygiene recovered.** Six dated entries on 2026-04-20 closed the debt v2 flagged. ADRs for decisions landing in v3 work must happen *at commit time*, not in a batch later.
- **Tests are behavioural.** The new Rust tests for registry, discovery, and subscription status assert outcomes, not internals. Good pattern — carry forward.

---

## <a id="tier-0"></a>TIER 0 — Code Quality Fixes Surfaced by Review

These are found-during-review items. Small, cheap, preventative. Do them in the same sprint as their surrounding work.

### T0-1 (v3) — Archive small-file proliferation

**What**: `src/archive.rs::flush_buffer` creates a new Parquet file per flush, per partition. At 10-second flush × 4 devices × 24 hours that's ~35,000 files/day. Parquet has non-trivial per-file overhead; readers pay for filesystem traversal; compression efficiency drops at small file sizes.

**Why**: this is a latent scaling problem. Lab-scale it's tolerable, but when T3-B GNN training wants to read 3 months of data, the read side will stall on directory traversal.

**Where**: `src/archive.rs`

**Options** (pick one, document in ADR):
- (Recommended) Append-to-current-hour: each partition has one file per hour; flushes append to it; closed at hour boundary. Requires managing open `ArrowWriter` handles per partition. ~80 lines of change.
- Alternative: compaction job — leave small files but add a daily background task that merges all files for a finished hour into one. Simpler but doubles I/O.

**Done when**:
- A one-hour run with 4 devices produces at most 4 Parquet files (one per device per hour), not dozens per device
- Compression ratio in the flush log is higher than today's per-flush values
- A read-path test with `pd.read_parquet("archive/", glob="**/*.parquet")` on a week's worth of data completes in reasonable time

---

### T0-2 (v3) — `normalize_address` validation

**Execution update — 2026-04-21**: completed. `normalize_address` now validates
`host:port` before persistence, accepting hostnames, IPv4, and bracketed IPv6
with ports in `1..=65535`, and returning the explicit error
`device address must be host:port` for invalid input. Focused release tests cover
valid and invalid forms.

**What**: `src/registry.rs::normalize_address` only trims whitespace. Any garbage string is accepted, persisted to `bonsai-registry.json`, and later fails at connect time with an opaque gNMI error.

**Why**: onboarding UX today surfaces connect-time failures instead of input-time failures. The UI displays a cryptic error several seconds after Save because registry accepted the bad address.

**Where**: `src/registry.rs`, line 247-253

**Done when**:
- Address validates as `host:port` where host is a hostname, IPv4, or bracketed IPv6, and port is 1-65535
- A specific error message (`device address must be host:port`) is returned on Save for invalid input
- Unit test covers valid and invalid forms

---

### T0-3 (v3) — Ingest wire format wasteful

**What**: `src/ingest.rs::telemetry_to_ingest_update` serializes `TelemetryUpdate.value` to a JSON string on the wire. For counter updates this triples the byte count compared to a proto-native representation.

**Why**: the distributed seam is new and not yet hot, but when T2-2 lands (compression and the disk-backed queue), this doubles the compressed size for no reason. Cheaper to fix now while callsites are few.

**Where**: `src/ingest.rs`, `proto/bonsai_service.proto`

**Design**: replace `string value_json` with `google.protobuf.Any value` or a `bytes` field with documented encoding (MessagePack recommended for size/parse balance).

**Done when**:
- Round-trip test covers binary encoded form
- On-wire byte count for 1000 interface-counter updates is at least 30% smaller than today
- ADR documents the encoding choice

**Execution update - 2026-04-21**: Implemented `bytes value_msgpack = 7` and encode/decode
`TelemetryUpdate.value` with MessagePack in `src/ingest.rs`. Regenerated the Python gRPC
stub so Rust and Python agree on the new proto field. Added focused release tests for binary
round-trip and for 1000 counter-value payloads showing at least 30% smaller encoded value bytes,
with a separate assertion that total protobuf bytes are smaller than the previous JSON field.
Important nuance: a value-field-only change cannot make the entire per-update protobuf 30%
smaller for scalar counters because `collector_id`, `target`, and `path` dominate the message;
full stream-level reductions should be handled by the later T2 compression/queue/batching work.

**Priority**: do this before T2 distributed hardening. Once the queue and compression ship on top of the current format, changing it becomes a breaking protocol change.

---

### T0-4 (v3) — UI polls every 10 seconds even when idle

**What**: `ui/src/lib/Onboarding.svelte::onMount` sets `setInterval(loadDevices, 10000)`. That's a full `GET /api/onboarding/devices` every 10 seconds for every open browser tab, forever.

**Why**: cheap today, wasteful tomorrow. When status updates get richer (per-path telemetry latency, trust marks, etc.) the payload grows and this amplifies.

**Where**: `ui/src/lib/Onboarding.svelte`

**Options**:
- Reuse the existing SSE `/api/events` stream and emit registry-change and subscription-status-change events over it. Pages subscribe; no polling.
- At minimum, pause polling when the tab is hidden (`document.hidden` check).

**Done when**:
- Onboarding page stops network traffic when browser tab is backgrounded
- Registry edits and subscription-status transitions appear in the UI without waiting for the next poll tick

---

### T0-5 (v3) — Rust clippy/format baseline

**What**: the CLAUDE.md build commands include `cargo clippy --release -- -D warnings` but no evidence in the branch that it ran clean across all new modules. Run it; fix what it flags.

**Where**: whole tree

**Done when**: `cargo clippy --release --all-targets -- -D warnings` passes cleanly; any rule that's legitimately too noisy (e.g. `clippy::too_many_arguments` on discovery's request builder) is explicitly allowed with a comment and rationale.

**Execution update - 2026-04-21**: Completed. `cargo clippy --release --all-targets -- -D warnings`
passes cleanly. The only flagged issue was `clippy::result_large_err` in
`src/api.rs::target_from_managed_device`; fixed structurally by returning a small
validation error from the helper and converting it to `tonic::Status` at the RPC boundary.
No lint allow was required.

---

## <a id="tier-1"></a>TIER 1 — Onboarding and Credentials Redesign

This is the single biggest area of v3 work. The scaffolding from v2 (discovery, registry, path profiles, subscription status) is correct — the onboarding experience built on top of it is thin.

The review findings are specific:

- **Not a wizard.** `Onboarding.svelte` is a single form with 9 fields. The vision was multi-step: (1) address + creds → (2) discovery report → (3) path/profile selection → (4) confirm.
- **No path selection.** Discovery returns recommended profiles, UI displays them read-only, system auto-applies the first.
- **Env-var-only credentials are impractical.** Today's flow: operator sets `BONSAI_DEVICE_USER` and `BONSAI_DEVICE_PASS` as OS env vars *before* starting bonsai; all devices sharing those vars share credentials. For per-device creds, operator creates `BONSAI_LEAF1_USER`, `BONSAI_LEAF2_USER`, etc., and must restart bonsai to pick them up. No operator will do this at scale.
- **Site is a string.** `TargetConfig.site` is a free-text field. No `Site` graph node, no hierarchy, no geography, no "site already exists" lookup.

### T1-1 — Credentials vault

**What**: a local encrypted credential store. Operator enters a username and password *once* in the UI, the system stores it encrypted at rest, gives it an alias (e.g. `srl-lab-admin`), and devices reference the alias instead of env var names.

**Why**: this is the minimum change that makes onboarding actually usable for someone other than a developer who already has their shell configured. Env vars don't disappear — they remain a valid option for headless/CI use — but they stop being the only option.

**Where**:
- New file: `src/credentials.rs` — vault implementation
- New dependency: `age` (modern rust-native encryption, ~400K downloads/week, well-maintained) or `ring`/`aes-gcm` if you want lower-level control
- Modification: `proto/bonsai_service.proto` — new `AddCredential`, `ListCredentials`, `RemoveCredential` RPCs
- Modification: `src/api.rs` — wire the RPCs
- Modification: `config.rs` — `TargetConfig` gains `credential_alias: Option<String>`
- Modification: `registry.rs` — resolve credentials by alias at connect time; existing env_var fields remain as fallback
- Modification: `http_server.rs` — new `/api/credentials` endpoints (GET list names only, POST add, DELETE remove)
- Modification: `ui/src/lib/Onboarding.svelte` or new `Credentials.svelte` workspace

**Design — vault layout**:
```
bonsai-credentials/
  vault.age          — encrypted file, opened at startup with an operator-provided passphrase
  metadata.json      — unencrypted: alias list, last-used timestamps (never the secret itself)
```

Vault is unlocked on bonsai startup with a passphrase (provided via env var `BONSAI_VAULT_PASSPHRASE`, CLI flag, or — on first run — stdin prompt). Once unlocked, credentials are cached in memory. Vault is never written with the passphrase; each write re-encrypts with the passphrase currently held in memory.

**Design — credential lookup order**:
1. `credential_alias` set → look up in vault (in-memory after unlock)
2. `username_env` and `password_env` set → read from environment
3. Inline `username`/`password` (lab-only, already supported in `bonsai.toml`)
4. None → return error before dialing gNMI

**Security constraints** (non-negotiable):
- The vault file is not readable by the HTTP API — credentials never leave the process via any endpoint other than the gNMI push channel to the target device
- `ListCredentials` returns aliases and metadata only, never the secret
- Passphrase is never logged, never serialised, never written to disk in plaintext
- Wiping the vault clears in-memory copies immediately

**Done when**:
- A fresh install without any env vars set can add a device through the UI by typing a username and password, assigning them alias `srl-lab-admin`, then adding multiple devices that reference the same alias
- Bonsai restart unlocks the vault and subscribers reconnect without operator intervention beyond the passphrase prompt
- Wrong passphrase yields a clear error at startup, not a pile of "credential missing" failures when devices try to connect
- Integration test covers the full flow
- ADR entry explains the design and the threat model (protects against on-disk snooping, not against a compromised process)

**Execution update - 2026-04-21**: Backend plus minimal onboarding UI slice implemented on
`codex/t1-1-credentials-vault`. Added `src/credentials.rs` using passphrase-encrypted
`age` storage at `bonsai-credentials/vault.age` plus plaintext alias metadata at
`metadata.json`; the directory is gitignored. Added `credential_alias` to `TargetConfig`,
managed-device proto/API/HTTP payloads, and the Python client. Added gRPC
`ListCredentials`, `AddCredential`, and `RemoveCredential` plus HTTP `/api/credentials`
endpoints. Subscriber startup, discovery, and remediation now resolve credentials in the
intended order: vault alias first, then env vars, then inline lab config. The current unlock
mechanism is `BONSAI_VAULT_PASSPHRASE`; a richer passphrase prompt remains part of the
later wizard polish. Focused release tests cover add/list/resolve/remove, restart decrypt,
and wrong-passphrase failure at vault open.

**Explicitly out of scope for v1 of the vault**:
- Remote secret stores (Vault, 1Password, cloud KMS) — add later via a `CredentialStore` trait if there's demand
- Per-device encryption (one passphrase protects the whole vault)
- Key rotation automation (manual re-add works for v1)

---

### T1-2 — Site as a first-class graph entity

**What**: `Site` becomes a real node in the graph, with hierarchical parent-child relationships, geography coordinates for optional map visualisation, and `Device-[:LOCATED_AT]->Site` edges. Device onboarding picks a site from the existing set or creates one.

**Why**: the current "site is a string" model loses information the moment the operator mis-types it. More importantly, when detections fire, the causal traversal cannot reason across sites ("is this failure affecting only the London DC or globally?") without structured site data. When T3 GraphML lands, site embeddings will be a natural feature — but only if Site is a node.

**Where**:
- `src/graph.rs` — schema additions for `Site` node and `LOCATED_AT` edge
- `src/api.rs` — new `ListSites`, `AddSite`, `UpdateSite` RPCs
- `src/http_server.rs` — new `/api/sites` endpoints
- `config.rs` — `TargetConfig.site` stays a string *alias* that resolves to a Site node at registry write time
- UI — site picker component, site-management workspace

**Graph schema**:
```
Site:
  id: string              — stable ID (UUID or human-readable slug)
  name: string            — display name ("London DC A")
  parent_id: string       — FK to another Site (nullable for top-level)
  kind: string            — "region" | "country" | "city" | "dc" | "rack" (enum, extensible)
  lat: float (optional)   — geo coordinate for map rendering
  lon: float (optional)
  metadata_json: string   — free-form operator notes

Edges:
  (Device)-[:LOCATED_AT]->(Site)
  (Site)-[:PARENT_OF]->(Site)
```

**Design principles**:
- A site can be a leaf (rack) or a container (region). Recursive parent chain is the hierarchy. No assumption about depth — an operator with only one DC uses one Site; someone with 20 global DCs uses a full tree.
- Moving a device between sites is a graph operation: delete old `LOCATED_AT`, insert new one. History is preserved if the edge carries a `valid_from`/`valid_to` (optional — align with the bitemporal work in T4-3).
- The UI does not force operators to define a hierarchy. A single flat list of sites is valid. Hierarchy unlocks optional features (region-scoped queries, aggregated dashboards).

**Done when**:
- Graph contains Site nodes and `LOCATED_AT` edges after onboarding
- UI onboarding wizard shows a searchable dropdown of existing sites and a "+ New site" link that opens a lightweight form
- A Cypher query like `MATCH (d:Device)-[:LOCATED_AT]->(:Site {kind: "dc", name: "lab-london"}) RETURN d.hostname` returns the expected devices
- An integration test covers create-site → add-device-with-site → query-by-site
- Existing string-site devices are migrated on startup: each unique string value becomes a Site node with `kind: "unknown"` and the old string as name; devices get `LOCATED_AT` edges to them. ADR documents the migration.

**Execution update - 2026-04-21**: First backend/UI slice implemented after the credential
vault checkpoint. Added `Site` graph schema plus `LOCATED_AT` and `PARENT_OF` relationships.
`GraphStore::sync_sites_from_targets` now migrates registry `TargetConfig.site` strings into
stable Site IDs and links devices on startup and after registry add/update. Added gRPC
`ListSites`, `AddSite`, and `UpdateSite`, HTTP `/api/sites`, Python `list_sites`/`add_site`,
and a minimal onboarding site picker/new-site form. Focused release test verifies
create-site-from-target and `Device-[:LOCATED_AT]->Site` query behavior.

**Explicitly out of scope for v1**:
- Map visualisation UI — defer to T5-UI-map
- Site ACLs — bonsai has no auth model; sites are metadata, not security boundaries
- Automatic site assignment from device hostname patterns — manual for v1

---

### T1-3 — Onboarding wizard UI

**What**: replace the single-form `Onboarding.svelte` with a four-step wizard that matches the vision. Step 1 collects connection details. Step 2 runs discovery and displays the report. Step 3 lets the operator pick a profile and optionally toggle individual paths within it. Step 4 confirms and saves.

**Why**: today's form surfaces all 9 fields at once with no progression. An operator onboarding 50 devices does not want to scroll past CA cert inputs every time. The wizard also enforces a natural order: you cannot save without discovery succeeding, which prevents the current failure mode of "Saved, but subscriber fails to start 30 seconds later for reasons I can't see."

**Where**:
- `ui/src/lib/Onboarding.svelte` — refactor to multi-step
- `ui/src/lib/onboarding/Step1Connection.svelte` — address, creds (alias picker + new-credential inline form), TLS
- `ui/src/lib/onboarding/Step2Discovery.svelte` — discovery report with vendor, models, recommended profiles
- `ui/src/lib/onboarding/Step3Profile.svelte` — profile picker, per-path toggles, preview of what will be subscribed
- `ui/src/lib/onboarding/Step4Confirm.svelte` — summary, final site selection, save button
- `ui/src/lib/onboarding/DeviceList.svelte` — separate workspace for managing existing devices (not a step)
- `src/http_server.rs` — new endpoint `POST /api/onboarding/devices/with_paths` that accepts a specific path selection (so Step 3 can override the auto-selected profile)

**Design — wizard state**:
- State lives in the parent `Onboarding.svelte` and is passed to each step component
- Navigation: Back enabled on steps 2+; Next only enabled when current step is valid
- Discovery result lives in wizard state — if operator goes back from Step 3 to Step 1 and edits the address, discovery is invalidated and Step 2 re-runs automatically

**Path-selection constraints**:
- Operator can deselect optional paths (those marked `optional: true` in the profile YAML)
- Operator cannot deselect required paths — those are greyed out with a tooltip explaining why
- Operator can see the full path string (not just the profile name) so they can verify it matches their mental model

**Done when**:
- Step 1 is completable without any env vars — operator picks credentials from vault dropdown
- Step 2 displays all fields of `DiscoveryReport` including warnings
- Step 3 shows each recommended profile as a card; selected profile's paths render as a checklist
- Step 4 shows a summary of exactly what will happen when Save is clicked (subscriber will start against N paths, expected first telemetry within 30 seconds)
- DeviceList workspace is separate navigation, not part of the wizard
- Going through the wizard end-to-end against an SR Linux leaf produces a device with specifically-selected paths and observable `SubscriptionStatus` transitions from pending to observed

**Execution update - 2026-04-21**: implementation slice completed.
`Onboarding.svelte` is now a four-step wizard (address/credentials, discovery,
profile/path selection, confirm) with a separate Device List workspace. Discovery
now exposes `optional` on each `SubscriptionPath`; required paths are always
selected and optional paths can be toggled. The new HTTP endpoint
`POST /api/onboarding/devices/with_paths` accepts `selected_paths`, persists
them in `TargetConfig.selected_paths`, and the subscriber honors that concrete
plan when present while retaining Capabilities-derived fallback behavior for
older entries. Managed device responses now show the armed path plan alongside
subscription status.

**Explicitly out of scope**:
- Bulk onboarding (import from CSV) — T5 item
- Onboarding via discovery broadcast (LLDP-based auto-discovery) — future, requires network-side agent
- Operator-created path profiles — templates remain YAML files checked into the repo for v1

---

### T1-4 — Device management improvements that follow from T1-1/2/3

**What**: small adjacent items that only make sense once the wizard and vault exist.

**Where**: mostly UI and minor API.

**Sub-items**:
- **T1-4a**: edit flow uses the same wizard, pre-populated — not today's quasi-modal "edit then save" that bypasses discovery
- **T1-4b**: device removal shows a confirmation with current subscription status and a brief `RemediationTrustMark` summary ("removing this device will tombstone 12 trust marks, 3 of which are active")
- **T1-4c**: bulk operations — select multiple devices → stop all / restart all. Useful for "site maintenance" operations
- **T1-4d**: `bonsai device` CLI commands — `bonsai device add`, `remove`, `list`, mirroring the UI

**Priority**: T1-4a is required. T1-4b/c/d are nice-to-have after the wizard lands.

**Execution update - 2026-04-21**: T1-4a completed. Device-list edit now opens
the same four-step wizard in explicit edit mode, pre-populates the saved device
identity, credentials, role, site, TLS fields, and carries the existing
`selected_paths` plan into discovery matching. Step 3 re-arms required paths and
preserves saved optional choices when they still match the current discovery
recommendations, so editing cannot bypass Capabilities validation or silently
drop the operator's saved subscription plan.

**Execution update - 2026-04-21**: T1-4b/c/d completed as the immediate
operator-control slice. Removal now fetches a confirmation summary with current
subscription counts and linked/trusted `RemediationTrustMark` counts before
deleting a device. Managed devices have an `enabled` registry flag; bulk
Stop/Start/Restart actions update that flag and let the subscriber manager stop
or restart selected devices without deleting them. The Windows binary now
supports `bonsai device list|add|remove|stop|start|restart` against the same
local registry file, providing the requested CLI mirror for common device
operations.

---

## <a id="tier-2"></a>TIER 2 — Distributed Hardening and Remaining Carryovers

The distributed runtime seam landed in v2 as a minimum viable shape. Before it becomes operationally real, these four items need to land.

### T2-1 — zstd compression on the ingest stream

**What**: enable tonic's zstd feature on the `TelemetryIngest` stream so collector→core bandwidth is compressed.

**Known blocker**: tonic's zstd feature pulls in a `zstd` crate version that conflicts with LadybugDB's bundled `zstd.lib` on Windows MSVC builds — duplicate symbol linker error.

**Resolution options** (pick one, test both):
- Pin tonic to a version that uses a compatible zstd version, or pin the `zstd` crate explicitly
- Build LadybugDB with `BUNDLE_ZSTD=OFF` so it uses the system zstd, which shares a symbol table with tonic's
- Use gzip instead of zstd — lower ratio but fewer crate-level conflicts. Document the tradeoff.

**Done when**: a collector-core run on the lab reports a compression ratio in tonic logs; the existing round-trip test passes with compression enabled; Windows release build is clean.

---

### T2-2 — Disk-backed collector queue

**What**: when the core is unreachable, the collector should persist incoming telemetry to a local disk queue and replay it once connectivity is restored.

**Why**: without this, any core outage silently drops telemetry. For a monitoring system whose whole purpose is detect-heal, silent data loss is a fundamental broken-ness.

**Where**:
- `src/ingest.rs` — modify `send_bus_updates` to first append to a local queue
- New dependency: `sled` (embedded KV), `rocksdb`, or a simple append-only file format
- Config: `[collector.queue]` section with path, max size, retention behaviour

**Design**:
- Queue is append-only on the collector side
- Forwarder drains the queue in order
- Queue entries older than `max_age_hours` or beyond `max_bytes` are dropped with a loud warning
- On core reconnect, replay happens before any new telemetry goes on the wire
- On collector restart, queue is read from disk and replay resumes

**Done when**:
- Simulating core outage (kill `bonsai core`) for 5 minutes while collector keeps running produces zero data loss when core comes back
- Queue size logged periodically so operators see when they're getting close to dropping
- ADR documents the queue format and the retention policy

---

### T2-3 — mTLS between collector and core

**What**: both sides authenticate via certificates. Today the `TelemetryIngest` channel is unauthenticated — a malicious collector could send arbitrary telemetry into the graph.

**Why**: even in a lab, this is the minimum credibility bar for the distributed mode. It's also the path to shipping bonsai as something an operator could actually deploy across security zones.

**Where**:
- `src/ingest.rs` — client-side TLS config
- `src/api.rs` — server-side TLS + client cert verification
- Config: `[runtime.tls]` section with cert paths on both sides
- Documentation: a `docs/distributed_tls.md` with openssl commands to generate the lab CA and certs

**Done when**:
- Collector with a valid cert connects successfully
- Collector with no cert or an expired cert is rejected at handshake
- ADR documents the CA structure for the lab

---

### T2-4 — Live two-process validation

**What**: the distributed slice has not yet been run as two real processes against the lab. Execute it. Document it.

**Where**: `docs/distributed_validation.md`

**Done when**: a step-by-step reproducible run exists — collector on one machine, core on another, lab devices, healing loop closes end-to-end, metrics confirm archive and graph writer both receive the same telemetry. Anything that breaks becomes a follow-up ticket.

---

### T2-5 — Model C data accumulation (operational)

**Not a code item.** Once T1 (onboarding) and T3-2-cont chaos plans are running continuously, Model C accumulates trusted success rows. This is tracked passively — the readiness script surfaces current count weekly.

**Unblocks**: Model C training and therefore the remediation selector part of the detect-heal loop.

**Expected timeline**: 2–4 weeks of continuous chaos before Model C crosses the readiness bar.

---

## <a id="tier-3"></a>TIER 3 — Strategic Evolution: Path A → Path B GraphML

This is the strategic direction you asked me to bake in. Bonsai today is a graph-native AIOps system with tabular ML. The Google/NetAI ANO paper defines GraphML as message-passing GNNs that operate on graph structure directly. The gap is real and the evolution is two stages.

**Why now**: the branch's onboarding, archive, chaos, and registry work mean bonsai is finally producing the *kind* of data a GNN wants — relational, structured, with topology. It would be premature to start on GNN work before the data exists. With T1-1c archive landed and chaos running, the data starts accumulating.

**Why gradually**: a GNN project that consumes three months and produces no operational improvement would kill the rest of the project. The T3-A stepping stone delivers topology-awareness with existing-ML infrastructure. T3-B is the destination — start it only after T3-A ships and the operational machinery is stable.

### T3-A — Graph embeddings as a stepping stone

**What**: generate node2vec or GraphSAGE embeddings for Device nodes from the bonsai graph, persist them, concatenate with the existing tabular feature vector, retrain the anomaly detector. The ML model stays IsolationForest (or Autoencoder, whichever was picked in Phase 5); what changes is that each feature vector now carries implicit topology information.

**Why this is accessible**:
- No new ML framework — existing Python `networkx` + `node2vec` library does the heavy lifting
- No message-passing inference at runtime — embeddings are precomputed from the graph once per training cycle, looked up at inference time
- Model code barely changes — feature vector grows from ~10 dims to ~30-40 dims; the rest is downstream

**Why it's not GraphML-in-the-strict-sense**: the model still treats the vector as flat. But it now has features that encode "this device is a spine with high centrality" vs "this device is a leaf with one upstream," which is the information topology-blind ML was missing.

**Where**:
- New file: `python/bonsai_sdk/graph_embeddings.py`
- Dependencies: `networkx`, `node2vec` (well-maintained Python library)
- New script: `scripts/generate_embeddings.py` that reads the graph, builds a NetworkX graph, runs node2vec, writes embeddings to a Parquet file
- Modification: `training.py::features_to_vector` appends the looked-up embedding
- Modification: `ml_detector.py` loads the embeddings file at init, looks up by device address
- New periodic task or CI job: regenerate embeddings weekly (or on topology change)

**Design**:
```python
# graph_embeddings.py
def compute_device_embeddings(graph_client, output_path: str, dims: int = 16):
    """
    Build a NetworkX graph from bonsai's Device + LLDP + BGP edges,
    run node2vec, save a DataFrame of (device_address, embedding_vec) to Parquet.
    """
    edges = graph_client.query("""
        MATCH (a:Device)-[:CONNECTED_TO]->(b:Device) RETURN a.address, b.address
        UNION
        MATCH (a:Device)-[:PEERS_WITH]->(b:Device) RETURN a.address, b.address
    """)
    g = nx.Graph(edges)
    model = Node2Vec(g, dimensions=dims, walk_length=30, num_walks=100).fit(window=10, min_count=1)
    vectors = {node: model.wv[node] for node in g.nodes()}
    pd.DataFrame([{"address": k, **{f"emb_{i}": v[i] for i in range(dims)}} 
                  for k, v in vectors.items()]).to_parquet(output_path)
```

**Done when**:
- Running `scripts/generate_embeddings.py` produces `embeddings/device_embeddings.parquet` from the current graph
- Retraining Model A with embeddings-augmented features succeeds, and the readiness check still passes
- Evaluation: A/B comparison of Model A with-embeddings vs without-embeddings on the same chaos run CSV — expect precision/recall improvement, especially on cascading failures where one device's anomaly is only distinguishable with knowledge of its neighbours
- ADR documents the hyperparameter choice (dims, walk length, window) and the retraining cadence
- `CLAUDE.md` is updated to say "Bonsai uses graph embeddings in its ML pipeline — this is a precursor to Path B GNN"

**Sequencing**: this is the next big ML item after Model C accumulates data.

---

### T3-B — Path B: Proper GNN with message passing

**What**: a PyTorch Geometric (or DGL) based GNN model that ingests the bonsai graph directly and runs message passing across topology. This is what the Google/NetAI paper is doing, in open source, at lab scale.

**Why this is the destination, not the starting point**:
- Requires 2–3 weeks of focused work minimum (model architecture, training loop, inference integration, validation against chaos data)
- Requires meaningful training data volume — Model A with embeddings can show gains on weeks of data; a GNN wants months
- Requires the operational infrastructure (archive, chaos, trust marks, readiness) that's only just landed
- Starting before the above is stable produces a model with no honest validation story

**Where**:
- New directory: `python/bonsai_sdk/gnn/`
- `model.py` — GraphSAGE or GAT architecture, configurable message-passing depth
- `training.py` — custom training loop using PyTorch Geometric's DataLoader
- `inference.py` — loads trained model, scores Device nodes in the live graph
- Integration: `ml_detector.py` gains a `GNNDetector` variant that coexists with the tabular MLDetector
- Dependencies: `torch`, `torch-geometric`, `torch-scatter` — these are heavy; document install instructions for Windows and Linux separately
- New script: `scripts/train_gnn.py`
- New evaluation: `scripts/evaluate_gnn.py` that produces precision/recall curves for GNN vs MLDetector vs rules

**Design principles**:
- The GNN is an *additional* detector, not a replacement. Rules remain the authoritative ground truth for known patterns. GNN catches the things rules miss.
- Node-level task: score each Device node as "anomalous now" vs "healthy." Edge-level and graph-level tasks (link-failure prediction, whole-fabric anomaly) are future.
- Training is offline, batch, against Parquet archive. Inference is online, triggered by state-change events.
- Model serialisation uses `torch.save` with an explicit schema version.

**Done when**:
- GNN trains to convergence on at least 4 weeks of archived data
- GNN integrated as a live detector alongside rules and MLDetector — three parallel paths, independent outputs
- Evaluation doc shows GNN catches at least one class of multi-hop cascading failure that rules and MLDetector miss
- ADR documents the architecture choice, the training cadence, and the accuracy vs latency tradeoff
- Model card (short markdown) documents: inputs, outputs, training data provenance, known failure modes, intended use

**Explicitly out of scope for T3-B v1**:
- Edge-level or graph-level GNN tasks
- Online/continual learning — batch retraining only
- Explainability — GNN predictions do not come with "why" narratives in v1 (that's T4 investigation agent territory)
- Multi-GPU or distributed training — single-machine CPU or single-GPU, whatever the developer has

---

### T3-C — GNN-assisted causal traversal (bridges T3 and T4)

**What**: when the GNN scores a node as anomalous, use the graph structure (not the ML output) to trace the fault's causal chain to upstream and downstream neighbours. This mirrors what the NetAI screenshots show as the "Causal Impact Tree" and "Causal Chain Narrative."

**Why**: GNN tells you *what* is anomalous; causal traversal tells you *why* (upstream cause) and *what else will fail* (downstream impact). These are complementary and easy to combine once both are in place.

**Where**: Python, sits on top of the graph client. No new ML required. Pure Cypher traversals.

**Done when**:
- A detection event for a device displays: its 1-hop graph neighbours, their recent states, any other anomaly scores within the last 5 minutes, and a short narrative built from the traversal
- Narrative format: plain-English, not LLM-generated (deterministic string template from traversal output)
- Integrated into the UI `Trace` view as an additional section

---

## <a id="tier-4"></a>TIER 4 — Investigation Agent (Agentic Layer)

This is the LangGraph-based investigation agent we discussed. **It lives outside the detect-heal hot path and is triggered only by specific conditions** — never in the routine detection loop.

### T4-1 — Investigation agent scaffolding

**What**: a Python service that wraps LangGraph with tools backed by bonsai's gRPC API. Triggered by:
- Any detection event with no matching playbook after 60 seconds
- An explicit operator `/investigate <detection-id>` call from the UI or CLI

**Why this architecture** (re-stating for the record):
- Detect-heal loop must stay deterministic, sub-second, and LLM-independent
- Investigation is inherently exploratory — reasoning about "what does this chain of events mean?" is exactly what LLMs are good at
- Clean handoff: deterministic path produces a DetectionEvent; if the remediation layer can't pick a playbook, agent wakes up
- Tight blast radius: agent never pushes remediation directly — every action goes through the existing `push_remediation` gate with a mandatory human approval step for agent-sourced proposals

**Where**:
- New directory: `python/bonsai_agent/`
- `agent.py` — LangGraph state machine
- `tools.py` — tool definitions wrapping `BonsaiClient` RPCs
- `prompts/` — system prompts, role descriptions
- New gRPC RPCs: `ProposeRemediation(detection_id) → ProposedPlan` (agent output), `ApproveProposal(proposal_id) → ExecutionStatus`
- Modification: `src/api.rs` — the new RPCs, human approval gate
- Dependency: `langgraph`, `anthropic` SDK (or OpenAI)

**Agent tools** (initial set):
- `query_graph(cypher: str) → rows` — safe-mode Cypher execution (read-only)
- `get_topology_context(device_address: str) → dict` — neighbours, role, site, recent states
- `get_recent_detections(window_seconds: int) → list` — what else happened nearby in time
- `get_playbook_library() → list` — all known playbooks with their detection rule IDs
- `suggest_playbook(detection_id: str, rationale: str) → proposal_id` — the only "write" tool; it writes a proposal to the graph, it does *not* execute
- `summarise(text: str) → str` — utility for producing the final investigation note

**LangGraph state machine** (high-level):
```
[detect] → [gather context] → [hypothesise root cause] → [search playbook library]
     ↓                                                          ↓
  no match                                                  match found
     ↓                                                          ↓
[suggest new playbook]                           [propose existing playbook]
     ↓                                                          ↓
             [write proposal to graph] → [notify operator]
                                              ↓
                                      [wait for approval]
                                              ↓
                             (external — human clicks approve in UI)
                                              ↓
                                       [execute via push_remediation]
                                              ↓
                                       [verify via executor.verify()]
```

**Human approval gate — non-negotiable**:
- Agent-proposed remediations *never* execute automatically
- Proposal is written to graph as `Proposal` node with status `pending`
- UI shows pending proposals in a dedicated workspace with a diff-style preview of what would happen
- Operator clicks Approve → standard `push_remediation` flow executes → status transitions to `applied`
- Operator clicks Reject → status `rejected`, proposal archived, reason captured for learning

**Done when**:
- A detection event for which no playbook matches triggers the agent within 60 seconds
- Agent produces a proposal in under 2 minutes (LLM inference is slow — that's fine because we're not on the hot path)
- Proposal appears in UI with a clear narrative of what the agent reasoned
- Operator approves one proposal end-to-end against a lab fault
- Rejection flow works — proposal stays in graph for post-mortem
- Integration test covers the no-playbook-match → agent → proposal → approve path
- ADR documents the architecture separation — specifically that agent is never in the hot path — and the human-approval invariant

**Explicitly out of scope for v1 of the agent**:
- Autonomous execution (the whole point is human-in-the-loop)
- Multi-step tool chains beyond ~8 steps (if an investigation needs more than that, escalate to operator)
- Agent-to-agent communication (for when multiple bonsai instances talk — far future)
- Cost controls — v1 assumes single-digit-dollar per investigation; add budget caps in v2

---

### T4-2 — Investigation workspace in UI

**What**: a new UI workspace `/investigations` that shows:
- Active investigations (agent currently reasoning)
- Pending proposals (awaiting approval)
- Recent completed investigations with outcome (approved/rejected/timeout) and narrative

**Where**: `ui/src/lib/Investigations.svelte`, new `/api/investigations` endpoints

**Done when**: operator can see every agent-driven investigation, approve or reject with a comment, and review history. The Trace view links to the investigation if one was triggered for a given detection.

---

### T4-3 — Agent memory across investigations

**What**: the agent persists a short summary of each completed investigation to the graph as a `PastInvestigation` node, linked to its DetectionEvent. Future investigations query this and use retrieval to learn from prior cases.

**Why**: the agent's value compounds over time only if it accumulates institutional memory. Otherwise every investigation starts from scratch and asks the same questions.

**Where**: Python, graph schema.

**Done when**: an investigation into a BGP session drop surfaces the narrative from the previous BGP session drop investigation as retrieved context in the agent's prompt.

**Priority**: after T4-1 and T4-2 ship.

---

## <a id="tier-5"></a>TIER 5 — Original Backlog Carryovers and Extensions

Items from v2 that still stand. Brief entries — see v2 document for full context.

### T5-1 — Natural-language query layer (was T4-2 in v2)

200-line Python module using existing `Query()` RPC. Two LLM calls per question (plan Cypher + render result). Safe-mode guard rejects destructive verbs. High demo value for low code volume.

### T5-2 — ML feature schema versioning (was T2-2 in v2)

Bundle models as `{"model": ..., "feature_schema_version": N, "feature_names": [...]}`. Assert match at load. Still outstanding.

### T5-3 — Training script validity checks remainder (was T2-3 in v2)

Partially landed (shared readiness module exists). Remaining: row count, class balance, null rate, value range checks integrated into both training scripts with clear error messages.

### T5-4 — Bitemporal schema (was T4-3 in v2)

Defer until NL query (T5-1) produces a question about historical state that today's schema can't answer. That's the forcing function.

### T5-5 — Metrics expansion (was T4-4 in v2)

Event-bus depth gauge, archive lag gauge (oldest unarchived event age), subscriber reconnect frequency, rule firing rate per rule_id, investigation agent success rate.

### T5-6 — Schema migration path (was T4-5 in v2)

Defer until forced by a breaking schema change.

### T5-7 — LLM-assisted playbook suggestion, production version (was T4-6 in v2)

Subsumed by T4-1 agent. Remove from separate tracking.

### T5-8 — Grafeo migration readiness (was T4-7 in v2)

Monitor LadybugDB releases. 60-day-no-release trigger for 3-day evaluation spike.

### T5-9 — TSDB integration adapter (was T4-8 in v2)

Bus subscriber that emits Prometheus remote-write with graph-enriched labels. Additive, not required. Positions bonsai as Telegraf-plus for operators running Grafana.

### T5-10 — Bulk onboarding (CSV import)

Spin-off from T1-3. A CSV with columns `address, role, site, credential_alias` imports many devices at once. Useful when operators onboard a whole rack.

### T5-11 — Geography/map visualisation

Stretch goal following T1-2 (Site as graph node). A world map view with sites as markers, colour-coded by aggregate health. Matches image 1 frame #05 from the NetAI screenshots.

### T5-12 — Multi-layer topology visualisation

Matches NetAI image 1 frames #02, #03, #04 (2D and 3D multi-layer). This is a pure UI item — the data (LLDP, BGP, MPLS when modelled) is already in the graph. Cross-layer correlation rendering is the differentiator.

### T5-13 — Expanded lab topologies

More vendor mix, multi-site lab, deliberate failure variety. Blocked on T1-3 (onboarding wizard) so adding devices doesn't require restart pain.

---

## <a id="execution-order"></a>Recommended Execution Order

Structured as sprints. Each sprint has a theme and a clear exit criterion.

### Sprint 1 — Merge the branch, fix Tier 0 quality

Exit: branch merged to `main`, Tier 0 items closed, CI green.

1. T0-2 `normalize_address` validation (1 session)
2. T0-3 ingest wire format (1 session — must happen before T2 locks the format)
3. T0-5 clippy baseline (1 session)
4. Branch merge (see merge plan below) — 1 session with review

### Sprint 2 — Onboarding v1

Exit: operator can onboard a device via the UI wizard without setting any env vars, without restarting bonsai, and the Site is a real graph node.

5. T1-1 credentials vault (3 sessions)
6. T1-2 Site as graph node with migration (2 sessions)
7. T1-3 onboarding wizard UI (2 sessions)
8. T1-4a edit flow uses wizard (1 session)

### Sprint 3 — Distributed hardening

Exit: collector-core deployment works across two machines with compressed, queued, authenticated transport.

9. T2-1 zstd compression (1–2 sessions — depends on Windows resolution)
10. T2-2 disk-backed queue (2 sessions)
11. T2-3 mTLS (1 session)
12. T2-4 live validation (1 session)
13. T0-1 archive small files (1 session, can overlap)

### Sprint 4 — Operational polish + ML readiness

Exit: chaos running continuously, Model C over the readiness bar, ready for T3-A.

14. T5-3 training script validity checks (1 session)
15. T0-4 UI polling via SSE (1 session)
16. T5-5 metrics expansion (1 session)
17. (Passive) Run chaos plans continuously; Model C accumulates trusted data

### Sprint 5 — Path A graph embeddings

Exit: Model A augmented with topology embeddings shows measurable improvement over baseline.

18. T3-A graph embeddings — implementation + training + evaluation (3 sessions)
19. T5-2 ML feature schema versioning (1 session, required before embeddings schema changes)

### Sprint 6 — Natural language query layer

Exit: operator can type "which BGP sessions went down in the last hour?" and get a cited answer.

20. T5-1 NL query layer (1–2 sessions)

### Sprint 7 — Investigation agent

Exit: agent triggers on unmatched detections, produces proposals, operator approves, full closed loop works once end to end.

21. T4-1 agent scaffolding (3 sessions)
22. T4-2 investigation workspace in UI (2 sessions)

### Sprint 8 — Path B GNN (the destination)

Exit: GNN model running alongside rules and MLDetector, evaluation shows it catches cases the others miss.

23. T3-B GNN (3 weeks of dedicated work — roughly 6–8 sessions)
24. T3-C causal traversal in Trace view (1 session)

### Longer horizon (in any order)

25. T5-9 TSDB adapter
26. T5-11 geography/map UI
27. T5-12 multi-layer topology UI
28. T4-3 agent memory across investigations
29. T5-10 bulk onboarding
30. T5-13 expanded lab

### Defer until forced by pain

- T5-4 bitemporal schema (forced by T5-1 NL query about history)
- T5-6 schema migration (forced by a breaking change)
- T5-8 Grafeo evaluation (forced by LadybugDB 60-day quiet)

---

## <a id="merge-plan"></a>Branch Merge Plan — `codex-backlog-priority-v2` → `main`

The branch contains a substantial amount of correct, tested work. It should be merged, but staged — one big-bang merge is higher risk than necessary and harder to bisect if something regresses.

**Execution update - 2026-04-21**: Sprint 1 Tier 0 fixes were completed and verified, then
`main` was fast-forwarded from `511d9d3` to `ba048d5` and pushed to `origin/main`. The
fast-forward included the prior v2 backlog branch commit plus the V3 Tier 0 closeout commit.
No merge conflicts occurred. Verification before merge: `cargo test --release normalize_address`,
`cargo test --release telemetry_ingest`, `cargo clippy --release --all-targets -- -D warnings`,
and `cargo build --release`.

### Stage 1 — Tier 0 fixes to main first (1 PR, 1 session)

Create small commits on `main` directly for:
- T0-6-cont shared extractor migration
- T0-7 ADR debt
- T0-8 retention tie-breaking

Rationale: these are pure fixes with no architectural dependencies. They land on main cleanly and reduce the merge surface.

### Stage 2 — Event bus and archive (1 PR)

Merge from the branch:
- `src/archive.rs`
- `[archive]` config section
- `scripts/archive_stats.py`
- Archive ADR

Rationale: archive is a bus subscriber with no registry dependency. It's self-contained.

### Stage 3 — Registry + lifecycle refactor (1 PR, careful review)

Merge:
- `src/registry.rs` — `ApiRegistry`
- `main.rs` subscriber manager refactor
- `proto` RPC additions for device CRUD
- `src/api.rs` wiring

Rationale: this is the architectural change — subscribers go from "started once at boot from TOML" to "lifecycle-managed from registry events." It touches many files but is cohesive. Merge in one PR with a dedicated review and a rollback plan.

Pre-merge check: run `bonsai` with a `bonsai.toml` that has no targets. After merge, bonsai must still start cleanly. This verifies the registry handles empty state.

### Stage 4 — Discovery + path profiles (1 PR)

Merge:
- `src/discovery.rs`
- `config/path_profiles/*.yaml`
- `DiscoverDevice` RPC
- Fallback logic for missing profiles

Rationale: discovery depends on registry (for later writing the discovered device) but is independently testable without it.

### Stage 5 — Subscription verification (1 PR)

Merge:
- `src/subscription_status.rs`
- Graph schema additions for `SubscriptionStatus` and `HAS_SUBSCRIPTION_STATUS`
- Subscriber wiring to publish paths

Rationale: builds on registry + subscribers.

### Stage 6 — HTTP facade and UI onboarding (1 PR)

Merge:
- `src/http_server.rs` onboarding endpoints
- `ui/src/lib/Onboarding.svelte` and App.svelte integration

**Note**: merge this as-is; the UI is known to be incomplete. The full wizard (T1-3) is a v3 item. Merging now makes the existing UI available; the better UI replaces it later. Do not block the merge waiting for the wizard.

### Stage 7 — Distributed runtime seam (1 PR)

Merge:
- Runtime modes in `config.rs`
- `src/ingest.rs`
- `TelemetryIngest` RPC
- `main.rs` collector/core mode branching

Rationale: last because it's the most experimental slice. Merging it last limits blast radius if it turns out to have issues.

### Stage 8 — Python SDK, scripts, chaos plans, docs (1 PR)

Merge:
- `python/bonsai_sdk/training_readiness.py`
- `RemediationTrustMark` backfill
- `scripts/check_training_readiness.py`
- `scripts/validate_playbooks.py`
- `chaos_plans/`
- `scripts/chaos_runner.py` changes
- `docs/DEVELOPMENT.md`
- `BACKLOG_V2_EXECUTION_LOG.md`
- ADRs that haven't been picked up in earlier stages

Rationale: non-Rust changes are lower risk, merge last.

### After all stages

- Delete the branch `codex-backlog-priority-v2`
- Tag `main` as `v0.3.0` (or whatever versioning you use)
- Write a brief release note summarising what landed

### If a stage fails review or tests

- Do not cascade — hold that stage, carry on with the next independent one if possible
- Document the failure reason in the execution log
- Create a follow-up ticket with concrete repro

---

## <a id="guardrails"></a>Guardrails — Binding Through v3 and Beyond

These are non-negotiable. Add to CLAUDE.md if they aren't already there.

### Architectural invariants

- gNMI only. No SNMP, no NETCONF. Ever.
- tokio only for async Rust. No async-std, no smol.
- Credentials never leave the Rust process. Python holds aliases or env var names, not values.
- No Kubernetes in v0.x. Two-machine collector-core is fine; orchestration is not.
- No fifth vendor until the four vendor families work vendor-neutrally end-to-end.
- Every non-trivial decision gets an ADR in `DECISIONS.md` *at commit time*, not in a batch later.

### Hot-path determinism

- The detect-heal loop does not call an LLM. Ever.
- Detection latency target stays sub-second.
- If the Anthropic API or any other SaaS is unreachable, bonsai still detects and heals.
- The investigation agent (T4) sits outside this path and is triggered only by explicit conditions.

### UI discipline

- Phase 6 UI is view-plus-onboarding. No arbitrary configuration push, no auth/RBAC, no admin panels.
- Onboarding wizard is the operational UI. Dashboards, reports, maps — those are future, optional, and never block onboarding work.

### ML discipline

- Tabular ML (Model A/C) remains the production path until the GNN (T3-B) has an honest evaluation showing improvement on real data.
- GraphML work (T3-A and T3-B) does not eat the onboarding or distributed work.
- GNN requires months of data before retraining — the operational infrastructure feeds it; the ML team does not get to accelerate by synthesising fake data.

### Scope discipline

- No auth/RBAC. The gRPC API is trusting; deployment is responsible for network perimeter.
- No multi-tenant graph.
- No production-grade HA (leader election, replication). Single-host core is fine for v0.x.
- No universal vendor playbook library — Codex catalog grows organically with real lab evidence.
- No replacement for Nautobot/NetBox — bonsai's graph is operational state, not source of truth for intent.

### Anti-patterns to reject

- "Let's use SNMP just for this one device" — no.
- "The UI could grow into a product" — no, it's a demo view.
- "We could ship to Kubernetes now" — no, ship a binary first.
- "A fifth vendor would be cool" — no, make the four work first.
- "Let's use the agent for detection" — no, investigation only.
- "Let's skip ADRs for the small stuff" — no, write them at commit time.

---

## What v3 Explicitly Excludes

For scope discipline, do not start:

- Auth/RBAC work of any kind
- Multi-tenancy in the graph schema
- Production HA for the core process (leader election, graph replication)
- Universal vendor playbook coverage outside the current four vendor families
- A competing source-of-truth product (Nautobot/NetBox equivalent)
- Online/continual ML learning
- Multi-GPU GNN training
- A fifth vendor before the existing four are vendor-neutral end-to-end
- Agent-driven autonomous remediation without human approval

---

*Version 3.0 — authored after reviewing the `codex-backlog-priority-v2` branch on 2026-04-21. Reflects the genuine progress that landed, the specific onboarding gaps that remain, the two-stage GraphML evolution (Path A embeddings → Path B GNN), and the investigation agent architecture. Merge plan is staged across eight PRs for bisectability. Guardrails keep ML work from starving operational work.*

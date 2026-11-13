# BN-015-U2 — Preserve site hierarchy on device save

**Date:** 13 November 2026 · **Branch:** `fix/bn015-u2-preserve-site-hierarchy` · **Base:** `main` @ `743d38d` (post PR #12 merge)

## Confirmed defect

`save_managed_device` (`src/http_server/managed_devices.rs:320-325`)
calls `GraphStore::sync_sites_from_targets` after every device
add/update (also at boot via `server_startup.rs:1024` and on the
device gRPC API paths). For each target with a non-empty `site`
string the sync built:

```rust
SiteRecord {
    id: site_id_from_name(site_name),   // name-derived slug
    parent_id: String::new(),           // ROOT — parent discarded
    kind: "unknown",
    lat: 0.0, lon: 0.0,
    metadata_json: "{}",
    environment_id: String::new(),
}
upsert_site_record(&conn, &site, now)?;   // ON MATCH SET overwrites ALL
link_device_to_site(&conn, &primary, &site.id)?;
```

`upsert_site_record` (`MERGE (s:Site {id}) … ON MATCH SET s.name,
s.parent_id, s.kind, s.lat, s.lon, s.metadata_json`) then:

1. overwrote `parent_id`/`kind`/coordinates/metadata/environment on
   ANY existing Site whose id collided with the name slug, and
2. deleted every `PARENT_OF` edge into that Site before re-adding at
   most one — flattening Region→Datacenter→Building→Rack chains;
3. created name-derived duplicate Site nodes when no slug match
   existed, merging same-named sites in different branches.

Device-to-site association and site-hierarchy administration were
not separate operations.

## Fix

`sync_sites_from_targets` no longer writes Site records. Site-claim
resolution is deterministic and shared (`resolve_site_claim` /
`SiteClaimResolution` in `src/graph/mod.rs`):

1. UNIQUE exact display-name match → link to that Site (covers
   slug-id sites, explicit-id imports and admin-created sites
   uniformly — a free-text claim is NEVER treated as a stable id).
2. Ambiguous display name (>1 Site shares it) → `Unresolved`, even
   when one candidate owns the name-derived slug: a free-text claim
   cannot pick a branch.
3. No name match → inspect the name-derived id
   (`site_id_from_name`): if a Site already owns that id under a
   DIFFERENT display name the claim is a slug collision →
   `Unresolved` (never link the unrelated site, never create a
   duplicate under a taken id).
4. Otherwise → `NewSite`: create the minimal name-derived record
   create-only (`ON CREATE SET … ON MATCH SET s.updated_at` — the
   same pattern `enrichment/netbox.rs::backfill_located_at` used).

`ensure_target_site` wraps the resolver and performs only the
`NewSite` creation. `upsert_site`/`upsert_site_record`/
`validate_site_hierarchy` remain the admin path with full overwrite
authority — unchanged.

### LOCATED_AT claim lifecycle (review correction B2)

`sync_sites_from_targets` now distinguishes three claim states per
device (`TargetConfig.site: Option<String>`):

- `None` (field omitted in a partial update) → existing `LOCATED_AT`
  association preserved untouched, regardless of which writer
  established it.
- `Some("")` (explicit clear, now expressible via
  `ManagedDeviceRequest.site: Option<String>` — absent/`null` inherits
  the stored claim, `""` clears) → transactional removal of the edge
  AND the mirrored `d.site` claim so no writer can resurrect it.
- `Some(name)` → resolved: transactional replace; ambiguous or
  slug-colliding: the stale edge is dropped (explicit unknown) rather
  than left presenting the previous site as current. The raw claim
  string stays mirrored on `d.site`.

All relationship mutations run inside `BEGIN TRANSACTION … COMMIT`
(`update_device_site`) so a failure cannot leave a half-applied
association.

`enrichment/netbox.rs::backfill_located_at` now resolves through the
same `resolve_site_claim` contract instead of blind
`site_id_from_name` — it can no longer fabricate an edge the device
sync would refuse (previously it would also create slug-duplicate
Site nodes for uniquely-named existing sites).

## Before / after evidence

Before-fix reproduction: the new tests were applied on top of the
pre-fix `sync_sites_from_targets` (restored in a scratch worktree at
`743d38d`) and run against a cold target dir:

```
cargo test -p bonsai --lib graph::tests::site_sync --offline
→ 1 passed; 5 failed   (38m26s cold build, then 5.83s run)
```

| Test | Before (pre-fix sync) | After (fix) |
|---|---|---|
| `site_sync_preserves_existing_hierarchy` | FAIL — device save created a name-derived duplicate `tower-a` Site alongside `bldg-tower-a` (and on slug-id collisions the blank record's `ON MATCH SET` overwrites parent/kind/coords/metadata and deletes `PARENT_OF` edges) | PASS — Site rows + parent edges byte-identical |
| `site_sync_move_changes_only_device_association` | FAIL — linked to the `tower-a` duplicate | PASS — only `LOCATED_AT` moves |
| `site_sync_ambiguous_display_name_creates_no_duplicate` | FAIL — 5 Sites (name-derived `dc-1` duplicate merged the branches) | PASS — 4 Sites, no guessed link |
| `site_sync_idempotent_and_survives_restart` | FAIL — duplicate `tower-a` persisted across reopen | PASS |
| `site_sync_preserves_imported_site_record` | FAIL — `imported-dc` duplicate created beside importer-owned `dc-imported` | PASS — resolves by unique name to `dc-imported`, metadata intact |
| `site_sync_preserves_slug_id_site_record` | (added with fix — covers the literal ON MATCH clobber path) | PASS |
| `site_sync_creates_site_and_located_at_edge` (baseline) | PASS in before-run (auto-create path was already correct; the earlier baseline failure was a stale test querying port-full `dut:57400` instead of canonical `dut` — test corrected) | PASS |

After-fix verification (B1/B2 review corrections included):

```
cargo check -p bonsai --lib --offline          → clean (~25s)
cargo check -p bonsai --tests --offline        → clean (~31s)
cargo test -p bonsai --lib --offline site      → 18/18 PASS (~13s run)
cargo test -p bonsai --lib --offline enrichment::netbox → 8/8 PASS
cargo test -p bonsai --lib --offline registry  → 18 pass / 1 fail
     (api_registry_persists_and_emits_changes — documented BN-019-B6
      baseline "registry persistence" failure, unrelated code path)
```

New/updated BN-015-U2 tests:

| Test | Coverage |
|---|---|
| `site_sync_preserves_existing_hierarchy` | unique-name resolution preserves Region→DC→Building→Rack + metadata byte-for-byte |
| `site_sync_move_changes_only_device_association` | resolved move replaces only that device's edge |
| `site_sync_ambiguous_display_name_creates_no_duplicate` | ambiguous name → no edge, no duplicate |
| `site_sync_preserves_slug_id_site_record` | slug-id Site record preserved; ambiguous "Tower A" (bldg-tower-a + tower-a) now correctly yields NO edge — arbitrary slug-owner selection removed |
| `site_sync_idempotent_and_survives_restart` | idempotence + reopen persistence |
| `site_sync_preserves_imported_site_record` | explicit-id imported Site resolves by unique name |
| `site_sync_slug_collision_with_unrelated_site` | two names → same slug: "Foo Bar" vs existing "Foo-Bar" → Unresolved, no edge, no Site |
| `site_sync_ambiguous_name_with_slug_owner_stays_unresolved` | two Sites share "DC-1", one owns slug → Unresolved |
| `site_sync_stale_edge_lifecycle` | valid→ambiguous (edge dropped, claim kept)→omitted (preserved)→corrected (re-resolves)→explicit clear (edge + d.site blanked)→idempotent→restart |
| `site_sync_omitted_site_preserves_existing_association` | partial update without site field keeps LOCATED_AT |
| `http_server::managed_devices::tests::managed_device_api_site_claim_lifecycle` | REAL API path: axum route → serde → save_managed_device → registry → graph sync; covers create/omit/ambiguous/corrected/explicit-clear |

## Scope notes / residuals

- Free-text `device.site` claims can still be ambiguous when two
  branches share a display name — now explicit (no link) rather than
  silently wrong; canonical stable `site_id` selection/migration is
  BN-015-U3.
- Explicit clear is expressible on the HTTP managed-device API
  (`"site": ""`) and on `TargetConfig.site = Some("")`. The device
  gRPC API (`target_from_managed_device`) maps empty proto `site` to
  `None` — proto3 non-optional string cannot distinguish unset from
  empty, so gRPC retains "omitted preserves" semantics only.
- `d.site` device property, `site_summary_handler` name-mapping and
  `/api/topology` name-keyed sites are untouched (U3 scope).
- No schema change, no new topology features, no UI change.

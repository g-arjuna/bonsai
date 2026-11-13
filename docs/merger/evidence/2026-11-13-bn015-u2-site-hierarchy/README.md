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

`sync_sites_from_targets` no longer writes Site records. New
`ensure_target_site` resolves the free-text claim for `LOCATED_AT`
linking only:

1. `site_id_exists(site_id_from_name(name))` — historical convention,
   compatible with NetBox `site_id_from_name` derivation;
2. else `site_ids_named(name)` — exactly one Site with that display
   name links by its explicit id (admin/imported records);
3. else create the minimal name-derived record **create-only**
   (`ON CREATE SET … ON MATCH SET s.updated_at` — the same pattern
   `enrichment/netbox.rs::backfill_located_at` already used);
4. ambiguous display name (>1 match, no slug hit) → `None`: no
   duplicate created, no guessed link — explicit unknown.

`upsert_site`/`upsert_site_record`/`validate_site_hierarchy` remain
the admin path with full overwrite authority — unchanged.

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

After-fix verification:

```
cargo test -p bonsai --lib graph::tests::site   → 9/9 PASS
cargo test -p bonsai --lib                     → 305 pass / 4 fail
```

Remaining 4 failures are the documented BN-019-B6 baseline set
(`archive::hourly_writer_limits…`, `subscription_status_write…`,
`api_registry_persists…`, `service_pattern_matching`) — `site_sync`
is no longer among them (baseline count 5 → 4).

## Scope notes / residuals

- Free-text `device.site` claims can still be ambiguous when two
  branches share a display name — now explicit (no link) rather than
  silently wrong; canonical stable `site_id` selection/migration is
  BN-015-U3.
- `d.site` device property, `site_summary_handler` name-mapping and
  `/api/topology` name-keyed sites are untouched (U3 scope).
- No schema change, no new topology features, no UI change.

# BN-015-U1 — NetBox inventory-only onboarding

Branch: `fix/bn015-u1-netbox-inventory-onboarding` (from main @ 4d9172c, post-PR-#14).

## Confirmed before-defect

`ui/src/lib/Onboarding.svelte::nbImportSelected` posted
`selected_paths: []` to `/api/onboarding/devices/with_paths`
(`managed_devices.rs` rejects empty paths → every import failed), while
silently binding `credentials[0]?.alias` and defaulting unknown roles to
`'leaf'`. Import was indistinguishable from telemetry activation.

## P0 hardening completed in this PR (review follow-up)

- **N1 stable external identity**: NetBox preview derives
  `netbox:{scheme}:{host}:{port}:device:{id}` from the connection URL's
  scheme+authority only — credentials, userinfo, path, query and tokens
  never participate (`netbox_instance_identity`, unit-tested incl. IPv6,
  default ports, malformed input). Two NetBox deployments sharing device
  id 123 produce distinct identities. Uniqueness is enforced at the
  persistence boundary by partial unique index
  `idx_devices_external_id` (schema v7) — concurrent imports cannot
  create duplicates; ambiguous pre-existing rows fail migration closed
  (`user_version` stays at 6). Arbitrary smallest-address first-match is
  gone: `find_by_external_id` fails closed on ambiguity, and
  `find_by_canonical_address`/`canonical_device_key` (shared with
  `strip_port`/graph identity) reject `host` vs `host:port` and IPv6
  variants the address-keyed graph cannot distinguish.
- **N2 truthful persistence**: registry mutations are transactional —
  in-memory state, SQLite write and audit commit together or roll back;
  `persist_state`/`upsert_device` errors propagate (no more log-and-
  swallow). Change notifications publish only after durable commit.
  Graph projection (`sync_sites_from_targets`) is a separate step:
  response carries `graph_sync: synced|pending` + `graph_sync_error`;
  unchanged re-imports re-run projection so retries reconcile missing
  `LOCATED_AT` edges (verified by test).
- **N3 lifecycle boundary**: imports create `enabled=false`,
  `inventory_only=true`, credential-free, path-free records.
  `spawn_subscriber`, `spawn_subscriber_with_creds`,
  `seed_subscription_plan`, `merge_seed_target` and collector
  assignment all skip `inventory_only` targets even if `enabled` is
  somehow set. Activation requires an explicit validated collection
  plan; `reject_inventory_enabled` blocks create-time
  inventory-only+enabled. `inventory_only` clears only via the
  explicit enable path and `external_id` survives activation.
- **N4 conservative metadata**: re-import refreshes importer-owned
  declared fields (`hostname/site/role/vendor` + `inventory_platform/
  status/tenant`) on inventory assets only. On configured devices,
  operator fields (credentials, paths, collector, lifecycle, selected
  paths) are never touched; declared-metadata drift is reported as a
  per-row `conflict`, not silently applied.
- **N5 browser identity**: `Onboarding.svelte` selects/deselects by
  `external_id` (address fallback only for legacy rows), disables
  ineligible candidates, matches results per external object, and shows
  the `graph_sync: pending` state distinctly from import success.
- **N6 migration correctness**: v6 `ALTER`s distinguish
  duplicate-column (partial prior run → verify + continue) from genuine
  failure (abort before `user_version` advances); every version verifies
  expected columns via `PRAGMA table_info` before reporting success.
  New tests cover fresh→v7, real v5→v7 upgrade with row/path
  round-trip, partial-v6 tolerance, duplicate-external-id fail-closed
  (version stays 6), persistence-boundary uniqueness, and malformed
  JSON recovery semantics.

## Verified

- `cargo test -p bonsai --lib` — 355 pass / 3 fail; the 3 failures are
  the documented BN-019-B6 baseline (`archive::hourly_writer...`,
  `graph::subscription_status_write_preserves_device_metadata`,
  `service_discovery::service_pattern_matching`). The previously
  failing `api_registry_persists_and_emits_changes` baseline now passes
  (isolated tempdir registry state).
- `cargo test -p bonsai --lib http_server::` — 24/24 incl. authenticated-router
  RBAC (401 unauthenticated / 403 viewer / 200 operator), instance
  identity unit tests, all inventory import scenarios.
- `cargo test -p bonsai --lib sqlite_store` — 6/6 migration tests.
- `cargo test -p bonsai --lib registry` — 25/25.
- `cargo test --test bn015_u1_inventory_browser` — Playwright/Chromium
  acceptance against real `router_from_state` + mocked paginated NetBox:
  two-page preview, stable-identity selection of two devices sharing one
  IP, ineligible no-IP row disabled, per-object results
  (imported/inventory-only + independent conflict), inventory-only badge
  on `/devices`, no credential assignment, external_id + inventory
  metadata round-trip via API.
- `npm run build` (ui) — clean, 6.37s.

## Limitations

- Missing-primary-IP candidates are surfaced as ineligible in preview;
  address-independent asset declaration remains BN-015-U4.
- Multi-connector provenance matrix remains BN-015-U6.
- Per-field authority reconciliation beyond declared-vs-operational
  split remains BN-015-U6.

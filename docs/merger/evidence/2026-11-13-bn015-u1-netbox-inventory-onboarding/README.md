# BN-015-U1 — NetBox inventory-only onboarding

Branch: `fix/bn015-u1-netbox-inventory-onboarding` (from main @ 4d9172c, post-PR-#14).

## Confirmed before-defect

`ui/src/lib/Onboarding.svelte::nbImportSelected` posted
`selected_paths: []` to `/api/onboarding/devices/with_paths`
(`managed_devices.rs` rejects empty paths → every import failed), while
silently binding `credentials[0]?.alias` and defaulting unknown roles to
`'leaf'`. Import was indistinguishable from telemetry activation.

## After

- `POST /api/onboarding/inventory/import` — narrow, authenticated
  (inherits `required_role` → Operator for POST; Viewer gets 403).
  Per-row outcomes: imported / already_imported / updated / conflict /
  invalid / failed.
- Inventory-only representation: `TargetConfig.enabled=false`,
  `inventory_only=true`, `external_id="netbox:device:<id>"`, no
  credential alias, no selected paths. `spawn_subscriber` skips
  disabled targets at startup and on registry updates — no gNMI
  connection, no default profile, no polling.
- Conflicts never overwrite: same external id + different address →
  conflict; any external id claiming a registered address → conflict;
  existing manually-onboarded devices preserved. Same object re-import
  refreshes declared metadata only.
- Activation remains the existing explicit path: managed-device update
  with enabled=true + collection plan; `inventory_only` clears only on
  enable.
- Preview (`/api/enrichment/netbox/import`) now returns NetBox object
  id, tenant, `eligible`/`skip_reason`; no-IP or id-less devices are
  shown ineligible, not dropped.
- sqlite schema v6 persists external_id/inventory_only/selected_paths/
  extra_ips (previously selected_paths+extra_ips were lost on
  sqlite-backed reload — restart persistence verified by test).

## Verified

- `cargo check -p bonsai --lib` clean.
- `cargo test -p bonsai --lib http_server::managed_devices` — 3/3:
  declared-only creation + per-row idempotence/conflicts/invalid,
  existing-active-device preservation, restart persistence.
- `npm run build` (ui) clean — 6.34s.

## Limitations

- Full mocked-NetBox Playwright journey (Phase J10) not yet executed —
  API-level acceptance covered by router tests; browser pass pending.
- platform/status/tenant accepted by the import API but not yet
  persisted on the device record (BN-015-U3 metadata).
- Missing-primary-IP candidates are surfaced as ineligible in preview;
  address-independent asset declaration remains BN-015-U4.
- Multi-connector provenance matrix remains BN-015-U6.

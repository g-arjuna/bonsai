# M6F-C Arista YANG repository audit — 2026-11-23

Source: `https://github.com/aristanetworks/yang` @
`f7fc5de3d80aa32de403f067b625dd8db633ca34` (the pin already in
`config/yang_sources.toml`), tree `EOS-4.37.0F/`.
Machine-readable detail: `audit.json` (includes pathreport.csv and
LICENSE.txt SHA256). Per-module inventory:
`module-inventory.json` — all 303 device-advertised models joined
against repo module name/namespace/latest-revision, classified by
source dir (`release_oc` / `vendored_upstream` / `experimental`).
Revision states: 180 `consistent` (single latest revision in repo),
106 `unresolved_no_revision_stmt` (mostly Arista augment/experimental
modules that declare no `revision` statement — YANG-legal but the
catalogue compiler records them revision-less), 17 `absent`
(IETF/IEEE modules resolved by bonsai's `ietf-yang` source,
cloud-platform `openconfig-hercules-*`, device-internal names).
Note: gNMI `ModelData.version` (e.g. `3.8.1`) is NOT a YANG revision
and is preserved only as `caps_version`.

## Repository layout (verified)

- `EOS-4.37.0F/release/openconfig/models/<feature>/` — 56 files:
  `arista-<feature>-augments.yang` + `arista-<feature>-deviations.yang`
  across 23 feature directories. This is Arista's *supported*
  OpenConfig surface: augmentations and `deviate` statements against
  upstream OpenConfig modules. Imports do not pin upstream revisions.
- `EOS-4.37.0F/openconfig/public/release/models/` — vendored upstream
  OpenConfig snapshot; NOT imported by the current manifest subdir
  (`release/openconfig` only). Bonsai's separate `openconfig-public`
  pin (v5.11.0) supplies upstream modules; revision compatibility
  between the two must be verified at compile time.
- `EOS-4.37.0F/experimental/eos/models/` — 123 files, ~101 modules the
  cEOS 4.37.0F device actually advertises. Experimental = not
  release-supported; must NOT enter the authoritative catalogue by
  default.
- `EOS-4.37.0F/pathreport.csv` — 6,704 vendor-documented path rows.

## Capabilities comparison (cEOS 4.37.0F, evidence
`2026-11-22-m6f-ceos-probe`)

303 advertised models → 286 resolve inside the pinned repo:
56 via `release/openconfig`, 129 via the vendored upstream snapshot,
101 via `experimental/eos`. The 17 unmatched are IETF/IEEE standard
modules (resolved by bonsai's `ietf-yang` source instead),
`openconfig-hercules-*` cloud-platform models absent from this
snapshot, and device-internal/versioned names
(`arista-segmentation.v1`, `synth-dev-mod-*`).

## pathreport.csv semantics

`PATH,NODE,ACCESS,BUILT-IN-TYPE,DERIVED-TYPE,AUGMENT,CONSTRAINTS,NOTES`
— 3,760 `read`, 1,381 `read+write`, 1,317 `not-supported`, 246 blank.
Notes include `gNMI-only access` (2,757 rows) and
`Not-supported deviation.` (1,110 rows). The observed live path
`interfaces/interface/state/counters/in-octets` is listed `read`.

## Conclusions for Stage C/D

1. The current manifest subdir (`release/openconfig`) is the correct
   supported closure — augmentations/deviations only; upstream comes
   from the existing openconfig pin. No manifest change needed for
   correctness; adding `experimental/` or the vendored upstream dir
   would be wrong.
2. `pathreport.csv` is usable as *advisory, versioned* provenance
   (source commit + file hash + EOS version) — never as device
   authority. cEOS exposes no YANG Library, so schema-bound
   applicability remains fail-closed by design.
3. Safe cEOS acquisition can only be explicit/selective: operator
   chooses a documented path (e.g. interface counters, `read` per
   pathreport), bounded subscription, admin-approved — with evidence
   classes kept distinct (`vendor_release_schema_pinned` vs
   `device_effective_schema_verified` etc.).

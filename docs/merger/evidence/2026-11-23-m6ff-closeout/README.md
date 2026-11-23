# M6F-F closeout evidence — 2026-11-23

Gate-tested production closeout for the enterprise multi-version YANG
architecture. Deployed binary: `ci/bonsai` built from `ecab60d`
(`ecab60dd704e`, branch `m6ff/closeout`; contains PR #71 scope-load
fix `7464390` and the revision-dedup-notice fix `ecab60d`).
SHA-256 recorded in `binary.sha256`; per-run provenance in each
`*/api/*l4_provenance*` artifact. Runtime image label
`bn021d-bonsai:local`; the mounted binary is the authority.

## Runs

| Dir | Scope | Result |
|---|---|---|
| `srl/` | SR Linux 26.3.1 scoped drive | PASS — full chain incl. telemetry + revoke stages |
| `srl-revoke/` | SR Linux revoke/withdrawal drive | PASS — all 7 withdrawal predicates (collector ack, no continued collection, old-session rejection) |
| `xrd/` `xrd-r2/` `xrd-r3/` | XRd attempts | FAIL→partial (pre-fix binaries / dedup-diagnostic defect; superseded) |
| `xrd-r4/` | XRd 24.4.2 production drive on `ecab60d` | PASS — full chain, clean withdrawal |
| `coexistence.json` | Multi-version isolation summary | PASS — see below |
| `stages.json`, `binary.sha256` | Run metadata | — |

## Key artifacts

- `srl/api/00-summary.json` — SR Linux binary SHA + stage results.
- `xrd-r4/api/` — XRd stage evidence: `catalogue_compile_scoped`
  (`ysc-9d38b26e36ba3474ee1a5be37f3f9a95`, complete, 9 modules,
  120 paths, not globally activated), `schema_selection`,
  `applicability_snapshot` (`applicable`, content-id
  `IOS-XR 7.5.1`, 120 paths), `plan_preview` permitted,
  `plan_activate` (rev 2), `telemetry_observed` (advancing),
  `plan_revoke`, `withdrawal_verified` (all predicates),
  `l4_provenance` (`ecab60dd704e`, clean tree).
- `srl-revoke/api/` — authoritative SR Linux withdrawal evidence.

## Multi-version coexistence (`coexistence.json`)

One core, two devices, two distinct selected schemas:

- SR Linux `172.100.102.11:57400` → `ysc-d7254b6e840b0f0041c4b2458bb509a1`
  (11 modules, 419 paths).
- XRd `172.100.103.21:9339` → `ysc-9d38b26e36ba3474ee1a5be37f3f9a95`
  (9 modules, 120 paths, exact device-reported revision pins).

Proven live: distinct per-device `effective_schema_id`; SR Linux
telemetry stayed `observed` and advancing through the entire XRd
compile/selection/discovery/preview/activate/revoke sequence; core +
collector restart preserved both selections and the SR Linux
approval from durable state with telemetry resuming without
re-approval; XRd operations never revoked the SR Linux binding.

Known limitation (truthful): XRd withdrawal sub-checks during
coexistence were confounded by the legitimate SR Linux subscription
present on the shared collector (`xrd-r3`); `xrd-r4` on a quiet
collector is the authoritative XRd withdrawal evidence, and
`srl-revoke` is the authoritative SR Linux one.

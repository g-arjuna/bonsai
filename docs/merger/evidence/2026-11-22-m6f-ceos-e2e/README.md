# M6F-C cEOS 4.37.0F — explicit selective acquisition slice: PASS

Date: 2026-11-22 (UTC). Branch `fix/m6fc-coverage`.
Design: `docs/M6F_CEOS_EXPLICIT_ACQUISITION.md`.

## Device / image

- `ceos:4.37.0F`, image ID `sha256:0c2645f9ce3c376b61420a5c068e307ba32b48a2c5cb2f86b81f89adf806c1f0`
  (rootfs archive `cEOS64-lab-4.37.0F.tar.xz`; NOT docker-save —
  `docker import` provenance, see `../2026-11-22-m6f-ceos/`).
- NOS: Arista cEOSLab 4.37.0F-51574443.4370F.
- containerlab `arista_ceos`, mgmt `172.100.104.11:6030`,
  plaintext lab channel (`tls: insecure` → explicit `--tls-mode
  plaintext`; production requires EOS `ssl profile` + tls_verify).
- Probe re-verified on this deploy: capabilities PASS, subscribe-once
  PASS, both YANG Library variants UNSUPPORTED
  (`../2026-11-22-m6f-ceos/`).

## Path taken (all production API/collector, no shortcuts)

1. `login,provision,wait_collector` stages via `m6f-bonsai-e2e.sh
   --action drive` → device registered `acquisition=disabled`,
   credential alias `m6f-ceos`, assigned to `m6f-col-1`
   (`api/00-summary.json` … `api/07-assignment-status.json`).
2. `POST /api/devices/172.100.104.11:6030/selected-paths` — one
   bounded path, `acquisition` flipped to `explicit`,
   `schema_binding: null` (deliberate: no schema authority exists
   for EOS).
3. Observed: status `observed`, `first/last_observed_at_ns`
   advancing across ≥2 SAMPLE intervals; interface projection
   `Management0 in_octets` advanced 21259 → 29994 with live
   `updated_at_ns` (`api/60-explicit-slice-evidence.json`).
4. Withdrawal: `selected-paths: []` → collector resync → both plan
   generations `superseded`, collector `subscription_count=0`,
   `last_observed_at_ns` frozen, projection timestamp frozen
   (`api/50-post-revoke-device.json`, `api/51-post-revoke-assignment.json`).

## Finding: gNMI origin echo (production-relevant)

First attempt used `origin: "openconfig"` (matching the probe's
subscribe origin). Telemetry streamed and projected, but coverage
stayed `subscribed_but_silent` forever: EOS does not populate
`Path.origin` in notifications, and `observation_covers`
(`src/gnmi_path.rs:131`) requires exact origin equality when the
expectation origin is non-empty. Re-submitting the identical path
with `origin: ""` produced `observed` coverage within one sample
interval. The empty-origin subscribe still selected the correct
model set on EOS.

Recorded as backlog item: expectation-origin vs notification-origin
echo semantics differ across vendors; operators must currently
leave `origin` empty for cEOS explicit plans. Whether
non-echoing origins should be tolerated is a schema-authority
policy decision — NOT relaxed here.

## Explicitly NOT claimed

- No schema-bound applicability (EOS has no YANG Library —
  fail-closed by design).
- No catalogue/compiler support claim for Arista models.
- No TLS-verified channel (lab plaintext only).
- `pathreport.csv` used as advisory path-selection provenance only.

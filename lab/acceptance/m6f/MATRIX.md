# M6F vendor compatibility matrix

Statuses: PASS / FAIL / UNSUPPORTED / NOT_EXECUTED.
A missing image or unexecuted check is NOT_EXECUTED — never PASS.
Evidence: `docs/merger/evidence/2026-11-21-m6f-srlinux/` (probe slice)
and `docs/merger/evidence/2026-11-22-m6fb-srlinux-r6/` +
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r7/` (production
Core/collector slice — full path PASS, run twice).

> **Correction 2026-11-22 (M6F-B review):** the original matrix marked
> SAMPLE and ON_CHANGE as PASS, but the only captured subscribe evidence
> is a single `mode=once` initial sync — that does not demonstrate
> sustained SAMPLE or ON_CHANGE streaming. Those rows are corrected to
> NOT_EXECUTED. The Capabilities, YANG Library, JSON_IETF, initial-sync,
> list-key and TLS findings are unchanged: they are backed by the
> committed capture. Historical evidence files were not modified.

| Capability | SR Linux | XRd | Junos (cJunos) | cEOS |
|---|---|---|---|---|
| Capabilities | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| YANG Library RFC 8525 | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Legacy YANG Library | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| JSON_IETF encoding | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| SAMPLE mode | PASS (production collector, sustained) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| ON_CHANGE mode | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| List-key handling | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Initial sync | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Delete semantics | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| TLS authentication | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Remote collector discovery | PASS (Core-issued job, snapshot 8) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Exact plan activation | PASS (schema-bound rev 4/5) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Coverage projection | PASS (`observed`, advancing timestamps) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Revocation | PASS (withdrawal verified) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Production Core/collector e2e | PASS (`docs/merger/evidence/2026-11-22-m6fb-srlinux-r6`, `-r7`) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |

SR Linux notes (v26.3.1-410, gNMI 0.10.0): requires canonical
`ietf-yang-library` module name for YANG Library paths (rejects
`yang:`/`yang-library:` prefix aliases with INVALID_ARGUMENT; bare
container names are also accepted); rejects `Encoding.JSON`; supports
ASCII/PROTO/JSON_IETF/JSON_IETF_FROM_ROOT/PROTO_ANY/PROTO_JSON/
PROTO_BYTES/CLI_FLAT plus Nokia-proprietary encoding enum values.
List-key PASS is limited to key preservation observed in
`subscribe-once.json` update paths (`name=ethernet-1/1`) and in the
production run (`name=mgmt0`); sustained SAMPLE is now demonstrated
through the production Core/collector path (M6F-B, evidence
`2026-11-22-m6fb-srlinux-r6`/`-r7`): notifications report at
**container granularity** — SR Linux emits
`srl_nokia-interfaces:interface[name=mgmt0]/statistics` (module-prefixed
element names, JSON_IETF struct value) rather than per-leaf paths, so
the accepted schema-bound plan selects the applicable container path
`/interface[name=mgmt0]/statistics`; leaf-level expectations cannot be
covered by container-granular notifications. ON_CHANGE, deletes and
other list keys remain NOT_EXECUTED.
TLS PASS means verified TLS against the clab lab CA; the harness now
enforces the configured server name (previously the option was parsed
but ignored — see `tools/m6f_gnmi.py`).

XRd / Junos / cEOS: owner archives exist under `~/Desktop/clab-images/`
(cEOS64-lab-4.36.0F/4.37.0F, xrd-control-plane 24.4.2/25.1.1/7.11.2,
cJunosEvolved-26.2R1.7-EVO). Manifest `image_archive` fields are set;
the harness resolves the exact archive-declared image reference via
`tools/m6f_image.py` — never a global newest-image pick. Vendor rows
stay NOT_EXECUTED until each is actually deployed and probed.

Update this matrix only from captured evidence in
`docs/merger/evidence/*-m6f-*/`. Mock-gNMI suites and inferred
compatibility do not count.

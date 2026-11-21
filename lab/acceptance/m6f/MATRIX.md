# M6F vendor compatibility matrix

Statuses: PASS / FAIL / UNSUPPORTED / NOT_EXECUTED.
A missing image or unexecuted check is NOT_EXECUTED — never PASS.
Evidence: `docs/merger/evidence/2026-11-21-m6f-srlinux/`.

| Capability | SR Linux | XRd | Junos (cJunos) | cEOS |
|---|---|---|---|---|
| Capabilities | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| YANG Library RFC 8525 | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Legacy YANG Library | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| JSON_IETF encoding | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| SAMPLE mode | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| ON_CHANGE mode | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| List-key handling | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Initial sync | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Delete semantics | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| TLS authentication | PASS | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Remote collector discovery | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Exact plan activation | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Coverage projection | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Revocation | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |

SR Linux notes (v26.3.1-410, gNMI 0.10.0): requires canonical
`ietf-yang-library` module name (rejects `yang-library:` alias);
rejects `Encoding.JSON`; supports ASCII/PROTO/JSON_IETF/
JSON_IETF_FROM_ROOT/PROTO_ANY/PROTO_JSON/PROTO_BYTES/CLI_FLAT plus
Nokia-proprietary encoding enum values.

XRd / Junos / cEOS: owner images exist under `~/Desktop/clab-images/`
(cEOS64-lab-4.36.0F/4.37.0F, xrd-control-plane 24.4.2/25.1.1/7.11.2,
cJunosEvolved-26.2R1.7-EVO) but manifest `image`/`image_archive` fields
are unset pending operator import — all rows NOT_EXECUTED.

Update this matrix only from captured evidence in
`docs/merger/evidence/*-m6f-*/`. Mock-gNMI suites and inferred
compatibility do not count.

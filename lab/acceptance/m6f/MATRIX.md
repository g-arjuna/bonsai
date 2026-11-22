# M6F vendor compatibility matrix

Statuses: PASS / FAIL / UNSUPPORTED / NOT_EXECUTED / PARTIAL
(PARTIAL = some sub-checks executed and passed, others not).
A missing image or unexecuted check is NOT_EXECUTED — never PASS.
Evidence: `docs/merger/evidence/2026-11-21-m6f-srlinux/` (probe slice),
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r6/` +
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r7/` (production
Core/collector slice — full path PASS, run twice) and
`docs/merger/evidence/2026-11-22-m6f-ceos-probe/` (cEOS probe slice —
standalone probe only, NOT the production path).

> **Correction 2026-11-22 (M6F-B review):** the original matrix marked
> SAMPLE and ON_CHANGE as PASS, but the only captured subscribe evidence
> is a single `mode=once` initial sync — that does not demonstrate
> sustained SAMPLE or ON_CHANGE streaming. Those rows are corrected to
> NOT_EXECUTED. The Capabilities, YANG Library, JSON_IETF, initial-sync,
> list-key and TLS findings are unchanged: they are backed by the
> committed capture. Historical evidence files were not modified.

| Capability | SR Linux | XRd | Junos (cJunos) | cEOS |
|---|---|---|---|---|
| Capabilities | PASS | NOT_EXECUTED | NOT_EXECUTED | PASS (probe) |
| YANG Library RFC 8525 | PASS | NOT_EXECUTED | NOT_EXECUTED | UNSUPPORTED |
| Legacy YANG Library | PASS | NOT_EXECUTED | NOT_EXECUTED | UNSUPPORTED |
| JSON_IETF encoding | PASS | NOT_EXECUTED | NOT_EXECUTED | PASS (probe) |
| SAMPLE mode | PASS (production collector, sustained) | NOT_EXECUTED | NOT_EXECUTED | PASS (probe) |
| ON_CHANGE mode | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | PARTIAL (probe: initial sync only) |
| List-key handling | PASS | NOT_EXECUTED | NOT_EXECUTED | PASS (probe) |
| Initial sync | PASS | NOT_EXECUTED | NOT_EXECUTED | PASS (probe) |
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
`tools/m6f_image.py` — never a global newest-image pick.

cEOS notes (4.37.0F, probe evidence `2026-11-22-m6f-ceos-probe/`):
archive is a rootfs tar.xz imported via `docker import` (NOT
`docker load`); clab `arista_ceos` kind auto-enables gNMI/NETCONF/eAPI
with `admin:admin`. gNMI listens on **TCP/6030 plaintext**
(`management api gnmi`, SSL profile none — verified TLS NOT_EXECUTED).
gNMI `origin` selects the model set (`openconfig`, `arista` valid);
element names are unprefixed — opposite convention to SR Linux.
**YANG Library is UNSUPPORTED**: every RFC 8525/legacy Get variant is
rejected and neither `ietf-yang-library` nor `yang-library` appears in
the 303-model Capabilities list, so Core remote discovery cannot
obtain library evidence and the production schema-bound path is
blocked at applicability (fail-closed, NOT a PASS). SAMPLE and
list-key and initial-sync PASS are probe-level only; no production
Core/collector run exists for cEOS.

XRd blocker (2026-11-22): image `ios-xr/xrd-control-plane:24.4.2`
loaded deterministically (nested archive → `ios-xr/xrd-control-plane:
24.4.2`, id `sha256:b3bd8e8e…`) and clab `cisco_xrd` node deployed, but
XRd aborts at boot: "Not enough inotify instance resource" — host
`fs.inotify.max_user_instances=128`, XRd needs ~4000. Raising it needs
host sysctl (sudo); NOT_EXECUTED until the owner raises the limit.

Junos: `cJunosEvolved-26.2R1.7-EVO.tar.gz` is a docker-save archive
(`docker.io/library/cjunosevolved:26.2R1.7-EVO`) — loadable; deploy
not yet attempted.

Junos notes (26.2R1.7-EVO, evidence `2026-11-22-m6f-junos-probe/`):
image loaded deterministically and boots fully under clab
`juniper_cjunosevolved` (QEMU/KVM guest, ~10 min boot, Junos
26.2R1.7-EVO on `ptx10001-36mr`). However this cPTX image exposes NO
gNMI service: no `system services grpc`/`gnmi` stanza exists and no
57400 listener is present — gNMI rows remain NOT_EXECUTED because no
gNMI endpoint exists to probe (recorded as UNSUPPORTED in the probe
README, not a matrix PASS/FAIL). A different Junos platform image
(vMX/vSRX or another PTX train) is needed for gNMI acceptance.

Update this matrix only from captured evidence in
`docs/merger/evidence/*-m6f-*/`. Mock-gNMI suites and inferred
compatibility do not count.

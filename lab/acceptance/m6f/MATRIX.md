# M6F vendor compatibility matrix

Statuses: PASS / FAIL / UNSUPPORTED / NOT_EXECUTED / PARTIAL
(PARTIAL = some sub-checks executed and passed, others not).
A missing image or unexecuted check is NOT_EXECUTED — never PASS.
Evidence: `docs/merger/evidence/2026-11-21-m6f-srlinux/` (probe slice),
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r6/` +
`docs/merger/evidence/2026-11-22-m6fb-srlinux-r7/` (production
Core/collector slice — full path PASS, run twice),
`docs/merger/evidence/2026-11-23-m6fc-srlinux-r8/` (M6F-C merged-code
regression — full path PASS incl. schema-aware coverage + all seven
authoritative withdrawal checks, final-SHA-eligible provenance),
`docs/merger/evidence/2026-11-23-m6fc-srlinux-g1/` (Stage G resilience —
collector/Core restart, device disconnect/reconnect, post-recovery
revocation, all PASS) and
`docs/merger/evidence/2026-11-22-m6f-ceos-probe/` (cEOS probe slice —
standalone probe only, NOT the production path).

> **Correction 2026-11-22 (M6F-B review):** the original matrix marked
> SAMPLE and ON_CHANGE as PASS, but the only captured subscribe evidence
> is a single `mode=once` initial sync — that does not demonstrate
> sustained SAMPLE or ON_CHANGE streaming. Those rows are corrected to
> NOT_EXECUTED. The Capabilities, YANG Library, JSON_IETF, initial-sync,
> list-key and TLS findings are unchanged: they are backed by the
> committed capture. Historical evidence files were not modified.

| Capability | SR Linux | XRd | Junos (cJunos) | cEOS | IOS XE c9kv (remote DevNet) |
|---|---|---|---|---|---|
| Capabilities | PASS | PASS (probe) | NOT_EXECUTED | PASS (probe) | FAIL (probe 2026-11-22: TCP/9339 reachable, gRPC UNAVAILABLE connection-reset — sandbox backend inactive; see notes) |
| YANG Library RFC 8525 | PASS | PASS (probe) | NOT_EXECUTED | UNSUPPORTED | NOT_EXECUTED |
| Legacy YANG Library | PASS | PASS (probe) | NOT_EXECUTED | UNSUPPORTED | NOT_EXECUTED |
| JSON_IETF encoding | PASS | PASS (probe) | NOT_EXECUTED | PASS (probe) | NOT_EXECUTED |
| SAMPLE mode | PASS (production collector, sustained) | PASS (probe + production collector, sustained, M6F-F) | NOT_EXECUTED | PASS (probe + explicit production) | NOT_EXECUTED |
| ON_CHANGE mode | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | PARTIAL (probe: initial sync only) | NOT_EXECUTED |
| List-key handling | PASS | PASS (probe) | NOT_EXECUTED | PASS (probe) | NOT_EXECUTED |
| Initial sync | PASS | PASS (probe) | NOT_EXECUTED | PASS (probe) | NOT_EXECUTED |
| Delete semantics | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| TLS authentication | PASS | NOT_EXECUTED (lab plaintext) | NOT_EXECUTED | NOT_EXECUTED (lab plaintext) | NOT_EXECUTED (verified-TLS harness ready; live probe failed before handshake completed) |
| Remote collector discovery | PASS (Core-issued job, snapshot 8) | PASS (job completed, snapshot 4/5) | NOT_EXECUTED | NOT_EXECUTED (no YANG Library) | NOT_EXECUTED |
| Exact plan activation | PASS (schema-bound rev 4/5) | PASS (schema-bound rev 1/2 on pinned XRd-revision bundle `ysc-9d38b26e…`, M6F-F `2026-11-23-m6ff-closeout`) | NOT_EXECUTED | NOT_EXECUTED (explicit mode, no schema binding) | NOT_EXECUTED |
| Coverage projection | PASS (`observed`, advancing timestamps) | PASS (`observed`, advancing timestamps, M6F-F) | NOT_EXECUTED | PASS (explicit: `observed`, advancing) | NOT_EXECUTED |
| Revocation | PASS (withdrawal verified) | PASS (withdrawal verified all 7 predicates, M6F-F xrd-r4) | NOT_EXECUTED | PASS (explicit empty selection, collector subs=0) | NOT_EXECUTED |
| Resilience (restart/disconnect/revoke) | PASS (`2026-11-23-m6fc-srlinux-g1`) | PARTIAL (core/collector restart mid-run preserved both device schema selections — `2026-11-23-m6ff-closeout/coexistence.json`) | NOT_EXECUTED | NOT_EXECUTED | NOT_EXECUTED |
| Production Core/collector e2e | PASS (`docs/merger/evidence/2026-11-22-m6fb-srlinux-r6`, `-r7`; M6F-C regression `2026-11-23-m6fc-srlinux-r8`; M6F-F scoped-schema run `2026-11-23-m6ff-closeout/srl`) | PASS (full production path on revision-pinned scoped bundle — `2026-11-23-m6ff-closeout/xrd-r4`; earlier `2026-11-22-m6f-xrd-e2e` refusal superseded) | NOT_EXECUTED | PARTIAL (explicit-mode slice PASS — `2026-11-22-m6f-ceos-e2e`; schema-bound N/A) | NOT_EXECUTED |

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
obtain library evidence and the schema-bound path is blocked at
applicability (fail-closed, NOT a PASS). Arista release-pinned audit:
`docs/merger/evidence/2026-11-23-m6fc-arista-yang-audit/`.

cEOS explicit slice (2026-11-22, evidence `2026-11-22-m6f-ceos-e2e/`):
production Core/collector run using `acquisition=explicit` +
operator-selected path (NO schema binding — the only eligible mode
without YANG Library). PASS: `observed` coverage with advancing
timestamps, interface-counter projection, withdrawal via empty
selection (collector subscription_count=0, timestamps frozen).
Finding: cEOS does not echo gNMI `Path.origin` in notifications —
expectation `origin` must be left empty or coverage can never match
(`observation_covers` requires exact origin equality); recorded as a
backlog policy question, not relaxed. Design:
`docs/M6F_CEOS_EXPLICIT_ACQUISITION.md`.

XRd (2026-11-22, evidence `2026-11-22-m6f-xrd/`): after the owner
raised `fs.inotify.max_user_instances`/`max_user_watches` to 64000,
`ios-xr/xrd-control-plane:24.4.2` (id `sha256:b3bd8e8e…`) boots under
clab `cisco_xrd` on mgmt `172.100.103.21` (distinct subnet so it
coexists with m6f-srlinux). Probe-level results: Capabilities PASS
(gNMI 0.10.0, encodings JSON_IETF/ASCII/PROTO, 1064 advertised
models), RFC 8525 YANG Library PASS (content-id `IOS-XR 7.5.1`, 4
module-sets / 2328 module entries), legacy YANG Library PASS,
subscribe-once PASS on
`/openconfig-interfaces:interfaces/interface[name=*]/state/counters`
(real per-interface counter JSON_IETF). gNMI service is plaintext on
TCP/9339 via the containerlab-injected `xrd.cfg` (`grpc port 9339
no-tls`, user `clab`); the earlier manifest guess of 5739 was wrong.

XRd production-path (2026-11-22, evidence `2026-11-22-m6f-xrd-e2e/`,
findings `BLOCKED-FINDINGS.md`): provisioning, collector binding,
credentials, assignment and Core-issued remote discovery all PASS
(snapshot 3, content-id `IOS-XR 7.5.1`). Schema-bound plan preview
correctly REFUSED activation — `overall_status=conflicted` —
because: (1) XRd's YANG Library declares only `running`/`candiate`
datastores, no `operational`; (2) capabilities-vs-catalogue
organization free-text mismatch on `ietf-interfaces`/
`ietf-yang-types` (`module_organization_conflict`, tested
fail-closed policy); (3) device OpenConfig revisions are 2022-era
(`openconfig-interfaces` 2022-10-25) vs the pinned 2026 catalogue
(`module_revision_unresolved`). No authority was weakened; unblocking
needs a revision-matched source closure and an org-semantics policy
decision (backlog).

XRd resolution (M6F-F, evidence `2026-11-23-m6ff-closeout/`): the
revision-matched closure was supplied through the generic
`module_scope` + `module_revisions` compile path — a 9-module bundle
pinning every device-reported revision (OpenConfig 2022-era +
`ietf-interfaces@2014-05-08`, `ietf-yang-types@2013-07-15`,
`cisco-xr-openconfig-interfaces-deviations@2024-02-21`) compiled
`complete` and was selected as XRd's device schema
(`ysc-9d38b26e36ba3474ee1a5be37f3f9a95`). Applicability reached
`applicable` with `content_id=IOS-XR 7.5.1`, the schema-bound plan
preview permitted activation, SAMPLE telemetry was observed with
advancing timestamps, revocation issued and the full withdrawal
verification passed (`xrd-r4`). Datastore note: XRd advertises
`running` + misspelled `candiate` — no `operational` datastore
exists; the approved plan binds `ietf-datastores:running`, which is
the datastore the device actually reports. Organization free-text
mismatch remains advisory under YANG-Library-backed evidence
(PR #61 policy). Simultaneous SR Linux/XRd operation on one core is
documented in `coexistence.json`.

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

IOS XE notes (Catalyst 9000v, remote DevNet Always-On sandbox
`devnetsandboxiosxec9k.cisco.com`, evidence class
`remote_hosted_virtual_nos` — a hosted virtual switch, NOT physical
hardware): the harness gained a `remote: true` manifest mode — no
containerlab deploy/destroy, DNS+TCP preflight, verified-TLS-only gNMI
probe (`--tls-mode verified`, optional operator CA bundle, never a
silent downgrade). Probe executed 2026-11-22
(`docs/merger/evidence/2026-11-22-m6f-iosxe/`): DNS resolves
(131.226.217.162), TCP/9339 accepts connections, but every gRPC/TLS
attempt is reset by the peer (`UNAVAILABLE: Connection reset by
peer`) and RESTCONF/NETCONF/SSH backends likewise return no service —
the time-limited reservation appears inactive. Capabilities is
recorded FAIL (connectivity), all dependent checks NOT_EXECUTED. Live
gNMI acceptance requires a fresh reservation plus refreshed
credentials via `BONSAI_LAB_IOSXE_USERNAME`/`_PASSWORD` — the
previously screenshot-exposed password is never used or stored.

FRRouting is a protocol platform, not a gNMI vendor — see
`lab/acceptance/m6f-frr/README.md`. gNMI rows: UNSUPPORTED (no
OpenConfig gNMI endpoint; FRR's daemon gRPC API is not gNMI).
Protocol-appropriate rows: BMP peer-up/down, route monitoring and
withdrawal PASS on executed evidence (FRR 10.6.x → Bonsai
routing_service: `2026-11-08-bn003-genuine-vrf-bmp`,
`2026-11-10-bn021c-nat-acceptance`, `2026-11-11-bn021f-authenticated-bmp`,
peer-down investigations); reusable pinned topology
`lab/acceptance/m6f-frr/` (FRR 10.6.2 digest-pinned); fresh live run
2026-11-22 PASS for peer-up/down, route announce/withdraw/re-announce
and BMP reconnect (`docs/merger/evidence/2026-11-22-m6f-frr/`);
BGP-LS UNSUPPORTED on the 10.6.2 image build (no BGP-LS AFI/SAFI).

Update this matrix only from captured evidence in
`docs/merger/evidence/*-m6f-*/`. Mock-gNMI suites and inferred
compatibility do not count.

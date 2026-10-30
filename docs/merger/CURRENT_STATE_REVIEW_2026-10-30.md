# Bonsai state review — 30 October 2026

Bonsai remains an integrated development prototype. The code has advanced well
beyond the checked-in baseline, but the active branch still points to `e4d8c44`
(the 12 October R08/R09 commit). BN-001–005, ownership mappings, compatibility
policy and this BN-006 slice are working-tree changes, not published commits.
The local tracking branch has no reported divergence; no remote refresh/push is
claimed. Preserve and review this work before attempting a clean checkout or
release build.

## Repository and document authority

`BONSAI_BACKLOG.md` is the single active queue; `docs/STATUS.md` is the state
snapshot. The former merger backlog/status files redirect correctly to them.
The RustyBMP handoff and NetOps strategy explicitly identify themselves as
reference designs, and their old task labels are superseded. No new backlog is
needed. This report records the current analysis and bounded implementation.

Earlier results remain historical evidence. In particular, 15 October live FRR
and browser captures are not presented as new 30 October live acceptance.

## Environment actually inspected

- Rust/Cargo 1.95.0, Node 20.20.2, Python 3.12.3 and Containerlab 0.79.0 are available.
- Docker is usable. Elasticsearch, Kibana, Grafana, Prometheus and Splunk were
  running with Docker health indicators. Their configuration/state was untouched.
- No listener was found on the inspected Bonsai ports 3000, 50051, 8090 or 11019.
  This does not rule out a nonstandard deployment elsewhere on the machine.
- No workspace `.env` was present. Secrets were not printed or copied. The old
  UI bundle was dated 15 October; the current source was subsequently rebuilt.
- Available local images include FRR 10.6.1, SR Linux 26.3.1-410, cEOS and XRd.
  Availability alone does not establish their Bonsai integration or license status.
- SR Linux `latest` resolves locally to digest
  `sha256:ad3d4a533674cf2e2e7321fef38806a6c66e3d56a3e8693c45fc2b4206078dd6`.
  The dedicated lab used that immutable digest, not a moving tag.
- Roughly 200 GiB disk space and adequate memory were available at inspection.
  Build/test timings were affected by concurrent compilation; no latency or
  resource-capacity benchmark is claimed.

## Merger standing

Bonsai owns the UI, authentication, graph, inventory and configuration. Imported
protocol/RIB/journal components live in `rbmp-core`, `rbmp-rib`, and `rbmp-store`.
The routing-service executable, durable state, public routing proxy and Svelte
workspace exist, with regression and earlier live-lab evidence. This is an
integration, not a verified retirement of every legacy path.

`src/streaming/bmp.rs` and other older paths still exist. BN-007 must inventory
which paths remain authoritative, prove migration/rollback equivalence, and
resolve upstream provenance and packaging before retiring anything. This turn
did not delete a legacy path, change journal identities, or claim the merger's
release gate complete.

The ownership registry persists explicit device/exporter+RD-to-tenant bindings.
The user's temporary `audit_only` access policy remains unchanged: tenant and
unsupported device-VRF restrictions do not block reads; authentication, role,
speaker and supported routing RD restrictions remain. It is not tenant isolation.
Strict mode restores the old denials, not ownership-aware query filtering.
No running Bonsai API was available here to inspect an operational policy or
populate real assignments.

## Bounded work continued: BN-006 native NOS evidence

The next canonical task was second-NOS acceptance. Existing A13 coverage used
hand-written flat JSON, and the adapter helper had no production caller. That
was insufficient evidence of native SR Linux state compatibility.

A dedicated two-node Containerlab pair was deployed with pinned SR Linux,
startup-only interface/BGP configuration and unique resource names. All later
NOS commands were read-only. Both neighbors reached `established`; a separate
CLI summary independently reported one established session and zero routes.
The actual NOS version was `v26.3.1-410-g98ed16c774c`.

Native JSON is nested by network instance/neighbor, with route counts inside
AFI/SAFI entries. Exact captures, version metadata, image/container identity,
commands and hashes were retained. Both dedicated containers and their network
were removed; the unrelated monitoring stack was left running.

Three pre-existing flat-parser defects were reproduced before changes:

1. Missing identity/state/counter fields could be classified `Complete`.
2. Missing version metadata was silently labelled `24.7.1`.
3. A peer ASN exceeding `u32` could wrap during conversion and remain `Complete`.

The legacy parser now reports schema mismatch with explicit missing/invalid
fields, uses `unknown` for absent versions, and checks ASN conversion. Its parser
version is 1.1.0. Legacy numeric placeholders remain for API compatibility only
under explicit schema mismatch; callers must check quality.

The new native adapter retains exporter, network instance, selected family,
peer, session state, optional counters, policy lists, parser lineage and raw
capture hash. It bounds input/record counts, detects duplicate identity,
distinguishes empty data from schema mismatch, and surfaces unparsed field
paths. Missing counters remain `None`. Real captured state is honestly partial
because many native fields are not projected; unknown features are not treated
as BGP failure. The parser is a library boundary with replay tests; production
automatic collection/tool dispatch is not yet wired to it.

The live lab only covered default-instance IPv4 neighbor state with genuine
zero route counts. Synthetic `blue` VRF/policy mutations in tests are labelled
as synthetic. No live VRF/RD isolation, BMP stream, prefix advertisement/update/
withdrawal/reconnect, RIB parity or FIB reachability is claimed. BN-006 stays
Partial. The reusable topology and capture commands are under
`lab/acceptance/bn006-srl/`.

## Verification and next action

Fresh checks passed: **46 focused Rust tests** (including 7 BN-006 regressions),
**132 imported-crate tests** (2 ignored older FRR tests are excluded), and **16
Python tests**. UI production build, `cargo fmt --check`, and `git diff --check`
passed. Existing UI accessibility/bundle warnings are retained in the build log.
The original 3 failing defect reproductions are retained and not counted as passes.
Full workspace/main-library tests, fresh FRR/browser gates, deployment, provider
quality evaluation and a soak/capacity run were not executed in this slice.

Evidence: [results manifest](evidence/2026-10-30-bn006/results.json).
The manifest distinguishes initial baseline checks, intentional pre-fix failures,
post-fix checks, capture commands and cleanup. Skips/ignored tests are not passes.
Source hashes identify the uncommitted snapshot.

Continue BN-006 with a dedicated second-NOS route lifecycle: controlled test
prefix/policy, genuine VRF/RD context and NOS/BMP/RIB event comparison. Wire the
native collection/adapter boundary before presenting it as an operator tool.
Keep FIB verification separate from BGP/BMP control-plane evidence.

Other open acceptance remains unchanged: BN-001 process-kill/crash phases;
BN-002 signing-key lifecycle and trusted live-assurance provenance; BN-003
router VRF/deployed browser-graph parity; BN-004 real provider quality/cost;
BN-005 the explicit compatibility exception; BN-007 migration/release closure;
and later ML/operational shadow-pilot work. No paid provider or operational
device-write path was exercised.

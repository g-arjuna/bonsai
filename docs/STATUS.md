# Bonsai current state

Updated: 30 October 2026 (Asia/Kolkata). Base Git commit: `e4d8c44`.
The BN-001–005 implementation is uncommitted working-tree code. Source hashes
and fresh results identify exactly what this review inspected.

**Standing: integrated development prototype.** Routing history/recovery is
substantially improved, dual live FRR streams and the browser workflow have
reproducible coverage. All three BN-001/002/003 items still have acceptance gaps.

Work from [BONSAI_BACKLOG.md](../BONSAI_BACKLOG.md). Latest review:
[30 October code/environment analysis and BN-006 native slice](merger/CURRENT_STATE_REVIEW_2026-10-30.md).
[Current evidence](merger/evidence/2026-10-30-bn006/results.json).
Earlier reviews and counts below are historical snapshots, not new live acceptance.

30 October revalidation: **46 focused Rust + 132 imported-crate tests** pass
(2 ignored tests excluded), plus **16 Python tests**. UI build and format/diff
checks pass; existing UI warnings remain. The new live slice is SR Linux CLI
capture only, not fresh FRR/browser or full second-NOS route acceptance.

| Area | Verified subcriteria | Remaining acceptance |
|---|---|---|
| BN-001 Partial | Durable scope/intent/completion records, direct failure/reopen tests, replay binding, open-time corruption/tail checks, cross-process scope ownership confirmed; runtime reload fails closed on corrupted/empty journal on disk (JournalPersistenceError); off-to-the-side atomic parsing under lock | Phase-specific process-kill and crash-phase recovery tests; actual device writes remain disabled |
| BN-002 Partial | Length-prefixed approval signing, mandatory enforced backing/identity/authority, report-fidelity binding, simulation-replay rejection; length-prefixed canonical report encoding eliminating identity delimiter collisions and binding limitations; enforced replay requires signing authority | Operational key lifecycle/migration policy and trusted live-lab assurance provenance (BN-014) |
| BN-003 Partial | Fresh 12-browser-journey run passes; submitted/fresh capture has zero exact fixture matches; native cookie and token-revocation checks exercised | Actual router VRF and deployed browser/graph restart assertions remain; BN-005 separately verifies actual process/container restart with new ingest |
| Routing history R01–R04 | 7 current regressions pass | Production scale/fault/soak remains release work |
| Wiring/scope R05–R06 / BN-004 | 2 wiring/scope regressions pass; provider clients have mock integration coverage | BN-004 real model comparison pending; mock scenario token/cost/conclusion is not model evidence; deployment/query scope under BN-005 |
| BN-005 Partial — temporary exception | Prior strict closure and current strict-mode regression assertions remain; default `audit_only` now relaxes tenant/unsupported-VRF denials at user request. Mappings persist; admin policy endpoint can restore strict denials | Tenant isolation is not enforced in compatibility mode. Authentication, roles, speaker and supported RD restrictions remain. See [policy contract](architecture/ownership_mapping.md) |
| BN-006 Partial | Pinned SR Linux 26.3.1 pair reached established eBGP; exact native/version captures, separate CLI summary, source/VRF/family-aware native adapter and flat-parser missing-field/version/ASN fixes | Default-instance IPv4 CLI only, zero routes. Automatic tool wiring, real prefixes/policy, BMP/RIB lifecycle and genuine VRF/RD/FIB acceptance remain |
| ML/research | Metrics/alignment/prototype smoke evidence remains | Trained held-out models, actual forecasting/provider comparisons remain separate gates |
| Release hygiene | Format, strict selected-core Clippy and UI build pass | UI warnings and full-product release gates remain |

BN-005 closeout snapshot verification: **62 focused Rust tests**, **19 explorer unit tests**, **132 imported-crate tests**
(**2 older FRR tests remain ignored and are not counted as passes**), and **12 live
FRR Chromium journeys** (plus both Rust browser orchestrator tests) pass. The real
Docker named-volume/container replacement gate and **16 Python tests** pass.
Formatting, strict selected-core Clippy and UI build pass; existing UI warnings remain.
The library test target compiles after repairing stale test fixtures; the other
249 library tests and a full workspace release/soak run are not claimed as executed.

The strict BN-005 profile remains tested. The current default has a user-requested
temporary exception: tenant and unsupported-VRF ownership restrictions no longer
block access. Speaker and supported routing RD filters remain. This default is
not a tenant isolation guarantee. Use the admin policy API to restore `strict`.
The original closeout manifest identifies its earlier tested source snapshot.

**Next action:** Continue BN-006 with automatic native collection/tool wiring and a real second-NOS BMP/RIB route lifecycle. BN-001/002 operational gates,
BN-003 genuine router VRF/deployed browser-graph parity, and BN-004 real provider
evaluation remain open. Operational device writes remain disabled.

Ownership mapping follow-up (BN-015): explicit device and exporter/RD → tenant
bindings persist in the graph, with admin assignment/list/audit endpoints. Missing
and mismatched mappings add no access denials. Tenant/unsupported-VRF denials are
now temporarily relaxed; authentication, roles and supported speaker/RD controls remain.
This is an inventory/audit foundation, not completed tenant isolation. See the
[rollout contract](architecture/ownership_mapping.md).

Ownership follow-up verification: **3 ownership tests + 32 existing regressions**
pass; formatting and diff checks pass. The ownership suite includes JWT and
API-key administrator attribution. [Evidence](merger/evidence/2026-10-15-ownership-audit/results.json).
No operational owner assignments have been populated.

Temporary access-policy follow-up: **38 distinct Rust tests pass** (35 existing
mapping/regression tests plus 3 compatibility tests). Strict acceptance fixtures
retain all original assertions; separate tests verify the requested relaxation,
retained speaker/RD/role constraints and persisted admin restore switch.
[Current evidence](merger/evidence/2026-10-15-ownership-compatibility/results.json).

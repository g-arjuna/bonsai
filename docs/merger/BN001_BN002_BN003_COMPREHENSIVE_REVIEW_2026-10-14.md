# BN-001 / BN-002 / BN-003 comprehensive acceptance review

> Historical audit snapshot. The four findings were subsequently repaired; current browser failures, capture provenance and sequencing are documented in the [latest readiness review](BN005_READINESS_REVIEW_2026-10-14.md).

Date: 14 October 2026 (Asia/Kolkata). Repository: Bonsai.
Base commit: `e4d8c4489da4b0048a72cd383685131c66adc7ae`.
The reviewed implementation remains in the working tree, not a new commit.
Source hashes and the working-tree inventory are recorded in the evidence manifest.

**Overall disposition: all three items remain Partial against their full acceptance.**
The new work repairs several 13 October findings and provides reproducible live
browser evidence. Current narrow gates all pass. That does not close remaining
runtime corruption handling, report encoding, live VRF and process-restart gates.
This review distinguishes completed subcriteria from unproven release acceptance;
it does not dismiss or reopen fixes that are independently verified below.

[Active backlog](../../BONSAI_BACKLOG.md) · [Current state](../STATUS.md) ·
[Prior detailed review](BN003_REVIEW_2026-10-13.md) ·
[Fresh evidence](evidence/2026-10-14-bn001-bn003-audit/README.md)

## Scope, method and evidence limits

Read current executor, signing/report hashing, shared router/authentication,
graph projection, collector and browser orchestration paths. Compared them with
BN-001/002/003's original acceptance and every major 13 October finding. Reran
current integration, imported-crate, Python, formatting, core Clippy and browser
gates. Added four diagnostic probes and an independent two-process scope-lock
confirmation. Tests use temporary journals and the dedicated FRR topology only.
No operational device execution or unrelated infrastructure was changed.

Older evidence is historical. The submitted BN-003 bundle was backed up before
execution and restored byte-for-byte after copying the fresh run. A new directory
retains today's logs, raw mixed-provenance BMP bytes, screenshots, checksums and
source hashes. Dates in old filenames do not identify today's execution.

The implementation is uncommitted, so the manifest's `git_commit` alone is not
sufficient to identify these fixes. Record a clean reviewable commit and a build
identity when the resulting acceptance is ready. BN-004 is outside this audit:
its new code/evidence and Verified label have not been independently reassessed.

## Results

| Check | Exact command | Result |
|---|---|---|
| Executor and related assurance | `cargo test --locked --offline --test a13_a20_change_assurance_and_executor --test r01_r04_routing_history_recovery --test r05_r06_runtime_wiring_and_scope -p bonsai` | 16 executor/assurance + 7 recovery + 2 runtime/scope passed |
| Live routing browser | `DOCKER_HOST=unix:///var/run/docker.sock cargo test --locked --offline --test bn003_live_routing_browser_acceptance test_bn003_live_routing_browser_acceptance -- --nocapture` | 1 orchestration test passed; 12 Playwright journeys passed in 18.9s; 1 unrelated Rust test filtered out |
| Imported foundation | `cargo test --locked --offline -p rbmp-core -p rbmp-rib -p rbmp-store` | 132 passed, 2 ignored |
| Python | `PYTHONPATH=. .venv/bin/pytest tests/test_*.py` | 16 passed in 6.42s |
| Format | `cargo fmt --check` | Passed |
| Selected core lint | `cargo clippy -p rbmp-core --locked --offline -- -D warnings` | Passed; this is not whole-workspace Clippy |
| UI build | `npm --prefix ui run build` | Succeeded in 6.87s with warnings |
| Additional review checks | `cargo test --locked --offline --test bn001_003_comprehensive_review_probes -- --skip process_scope_worker` | 5 top-level checks passed: 4 undesirable behaviors reproduced and 1 cross-process confirmation; worker subprocess passed separately |

There are **158 passing existing Rust tests**, **12 browser journeys**, and
**16 Python tests** in this fresh audit; 2 Rust tests ignored. The five review
checks are accounted for separately. The child worker is an implementation detail
of one cross-process confirmation, not an extra product acceptance test.
Temporary probe target removed after execution; retained [source](evidence/2026-10-14-bn001-bn003-audit/review-probes.rs)
and [log](evidence/2026-10-14-bn001-bn003-audit/probes.log) permit reproduction.

## BN-001 — durable executor and recovery

Required invariant: before acknowledging or retrying an execution, durable state
must establish ownership, intent, result and the exact idempotency binding. A
journal error must not produce a fabricated success or erase an unresolved intent.
Concurrent processes must agree on that authority, including recovery and repair.

| Acceptance point | Disposition | Evidence / remaining boundary |
|---|---|---|
| Persist scope acquisition before effects | Verified focused behavior | `LockAcquired` is synced before the in-memory acquisition is accepted |
| Independent instances see a newly persisted competing owner | Verified | Existing concurrent-instance regression passes |
| Different processes with stale initial snapshots see competing ownership | Verified focused behavior | New parent/child probe: child opens first, parent acquires, child is rejected on reload |
| Durable intent before simulated effects | Implemented; focused recovery verified | `ExecutionIntent` precedes simulated steps; fixture-written interrupted intent reloads and blocks retry |
| Actual process kill between execution phases | Not verified | Current interrupted-execution test writes records by hand; no kill/restart at phase boundaries |
| Completion synced before in-memory completion | Verified focused behavior | Current append-failure regression passes; memory update follows completion append |
| Recover completed idempotency across reopen | Verified | Completed-entry restart test passes |
| Bind replay to plan/hash/device/VRF/tenant | Verified focused cases | Mismatched plan/scope/tenant regressions pass; legacy empty-tenant handling needs migration policy |
| Reject malformed interior JSON/UTF-8 during open | Verified | Opening corrupt journal fails rather than silently skipping records |
| Repair torn final record and permit future restart | Verified focused cases | On-disk truncation/newline repair tests pass |
| Reject new corruption on an already-open executor | **Reproduced failure** | Execution ignores `reload_from_journal_path` errors; completed replay can still succeed |
| Respond safely to a missing/empty durable journal | **Reproduced failure for empty file** | Early return leaves old in-memory completion intact; reopen sees no history, live executor returns replay |
| Serialize all journal read/repair/append transitions | Partial | File guard covers open and scope acquisition/release, but execute's reload and intent/abort/completion append do not share that guard |
| Report lock-release persistence failures consistently | Partial | Public release returns errors; execution discards release errors after completion/precheck failure; release also ignores reload failure |
| Cross-process idempotency/in-flight transitions | Not verified | Ownership confirmation does not prove concurrent same-owner/same-key execution or append/repair races |
| Recovery decision for interrupted work | Partial | Retry is blocked for in-flight records; explicit operator reconciliation/resolution flow remains undefined |
| Keep real device effects disabled | Verified source boundary | Current effects/recovery are recorded strings and caller-supplied check results |

### New reproductions

**Journal corruption after completion:** run a valid simulated execution, append
`{broken-json}\n` to the temporary journal, and reopen it. Reopen correctly fails.
The already-open executor nevertheless returns `deduplicated_replay = true` for
the same key because its execution path discards reload errors after rebuilding
part of the state. Opening safely and executing safely are different gates.

**Empty durable journal:** complete execution, replace only that test's temporary
journal with an empty file, then compare reopen with the live executor. Reopen
has zero entries; the live executor returns the cached completion. An external
truncation is a failure-injection case, not evidence of a normal writer truncating
the file. It shows durable-authority loss is not detected consistently.

**Cross-process ownership:** the independent child-process confirmation passes.
Yesterday's concurrent-scope finding is repaired for distinct owners. Do not
carry that old bug forward as unresolved; the remaining issue is consistent
serialization/error propagation for the rest of the state machine.

### Closure actions, in order

1. Propagate every journal reload/repair error; build replacement state off to
   the side and publish it only after successful parsing. Define fail-closed
   handling of a vanished/empty file after persisted history existed.
2. Use one transaction/serialization boundary for reading, deciding and persisting
   ownership, intent, completion and recovery. Prevent an unlocked repair from
   truncating another writer's partial record. Recheck idempotency after acquiring
   durable authority. Define same-owner reentrancy explicitly.
3. Preserve/diagnose unresolved locks after persistence failure; return structured
   outcomes rather than dropping release errors. Test phase-specific write/sync
   failures, disk exhaustion and concurrent operations on different scopes.
4. Add actual child-process kill/restart tests between lock, intent, effects,
   completion and release. Confirm identical result/history and explicit unresolved
   work after every crash point. Do not turn these into live device-write tests.

## BN-002 — approval, signing and assurance boundaries

Required invariant: enforced execution and enforced replay must authenticate the
exact approved plan, report, identity/scope and execution class. The signed bytes
must have one interpretation. A valid HMAC authenticates approval content; it does
not independently establish that a real lab produced the report.

| Acceptance point | Disposition | Evidence / remaining boundary |
|---|---|---|
| Separate simulation and enforced modes | Verified | Typed mode and persisted completion mode; simulation replay rejected in enforced mode |
| Require durable backing in enforced mode | Implemented | Entry point rejects absent journal; adversarial failure of backing remains BN-001 |
| Require non-empty plan/approval tenant | Implemented | Entry point rejects blank identities and fresh execution checks tenant equality |
| Require signing authority/signature | Verified focused cases | Missing authority/unsigned approval regressions pass |
| Constant-time HMAC verification | Implemented | `verify_slice`; tampered/wrong-key signature regressions pass |
| Versioned unambiguous approval bytes | Verified focused cases | Approval encoding is length-prefixed and binds key metadata, tenant, actor and report hash |
| Reject unsigned fidelity mutation of a bound report | Verified | Changing modeled report to live after signing changes report hash and is rejected |
| Reject modeled report in enforced execution | Verified | Explicit fidelity gate |
| Bind exact report content unambiguously | **Reproduced encoding gap** | Report digest still uses delimiter-concatenated strings; different report/plan identities hash identically |
| Bind fidelity limitations presented to the operator | **Reproduced omission** | `emulation_fidelity_limitations` is omitted from the report digest |
| Verify authority on enforced replay | Verified repaired cases | Missing authority/signature and simulation replay checks added; prior bypass probe now has a regression |
| Enforce all replay identity/report/expiry policy | Partial | Replay has a separate validation branch; no current fresh-approval tenant/scope/hash/expiry checks beyond cached-plan binding and optional report-hash comparison |
| Key rotation/revocation | Partial | Local key registry and revoked IDs exist; fallback to unnamed/default key remains, operational policy/persistence not established |
| Authenticated assurance provenance | Partial | Signed approval binds report digest; report fidelity still originates in caller-provided snapshot, not a trusted capture record |
| Authenticated actor and permitted tenant/scope | Not established by this library | Signed actor text is an assertion; identity/role/tenant authority belongs to authenticated entry points |
| Actual live twin correctness | Outside BN-002 closure; BN-014 pending | Real twin collection/execution remains a separate gate; do not equate a live enum in unit fixtures with a lab run |

### New report-hash reproductions

Two distinct reports with `(report_id="a:b", plan_id="c")` and
`(report_id="a", plan_id="b:c")` produce identical hashes when remaining fields
are equal. This is ambiguous serialization, not a cryptanalytic SHA-256 collision
and not a demonstrated end-to-end unauthorized device execution. Other plan/report
checks still apply. The report serializer needs the same discipline already
applied to approval serialization.

Changing only `emulation_fidelity_limitations` also leaves the hash unchanged.
If a signer reviews these notices as part of approving the report, they must be
bound; otherwise clearly define a signed safety payload separately from mutable
presentation metadata and prevent the latter from changing operator meaning.

### Closure actions, in order

1. Define a versioned report schema and canonical structured or length-prefixed
   bytes. Bind the complete safety-relevant report, including limitations and
   evidence/provenance references; reject unknown/inconsistent combinations.
2. Consolidate fresh-execution/replay authorization or explicitly specify which
   checks may differ. Test replay with altered tenant/scope/plan approvals,
   expired/revoked authority and missing report binding. Cached replay is read
   access, not a new device write, but it still needs an explicit policy.
3. Define exact key-ID resolution, legacy fallback/migration, rotation/revocation
   persistence and actor/scope authority at the authenticated boundary.
4. Establish who may assert live assurance and how independently collected
   evidence is linked to its immutable report. Keep live-twin claims under BN-014
   until actual lab evidence exists.

## BN-003 — live routing and RIB browser acceptance

Required invariant: the production UI/proxy/auth path must reflect actual routing
observations through changes, transport loss and process restart without crossing
source/VRF boundaries or inventing healthy/complete states.

| Acceptance point | Disposition | Evidence / remaining boundary |
|---|---|---|
| Shared production router and login/logout handlers | Verified repaired path | Wrapper now calls `router_from_state(AppState::new_minimal(...))`; duplicate auth handlers removed |
| Native server-issued cookie login | Verified repaired path | `page.request` processes `Set-Cookie`; browser cookie flags inspected before navigation; later test contexts reuse those cookies |
| Unauthenticated API denial / UI alert | Verified | Routing API 401 and DOM error; static SPA navigation itself is permitted, not an HTTP 401 |
| Logout cookie clearance / UI denial | Verified current assertions | 12th journey checks cookie removal and API-error UI; saved pre-logout token replay/denylist restart is not separately asserted |
| Speaker/VRF API-key denial | Verified selected endpoints | Explicit 403 assertions on prefix-history; all-endpoint/tenant matrix remains BN-005 |
| Two genuine exporters visible | Verified repaired assertions | Actual FRR connections and DOM assertions for `.11` and `.12` |
| Pre-policy, post-policy, Loc-RIB views | Verified live global prefix | `10.10.1.0/24` renders in all three views |
| VRF versus global isolation | Verified fixture/browser behavior | Selected RD includes expected prefix and excludes the other RD's prefix |
| Actual router VRF BMP export | **Not verified** | RD `65000:100` is still created by `build_rd_bmp_frames` and direct ingest, not an FRR VRF |
| Router announce, MED/AS-path change, withdrawal | Verified live behavior | `vtysh` changes produce matching DOM presence/attributes and removal |
| Router-driven BMP disconnect/reconnect | Verified improved harness path | Disable/re-enable FRR BMP target; test reader records EOF/error and real socket reconnect |
| Same production collector listener/epoch allocation | Not fully exercised | Harness owns frame reader/epoch values instead of launching `bonsai-routing` listener; analogous logic exists in production |
| Preserve routes without synthesized withdrawals | Partial assertions | Route presence and nonzero KPI asserted; exact before/after counts and no new withdrawal events not asserted |
| Reconnect completeness | Verified stronger assertion | Badge must contain complete and exclude gap; source `.11` is asserted; epoch supplied/logged by harness rather than asserted through API/DOM |
| History exact attributes | Partial assertion precision | NH, AS, prepend and MED 150 checked across table; row-specific before/after transition values and sequence are not compared |
| Empty/unsupported state | Verified selected states | Missing prefix and BGP-LS unsupported notice; independent stale/unknown/error transitions not exhaustively exercised |
| Graph projection | Partial acceptance | Lag badge zero; representative graph nodes/edges, checkpoint and absence of duplicate projection not independently queried |
| Journal reload and continued shared-state reference | Verified repaired reload path | Restart swaps shared state and UI retains global/fixture routes and both exporters |
| Actual service process restart / post-restart new ingest | **Not verified** | HTTP task abort/state replacement occurs within one process; no deployed binary kill/restart/new route after restart |
| Raw capture/hash provenance | Improved; mixed source | Binary now saved and hash reproducible; file includes injected fixture bytes, so it is not exclusively network-captured traffic |
| Build/test revision provenance | Partial | Commit recorded but reviewed code uncommitted; this audit adds source hashes and working-tree identity |
| Browser/tool portability | Partial | Cached Chromium override exists; Rust runner still constructs one specific cached revision path |

### Closure actions, in order

1. Configure actual FRR VRF/RD export and obtain router output plus per-exporter,
   per-epoch raw BMP captures. Keep fixtures separately named and attributed.
2. Launch the actual routing service process and production collector loop;
   verify EOF/reconnect/epoch behavior, kill/restart from durable storage, then
   receive a new announcement/change after restart.
3. Add row-specific history, route/event count and no-fabricated-withdrawal
   assertions; require explicit source/epoch/completeness. Query representative
   projected graph records and persisted checkpoint after restart.
4. Test revocation of a saved pre-logout JWT, not just removal of its cookie.
   Capture actual Playwright result JSON, build/source identity and timestamps.
   Second-NOS acceptance remains BN-006 rather than silently becoming BN-003 work.

## Comparison with the 13 October review

| Earlier finding | Current disposition |
|---|---|
| Competing executor instances acquire same scope | Repaired for distinct owners; now confirmed across processes too |
| Report fidelity mutation bypasses enforced gate | Repaired when report hash is present; new serializer/limitations gaps remain |
| Enforced replay bypasses authority entirely | Repaired for authority/signature/mode cases; full replay policy remains incomplete |
| Enforced execution allows no backing/empty tenant | Entry-point rejection implemented |
| Execution drops abort-append errors | Now returns journal error; lock-release/reload errors still discarded in some paths |
| Separate login router implementation | Repaired by shared router/handlers |
| Cookie manually fabricated from JSON token | Repaired initial login; server-issued cookie inspected and reused |
| Only one exporter asserted | Repaired: both exporters checked |
| Synthetic VRF presented as live | Manifest now admits fixture provenance; backlog/status still overstate live VRF closure |
| Reconnect can pass merely by lacking gap | Improved: explicit complete assertion; production epoch/process acceptance remains |
| Old BMP readers retain old state after reload | Shared state swap repaired; actual process restart still absent |
| Raw hash names unavailable file | Repaired: binary written; fixture and live bytes still mixed |

## Closure decision and work order

Do not mark BN-001/002/003 fully Verified yet. Update their descriptions to name
which subcriteria pass and which acceptance remains. Preserve all old evidence
as historical snapshots; this report supersedes its present-state conclusions.

1. BN-001: fail-closed journal reload and consistently serialized state machine.
2. BN-002: unambiguous complete report binding and explicit replay authorization.
3. BN-003: live VRF plus actual collector/process-restart acceptance and precise
   evidence assertions.

These are subactions of the existing three IDs, not a new backlog. BN-004's label
is outside this audit. BN-005 deployment auth/persistence and BN-006 second NOS
remain independent known gates. No conclusion here establishes an operational
executor, autonomous remediation, actual model quality or a release-ready product.

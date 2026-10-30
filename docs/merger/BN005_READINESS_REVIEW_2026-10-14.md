# BN-005 readiness and remediation verification — 14 October 2026

Reviewed working-tree changes on `e4d8c44`. The new implementation is not yet
committed. This report supersedes the current-state conclusions in the preceding
[comprehensive review](BN001_BN002_BN003_COMPREHENSIVE_REVIEW_2026-10-14.md), while
preserving that review and its reproductions as historical evidence.

**Decision: the executor/report fixes check out, but this is not a clean BN-003
acceptance.** The first fresh browser run failed reconnect; an unchanged rerun
passed. The submitted live-only capture also contains synthetic fixture frames.
Fix/diagnose these two BN-003 findings before treating BN-005 as the sole next
focus. BN-005 read-only deployment hardening can proceed independently while
these failures remain explicitly open. Broader crash/VRF/process/model criteria
remain Partial; no conclusion here establishes operational executor readiness.

## Repairs that check out

| Earlier finding | Current implementation and evidence |
|---|---|
| Runtime replay ignores corrupt journal | Reload failure becomes `JournalPersistenceError`; replacement maps are built separately and published only on successful parse |
| Empty durable journal can replay cached completion | Existing history/intent/lock markers cause empty/vanished journal reload to fail; regression covers empty-file replay |
| Report identity delimiter collision | V2 length-prefixed report encoding includes field lengths and vector counts; independent prior collision case now produces distinct hashes |
| Report digest omits fidelity limitations | Canonical bytes now include limitations, unsupported constructs and invariant IDs/results/details; mutation regression changes report hash |
| Inconsistent read/repair/append locking | Execution acquires the journal guard before reload and retains it across validation, intent, completion and scope release; public lock methods also reload under lock |
| Replay uses weaker authorization | Shared validator applies plan/report/scope/expiry/signature/tenant checks; enforced simulation replay is rejected before authority checks |
| Release persistence error discarded after completion | Completion path propagates lock-release errors. A completion may already be durable when release fails; recovery/returned outcome semantics still need phase-fault acceptance |
| Logout clears cookie without proving revocation | Journey 12 saves the JWT and asserts its bearer replay returns 401 after logout |
| Capture mixes live and fixture bytes without clear identity | Separate files/hashes exist, but epoch-2 fixture reinjection still appends to the live recorder; the submitted live file contains three exact fixture frames |

Validation ordering that reports an invalid assurance gate before a plan mismatch
changes rejection precedence; it does not itself prove extra assurance fidelity.
The shared validator and focused regressions are the meaningful improvements.
Approval signing and report hashing are different versioned encodings. Existing
approval/report migration compatibility needs an explicit policy before release;
new signatures must not silently be described as compatible with old encodings.

## Submitted evidence integrity

Both supplied files match their stated SHA-256 and manifest:

| Artifact | Bytes | SHA-256 |
|---|---|---|
| `frr_live_network_capture.bin` | 5,239 | `381ea02c1c513c8b9c653b4b09469c41360cdbf13d3614549a359599cc814d81` |
| `synthetic_vrf_fixture.bin` | 329 | `3db7c3b2c997a824fdfcc0bf0b860c6f36b0c0937ff1ce707ddb7ad4329d3e03` |

These validate artifact identity, **not the asserted pure-live provenance**. The source
clones `raw_bytes_log` for the epoch-2 recorder, then appends `vrf_peer_up2`,
`vrf_rm2` and `vrf_eor2` to it at lines 750–752. Independent BMP frame splitting
finds three frames in the submitted live file that exactly match the fixture:
PeerUp (126 bytes), route monitoring (102 bytes), End-of-RIB (71 bytes), totaling
**299 bytes**. All carry fixture peer type 1 and RD bytes `0000fde800000064`.
The live file has 46 frames; the separate initial fixture file has four.
[Frame comparison](evidence/2026-10-14-bn005-readiness/capture-provenance-check.json)
records exact frame hashes. Move every reinjection into the fixture recorder and
assert live-only output excludes these known fixture frames.

These files do not prove a live router VRF, independent
forwarding/data-plane behavior or successful process-crash recovery. Network
bytes from two exporters are still combined in one file; per-exporter/session
metadata is desirable for independently replaying identity and epoch behavior.
Fresh run artifacts are saved separately and the submitted bundle restored
byte-for-byte. BMP timestamps/event ordering can change fresh capture hashes;
fresh hashes must match their own manifest rather than equal a previous run.

## Fresh verification and reconnect failure

| Check | Result |
|---|---|
| Executor/assurance | 20 passed in 2.07s |
| History + runtime/scope | 7 + 2 passed |
| Imported foundation | 132 passed, 2 ignored |
| Router sanity | 1 passed in the first full BN-003 test-target invocation |
| Python | 16 passed in 6.27s |
| Format / strict `rbmp-core` Clippy | Passed |
| UI build | Successful in 7.28s with warnings |
| Live browser, attempt 1 | **Failed:** 7 journeys passed, journey 8 failed, 4 did not run; Cargo exit 101 |
| Live browser, diagnostic attempt 2 | 12 journeys passed in 14.6s; Cargo exit 0 |

First invocation: `DOCKER_HOST=unix:///var/run/docker.sock cargo test --locked
--offline --test bn003_live_routing_browser_acceptance -- --nocapture`.
Diagnostic rerun: same flags with `test_bn003_live_routing_browser_acceptance`
filter, with assertions and implementation unchanged. The difference excludes
only the independent router sanity test; this is not sufficient evidence to
attribute the failure to concurrent test execution. No further retries were run.

The first run accepted `.11` at epoch 2, then timed out waiting for `complete`:
actual header remained `Completeness: gap`. Its failure snapshot shows both
speaker rows as `complete_for_observed_view` while overall completeness remains
gap. This discrepancy warrants per-request health/gap/epoch logging and a
reproduction of disconnect/reconnect ordering; root cause is **not established**.
Do not increase the timeout or weaken the completeness assertion to hide it.
The unchanged rerun passes, demonstrating an intermittent gate on this checkout,
not proof of deterministic acceptance. Failure screenshot/context and both logs
are preserved. Journey 12's new revocation assertion passed on the rerun, but was
not reached on the failed attempt.

There are 162 passing non-live Rust checks in this review (including router
sanity); live browser results are mixed and reported separately. The rerun is not
used to erase the failed gate or promote it to a clean aggregate pass.

## Remaining acceptance, kept under existing IDs

| Item | Remaining work | Effect on proceeding to BN-005 |
|---|---|---|
| BN-001 | Actual subprocess kill/restart across acquisition, intent, completion and release; phase-specific write/sync failure recovery; explicit interrupted-work reconciliation | Keep executor operational acceptance open; does not block read-only deployment hardening |
| BN-001 | Durable identity/backup policy for journal disappearance discovered only after reopening with no prior memory; deterministic completion-with-unreleased-lock outcome | Clarify recovery contract before enabling execution; source check catches disappearance on an already-open instance |
| BN-002 | Operational key rotation/revocation persistence and exact key-ID/legacy fallback policy; signing/report encoding migration | Retain as open authorization lifecycle work; no need to add a key-management API merely to satisfy a helper test |
| BN-002 / BN-014 | Authenticate who may assert live assurance and bind real lab provenance; actor/tenant/scope authority at the authenticated boundary | Signed report content is now protected, but signing a caller-supplied live flag does not prove a twin run |
| BN-003 | Diagnose intermittent reconnect completeness and remove fixture contamination from the live recorder | Address these concrete findings before presenting a clean routing/browser gate; BN-005 can proceed independently |
| BN-003 | Actual FRR VRF/RD export; fixture isolation is verified but remains synthetic | Correctly separate and label fixture coverage; it does not close live VRF acceptance |
| BN-003 | Launch/restart actual `bonsai-routing` process and ingest a new route afterward using its production listener/epoch allocation | Current restart aborts/rebinds HTTP and replaces state in the same process |
| BN-003 | Exact route/event counts and no fabricated withdrawals; row-specific before/after history; representative graph records/checkpoint and epoch assertions | Existing DOM checks are useful but weaker than these criteria |
| BN-004 | Real provider/model execution and held-out policy comparisons with actual token/cost provenance | Current Verified label overstates mock-only provider evidence; treat as Partial |
| BN-007 / BN-019 | Clean reviewable source revision, package/config upgrade, rollback and remaining release gates | Current code is still uncommitted; evidence commit alone does not identify the reviewed implementation |

The remaining items are established acceptance criteria, not newly invented
requirements. Actual device writes remain disabled. A real-twin operational pilot
belongs under BN-014; moving to BN-005 is a development sequencing decision.

## BN-004 evidence boundary relevant to readiness

The suite uses `start_mock_wire_server` with production Anthropic/OpenAI client
implementations, which is valid HTTP/provider wiring coverage. Its scenario
comparison uses `ScenarioMockProvider`: hardcoded tool calls, `policy_filtering`
conclusion, evidence ID, tokens and costs. This is not real hosted/local model
inference or a measured model-policy comparison. Source inspection establishes
this limit; BN-004's six tests were not rerun in this review and no provider calls
were made. Keep the verified wiring subcriteria and reopen only the real-provider
acceptance portion, rather than discarding the existing tests.

## Concrete BN-005 starting scope

BN-005 is useful now because deployment defaults directly affect the read-only
routing/investigation foundation. Work on one bounded subaction at a time:

1. Build a deployment matrix for direct binaries, Compose and supported service
   launch paths: public authentication, internal token, listen interfaces,
   journal paths and persistent mounts. Make protected production paths fail
   closed when required settings are missing. Use an explicit local development
   exception rather than accidental opt-in security.
2. Validate identity-derived tenant/exporter/VRF scope on every public routing and
   investigator endpoint. The current scoped API-key tests cover selected paths;
   caller-provided tenant labels alone do not establish isolation.
3. Enforce read-only graph access and cancel/limit actual query execution. Current
   `.take(201)` limits collected rows, while writable connection plus keyword
   filter and timeout of `spawn_blocking` do not supply database-level read-only
   or query interruption guarantees.
4. Fix diagnostic history retrieval after long operation. Several helpers read
   the first 5,000/10,000 events from sequence zero; later relevant evidence can
   be missed. Use scoped pagination/indexing with explicit coverage/truncation.

This is the existing BN-005 scope, not a new backlog. Its first concrete task is
step 1: default authentication, internal-token and journal persistence behavior
for the supported launch paths, verified with startup and request tests.

## Verification and limits

Exact fresh commands, results, source hashes and artifact integrity are recorded
in the [evidence manifest](evidence/2026-10-14-bn005-readiness/results.json).
The existing targeted suites and imported foundation pass; the browser gate
failed then passed on a diagnostic rerun;
UI build success retains warnings. Whole-workspace Clippy, actual executor crash
phases, router VRF export, deployed service restart, real provider evaluation and
operational device execution were not performed. The raw submitted hashes above
were independently checked before rerunning the browser gate.

No implementation fixes, commits or pushes were made during this review. The
active backlog/status are reconciled with these dispositions, earlier evidence
preserved, and fresh evidence retained for review.

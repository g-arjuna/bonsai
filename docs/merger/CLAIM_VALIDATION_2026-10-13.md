# Claim validation — 13 October 2026

Reviewed Bonsai `main` at `e4d8c4489da4b0048a72cd383685131c66adc7ae`.
The worktree was clean before this review. Read-only `git ls-remote origin
refs/heads/main` returned that exact commit, confirming the pushed tip.
`657dee5` is its predecessor remediation commit. This review changes planning
documents and retains evidence; it does not fix the new execution findings.

**Conclusion:** The stated narrow checks are reproducible and the routing
recovery fixes are substantial. “All R00–R10 fully remediated” is not supported
by the original acceptance scope. R08 and R09 remain partial; R07 verifies a
live FRR backend, not browser or second-NOS acceptance. The merger is an
integrated development prototype with an improved durable routing foundation.

The only active queue is [BONSAI_BACKLOG.md](../../BONSAI_BACKLOG.md).
[Current state](../STATUS.md) summarizes this review. Historical acceptance
criteria remain in the [planning archive](../backlog_archive/2026-10-13/README.md).

## Fresh verification

All commands below ran against this checkout. The five Rust suites were grouped
in one Cargo invocation, preserving each suite's result. Logs and fresh generated
outputs are retained in [the evidence directory](evidence/2026-10-13-validation/).
Previously committed generated reports were restored after copying the new runs;
historical evidence was not replaced with today's outputs. The harness-generated JSON may retain hardcoded historical dates; [results.json](evidence/2026-10-13-validation/results.json) identifies this fresh execution.

| Check | Executed command | Result / boundary |
|---|---|---|
| Scenario | `cargo test --locked --test a10_a11_a12_operator_and_scenario_pack` | 3 passed; includes simulated model trace branches |
| Assurance/executor | `cargo test --locked --test a13_a20_change_assurance_and_executor` | 6 passed; modeled assurance and completed-entry restart coverage |
| Recovery | `cargo test --locked --test r01_r04_routing_history_recovery` | 7 passed; original large-batch/crash-tail/gap regressions now pass |
| Runtime/scope | `cargo test --locked --test r05_r06_runtime_wiring_and_scope` | 2 passed; separate HTTP routing path, projector and mocked provider |
| M3 helpers | `cargo test --locked --test b16_b17_b18_b19_m3_release_gates` | 3 passed; its FRR/multivendor case remains synthetic |
| Live FRR | `DOCKER_HOST=unix:///var/run/docker.sock cargo test --locked --test r07_frr_containerlab_verification` | 1 passed; two clean runs using actual FRR 10.6.1 |
| Imported crates | `cargo test --locked --offline -p rbmp-core -p rbmp-rib -p rbmp-store` | 132 passed; 2 ignored, not counted as passed |
| Python | `PYTHONPATH=. .venv/bin/pytest tests/test_*.py` | 16 passed in 7.11s |
| Format | `cargo fmt --check` | Passed |
| Core lint | `cargo clippy -p rbmp-core --locked --offline -- -D warnings` | Passed with warnings denied; not whole-workspace Clippy |
| UI | `npm --prefix ui run build` | Successful in 7.53s; accessibility, plugin timing and large-chunk warnings remain |
| Additional executor probes | `cargo test --locked --test claim_validation_executor_probes` | 4 observed-boundary probes passed; they reproduce undesirable behavior, not safety acceptance |

Total Rust checks: 154 ordinary tests passed (132 imported + 21 integration +
1 live lab), plus 4 diagnostic probes; 2 imported tests ignored. No fresh browser
session, real provider benchmark, trained STGNN run, foundation-model inference,
second-NOS run or operational pilot was executed or claimed.

Only `lab/fast-iteration/frr-bmp-r07.clab.yml` resources were used for the FRR
gate. Its cleanup removed its own lab; the existing stopped SR Linux lab and
other running integration services were left intact.

## Claim dispositions

| Claim | Assessment | Remaining acceptance |
|---|---|---|
| R00 documentation reconciliation | Incomplete before this review: STATUS marked everything complete, BACKLOG still said R07/R08 pending, release reports retained obsolete R05/R06/R10 pending text, CANONICAL named empty DV2 as active, archive index pointed to missing CV7 | Current consolidation establishes one queue and a factual snapshot |
| R01 direct durable event delivery | Verified focused repair: `process_for_epoch` returns events directly and the 5,000-route regression passes | Preserve scale/recovery regression gates |
| R02 raw/event reconciliation | Verified focused repair: startup reconstructs missing event records deterministically and survives repeated restart | Broader production fault/soak evidence remains a release gate |
| R03 torn-tail repair | Verified focused repair: both journals repair the on-disk tail before append; next restart works | Retain regression tests |
| R04 epoch-fenced durable gap | Verified focused repair and live FRR disconnect/reconnect | Multi-source/session coverage and browser rendering remain |
| R05 runtime wiring | Real callers exist; remote MCP/projector and durable budget/dedup paths pass integration tests | Real provider and long-running production behavior, concurrency/cancellation |
| R06 scopes/defaults/bounds | Improvements verified: scoped API keys, speaker/VRF filtering, required Compose token and bounded Rust row collection | Direct launch can still have no token; tenant provenance, database-level read-only enforcement and actual query cancellation need acceptance |
| R07 genuine lab | Supported for actual FRR backend: announcement, MED change, withdrawal, BMP loss, reconnect and reproducible RIB digests | Browser end-to-end and actual second-NOS verification remain separate gates |
| R08 evaluation | Honest measurement tags and validation-clean calibration are improvements; genuine PyTorch forward pass exists | No trained STGNN held-out episode experiment, actual Chronos inference or measured LLM comparison |
| R09 operational assurance | HMAC verification, optional file-backed completed-entry replay and durable shadow records exist | Durable in-flight execution/locks, mandatory approval authority, provenance-backed live twin and real shadow pilot are not completed |
| R10 hygiene | Workspace formatting and strict selected-core lint pass | No claim of full workspace Clippy or all release gates passing |

## Newly reproduced executor gaps

Reproduction source: [executor-probes.rs](evidence/2026-10-13-validation/executor-probes.rs).
Result: [executor-probes.log](evidence/2026-10-13-validation/executor-probes.log).
The temporary test target was removed after execution. These are observations of
current behavior, so a passing probe means the gap was reproduced. When fixing
BN-001/002, convert them into assertions of the required safe behavior.

1. **Failed persistence can become a successful replay.**
   `execute_approved_plan` inserts a completed entry into the in-memory map and
   journal before opening/writing/syncing its JSONL file. A directory obstructing
   the journal path makes the first call return an error; a retry with the same
   key returns `deduplicated_replay = true` despite no durable entry. Persistence
   errors are classified as `PrecheckFailed`, although they occur after simulated
   steps and in-memory completion. This is BN-001's first regression.
2. **In-flight locks are not recovered.** Acquiring a scope lock and reopening the
   file-backed executor permits a second owner to acquire that same scope. Only
   completed entries are loaded; active locks and execution intents are never
   persisted. The existing restart test reopens an executor after successful
   completion in one process; it does not kill an executor between steps or prove
   recovery of locks in a second process.
3. **Unsigned modeled execution is accepted by the default constructor.**
   Signature checks run only when `with_signing_secret` is used. A modeled snapshot
   can produce `executable = true`; `ApprovalBoundExecutor::new()` accepts its
   unsigned approval. The `LiveContainerlabVerified` enum is caller supplied, not
   established by this function's collection of lab evidence. Existing healthy
   test twins set the live enum even though they are synthetic fixtures.
4. **Signing bytes are ambiguous.** Approval fields use pipe-delimited formatting.
   `(approval_id="a|plan_id=b", plan_id="c")` and
   `(approval_id="a", plan_id="b|plan_id=c")` produce identical signing bytes.
   This proves a serialization collision, not a demonstrated end-to-end device
   authorization bypass; other plan checks still apply. Use structured versioned
   canonical encoding and bind signing-key metadata and identity deliberately.

Additional source findings: journal reopening silently ignores malformed JSONL
records; replay lookup occurs before current plan/scope binding checks; locks are
local memory rather than cross-process exclusion. Actual application/recovery is
still recorded as strings, with caller-supplied pre/postcheck booleans. HMAC is
shared-secret authentication; key distribution/rotation and operator identity
binding are not established by the helper alone. These are prototype boundaries,
not a live executor release.

## Research evidence boundaries

`run_pytorch_stgnn_held_out_experiment` creates a freshly seeded model and calls
`model.eval()` plus a no-gradient forward pass. It does not train or load a trained
checkpoint. It injects noise/faults into synthetic device features and splits
nodes by even/odd index within the same graph; that is not independent held-out
chronological episode evaluation. Entity-alignment flags are returned as literal
true values; independent alignment tests exist elsewhere, but these flags alone
are not experiment assertions. Attention extraction and inference timing are
real; predictive utility is unestablished.

The forecasting challenger remains a mean/maintenance-shift surrogate. Recording
`perf_counter()` and `foundation_model_status` truthfully labels its execution;
it does not execute foundation-model weights. Scenario model policies explicitly
remain trace scaffolds. Model provider comparisons must measure actual model
calls and outputs, not infer quality from simulated budgets. Track these under
BN-004/010/012 rather than marking A12/A14/A15's original goals complete.

## Merger standing and pending work

The routing core/history libraries are incorporated under `crates/rbmp-*` and
Bonsai has the intended service/proxy/UI/graph path. The source integration
worktree remains at `f330feb` and the original RustyBMP checkout at `e2a1189`;
new fixes live in Bonsai. Source lineage and future upstream disposition need a
clear maintenance record. Importing code is substantially accomplished; the
product acceptance and legacy retirement are not complete.

Remaining merger acceptance is BN-003/005/006/007/008/019: authenticated browser
routing workflow, default-secure deployment paths, actual second NOS, explicit
legacy-authority retirement and rollback, source-aware graph/evidence continuity,
and reproducible packaging/operational proof. The diagnostic helpers still read
the first 5,000/10,000 journal events in several paths; recent evidence can be
missed after long operation. Graph query `.take(201)` bounds Rust-side result
materialization, not database query work; its writable connection plus keyword
filter is not an engine-enforced read-only transaction, and timing out a spawned
blocking task does not interrupt the database query. Tenant IDs in observation
helpers are still supplied labels; require identity-derived provenance before
claiming tenant isolation. These points are explicitly retained in BN-005/008.

Live RPKI cache integration and BGPsec are distinct security features, deferred
under BN-018. Live graph training/PCA remains BN-010/011. Neither is the next
release blocker ahead of durable execution boundaries and routing acceptance.

## Planning reconciliation performed

Created one queue with 19 stable BN IDs, priorities, dependencies, acceptance
criteria and mappings for B/A/R and every DV3/DV4/DV4S/EV1 epic. Historical checked
tasks are not automatically reopened or promoted to verified. Remaining legacy
requirements enter explicit intake items and retain their complete original
text. DV1/DV2 root files were empty, so no tasks were inferred from them.

The archive index also referenced a missing CV6 snapshot; its entry now explicitly says the snapshot is unavailable rather than linking to a nonexistent file.

Nine old planning files were archived byte-for-byte with SHA-256 provenance;
their original locations now redirect to the common backlog. The former canonical reference was also preserved byte-for-byte and replaced with a current orientation, giving ten hashed snapshots in total. Handoff/NetOps
strategy remain design references. AGENTS, README, CANONICAL, archive index,
release-report entry points and the old operational handoff point to current
state. Archives preserve original relative-link context and obsolete procedures
as history, not actionable instructions.

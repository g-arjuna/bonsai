# BN-001 crash-recovery evidence — 8 November 2026

Scope: approval-bound executor (`src/remediation/approval.rs`) durable journal
recovery. Base commit `0b89f00`; implementation is uncommitted working-tree code.

## What was added

- `ExecutorCrashPoint` + `with_crash_point_for_testing` (`#[doc(hidden)]`):
  `abort()`s the process immediately after each durable journal record is
  synced — SIGKILL/power-loss semantics, no destructors, flock released by OS.
- Crash hooks after every persisted phase: `lock_acquired`, `execution_intent`,
  `execution_completed`, `execution_aborted`, `lock_released`.
- `abort_interrupted_execution(idempotency_key, reason, now)`: operator-driven
  reconciliation that persists `ExecutionAborted` and releases the scope lock
  only when still owned by the interrupted approval. New rejection variant
  `InterruptedExecutionNotFound`.
- `tests/bn001_executor_crash_recovery.rs`: 8 tests. Five spawn the test binary
  as a real child process killed at each persistence phase; the parent reopens
  the journal and asserts recovered state, retry/replay behavior, orphaned-lock
  handling, and post-reconciliation execution across a fresh executor instance.

## Verified recovery semantics

| Crash phase | Durable state on reopen | Recovery |
|---|---|---|
| after lock_acquired | lock held, no intent | same approval completes; other owners excluded until release |
| after intent | lock + in-flight intent | blind retry rejected (`InFlightExecutionInterrupted`); operator abort reconciles, then fresh execution succeeds |
| after completed | lock + intent + completion, no release | idempotent replay deduplicates; orphaned lock requires explicit operator release |
| after aborted | lock + intent + abort, no release | retry executes fresh (not in-flight, not completed) |
| after lock_released | full clean record set | replay deduplicates; no locks or in-flight residue |

Pre-existing coverage retained: corrupt interior line fails closed,
torn tail truncated and repaired, vanished/emptied journal fails closed,
failed disk append never replays from memory.

## Commands and results

- `cargo test --test a13_a20_change_assurance_and_executor --test bn001_executor_crash_recovery` — 20 + 8 passed, 0 failed (`regressions.log`)
- `cargo fmt --check` — clean (`format.log`)
- `git diff --check` — clean (`diff-check.log`)
- `cargo clippy --lib --tests` — 1 pre-existing `deny`-level error in untouched `src/http_server/device.rs:637` (`never_loop`); zero new warnings from changed files

## Limits

- Kill is simulated via `abort()` after `sync_data`, not a physical power cut;
  fsync/write-error injection on real storage is not exercised (ENOSPC-style
  failures are partially covered by the existing obstructed-path test).
- Reconciliation is deliberately manual: the operator must verify device state
  before retrying, since a crashed execution may have applied partial effects.
- Operational device writes remain disabled per the development contract.

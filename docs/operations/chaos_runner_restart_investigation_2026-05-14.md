# Chaos runner restart investigation — CV7 T3-1

> **Status**: investigation framework authored on Mac 2026-05-14. The actual
> findings are filled in during the Ubuntu run. Each hypothesis has an explicit
> verification command and a result slot.

## Symptom

On 2026-05-13 the chaos_runner daemon restarted **5 times in 24 hours**. Each
cycle: `runtime/chaos_log.jsonl` records `restart_marker` events with reason
`stale_pid`, the cron-based `--ensure-running` detected the missing daemon,
spawned a new one. Each restart silently destroys open parquet writer buffers,
eroding the archive accumulating for GNN training.

CV6 daily report 2026-05-13 evening: 6.3 MB in writer buffers, zero closed
parquet files past the 1-hour rotation age.

## Hypotheses to verify

### H1 — OOM kill

**Evidence to collect on Ubuntu**:
```bash
dmesg | grep -i 'killed process'
journalctl --since "2026-05-13 00:00" --until "2026-05-14 00:00" | grep -i oom
```

**Finding**: _(fill in on Ubuntu)_
- [ ] OOM events found near restart timestamps
- [ ] No OOM events — ruled out

### H2 — Python exception not caught

**Evidence to collect**:
```bash
# Inspect chaos_runner.log lines immediately preceding each restart_marker
jq -r '. | select(.event_type == "restart_marker") | .ts' runtime/chaos_log.jsonl \
  | while read -r ts; do
      echo "=== restart marker at $ts ==="
      grep -B 30 "$ts" runtime/chaos_runner.log | tail -40
      echo
    done
```

**Finding**: _(fill in)_
- [ ] Tracebacks found immediately before each restart — capture the top frame
- [ ] No tracebacks; restart cause is upstream

**Mitigation already in CV7 T2-3 rewrite**: `scripts/chaos_runner.sh` now
wraps the Python invocation with `set +e` / exit-code-capture / restart-marker
emission. A Python crash no longer kills the bash daemon; it logs and retries.

### H3 — Lab connection timeout

**Evidence to collect**:
```bash
grep -E 'TimeoutError|read timed out|connection refused' runtime/chaos_runner.log | tail -50
```

**Finding**: _(fill in)_
- [ ] Recurring timeouts to SR Linux / docker exec — needs shorter timeouts + retry
- [ ] No recurring timeouts

### H4 — inject_fault.py SR Linux candidate state poisoning (CV6 fix)

CV6 added enter-private + discard-now pattern to `python/inject_fault.py`.
This hypothesis asks: were the 2026-05-13 restarts pre-fix or post-fix?

**Evidence**:
```bash
git log --oneline python/inject_fault.py | head -10
# Restart marker timestamps versus the commit that added the discard-now fix.
jq -r '.ts' runtime/chaos_log.jsonl | head -10
```

**Finding**: _(fill in)_
- [ ] All restarts pre-fix — fix should resolve
- [ ] Some restarts post-fix — different root cause

### H5 — Cron race (concurrent --ensure-running invocations)

The 2026-05-13 cron entry fires `--ensure-running` every 5 minutes. If a fresh
daemon is mid-startup when the next cron fires, both invocations might race on
the PID file.

**Mitigation already in CV7 T3-2 rewrite**: `scripts/chaos_runner.sh
--ensure-running` now opens `runtime/chaos_runner.lock` with `flock -n`. A
second concurrent invocation exits cleanly without touching the daemon.

**Evidence post-fix**:
```bash
# After running with the rewritten script for 1h, count restart markers:
jq -r '. | select(.event_type == "restart_marker") | .reason' runtime/chaos_log.jsonl | sort | uniq -c
```

**Expected**: zero new `stale_pid` markers in a clean window. Any markers
reflect genuine daemon deaths, not cron races.

## Fix sequencing

| Fix | Where | Lands as |
|---|---|---|
| Wrap python in set+e / capture exit code | `scripts/chaos_runner.sh` | CV7 T3-2 (in this branch) |
| flock-based --ensure-running | `scripts/chaos_runner.sh` | CV7 T3-2 (in this branch) |
| SIGTERM flush of open parquet writers | `src/archive.rs` | CV7 T3-4 (in this branch) |
| Smaller SSH/docker exec timeouts | `python/inject_fault.py` | only if H3 confirmed |
| Memory bound on chaos_runner.py | `scripts/chaos_runner.py` | only if H1 confirmed |

## Regression smoke

After fixes land, run `bash scripts/smoke/smoke_chaos_stability.sh` (CV7 T3-3).
Pass criterion: zero new `restart_marker` events in 1 hour wall-clock.

## How to fill in this doc

The validation cycle script (`scripts/ops/rebuild_and_validate.sh`) runs the
checks above and writes findings into a sibling file
`docs/test_results/cv7-validation-<date>.md` so the investigation report and
the run-output stay separated. Once the H1/H2/H3 evidence is in, edit this
file's checkboxes and add a "Conclusion" section below.

## Conclusion

_(filled in after Ubuntu run; root cause + which fixes resolved it)_

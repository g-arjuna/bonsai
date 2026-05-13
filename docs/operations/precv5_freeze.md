# Pre-CV5 Freeze — CV5 T1-3

The pre-CV5 freeze is the restoration point for CV5 work. It supersedes the pre-CV2 freeze
as the "known-good" baseline. The pre-CV2 freeze is preserved independently.

## When to Create the Freeze

After `scripts/cleanup_laptop.sh` has run and the laptop is in clean state:
- Zero bonsai/chaos_runner processes
- Zero clab labs running
- Zero docker-compose stacks running
- Runtime backup dirs created (`.precv5-*` suffix)

## Creating the Freeze

```bash
# Create the freeze
bash scripts/precv5_freeze.sh

# Verify it exists
bash scripts/precv5_freeze.sh --verify
```

The freeze is created at `pre_cv5_freeze_<unix_timestamp>/` in the repo root.

## What Gets Frozen

| Item | Source | Purpose |
|------|--------|---------|
| `archive.precv5-*/` | `runtime/archive.precv5-*` | Labeled chaos Parquet from CV1–CV4 |
| `logs.precv5-*/` | `runtime/logs.precv5-*` | Daily check + chaos runner logs |
| `driver_results.precv5-*/` | `runtime/driver_results.precv5-*` | CV4 driver run artefacts |
| `bonsai.toml.snapshot` | `bonsai.toml` | Runtime config at freeze time |
| `git_state.txt` | `git log` + `git status` | Exact git position at freeze time |
| `RESTORE.md` | Generated | Step-by-step restore instructions |

## What Does NOT Get Frozen

- The LadybugDB database (on Windows; path in bonsai.toml — DB state is transient)
- Docker named volumes (Splunk, Elastic index data — use `scripts/backup_volumes.sh` if needed)
- The bonsai binary itself (re-built from source via `cargo build --release`)

## Gitignore

The `pre_cv5_freeze_*/` pattern must be in `.gitignore`. Verify:

```bash
grep "pre_cv5_freeze" .gitignore || echo "ADD pre_cv5_freeze_* to .gitignore"
```

**Reason**: `bonsai.toml.snapshot` inside the freeze may contain credentials.
The freeze dir is purely local state.

## Restore Procedure

See the auto-generated `pre_cv5_freeze_<TS>/RESTORE.md` for the exact commands.
The general pattern:

```bash
FREEZE="pre_cv5_freeze_<timestamp>"   # replace with actual dir name
REPO_ROOT=$(git rev-parse --show-toplevel)

# Restore runtime data
cp -r "$FREEZE/archive.precv5-*"         "$REPO_ROOT/runtime/archive"
cp -r "$FREEZE/logs.precv5-*"            "$REPO_ROOT/runtime/logs"
cp -r "$FREEZE/driver_results.precv5-*"  "$REPO_ROOT/runtime/driver_results"
cp    "$FREEZE/bonsai.toml.snapshot"     "$REPO_ROOT/bonsai.toml"
```

## Relationship to Pre-CV2 Freeze

The pre-CV2 freeze (if present at repo root) represents the state before CV2 began.
It remains valid as an earlier restoration point. Pre-CV5 freeze is the preferred
baseline for CV5 work — it incorporates all CV2–CV4 code improvements.

If both exist:
- Pre-CV2 freeze: oldest stable baseline; use only for extreme rollback
- Pre-CV5 freeze: current stable baseline; use for CV5 recovery

#!/usr/bin/env bash
# CV5 Sprint 1 — T1-3
# Pre-CV5 freeze: snapshot the cleaned-up laptop state as a known-good restoration point.
#
# Run AFTER cleanup_laptop.sh has been executed and verified clean.
# Creates pre_cv5_freeze_<timestamp>/ at repo root containing:
#   - runtime backup dirs (archive.precv5-*, logs.precv5-*, driver_results.precv5-*)
#   - current bonsai.toml (gitignored config)
#   - git log summary
#   - RESTORE.md with step-by-step restore instructions
#
# Usage:
#   bash scripts/precv5_freeze.sh            # create freeze
#   bash scripts/precv5_freeze.sh --verify   # check if a freeze already exists
#   bash scripts/precv5_freeze.sh --help

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TS="$(date +%s)"
FREEZE_DIR="$REPO_ROOT/pre_cv5_freeze_${TS}"

RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; NC='\033[0m'
info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
step()  { echo -e "\n${YELLOW}=== $* ===${NC}"; }

case "${1:-}" in
    --verify)
        EXISTING="$(ls -d "$REPO_ROOT"/pre_cv5_freeze_* 2>/dev/null || true)"
        if [[ -n "$EXISTING" ]]; then
            info "Existing freeze(s) found:"
            echo "$EXISTING"
        else
            warn "No pre_cv5_freeze_* directory found at repo root."
        fi
        exit 0
        ;;
    --help|-h)
        echo "Usage: $0 [--verify]"
        echo "  (no args)  create pre-CV5 freeze at pre_cv5_freeze_<timestamp>/"
        echo "  --verify   list existing freeze directories"
        exit 0
        ;;
    "") : ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
esac

# ── Preflight: ensure cleanup has been run ────────────────────────────────────
step "Preflight checks"

RUNTIME="$REPO_ROOT/runtime"
ACTIVE_DIRS=(archive logs driver_results)
PROBLEMS=0
for d in "${ACTIVE_DIRS[@]}"; do
    if [[ -d "$RUNTIME/$d" ]]; then
        warn "runtime/$d still exists — run scripts/cleanup_laptop.sh first, then re-run this script."
        PROBLEMS=$((PROBLEMS + 1))
    fi
done

ACTIVE_PROCS="$(ps aux | grep -E "target/release/bonsai|chaos_runner" | grep -v grep || true)"
if [[ -n "$ACTIVE_PROCS" ]]; then
    warn "bonsai/chaos_runner still running — stop them before freezing."
    PROBLEMS=$((PROBLEMS + 1))
fi

if [[ "$PROBLEMS" -gt 0 ]]; then
    echo ""
    echo "Fix the above before creating the freeze. The freeze captures a clean state."
    echo "Run:  bash scripts/cleanup_laptop.sh && bash scripts/precv5_freeze.sh"
    exit 1
fi

info "Preflight passed."

# ── Create freeze directory ───────────────────────────────────────────────────
step "Creating freeze at $FREEZE_DIR"
mkdir -p "$FREEZE_DIR"

# ── Copy runtime backups ──────────────────────────────────────────────────────
step "Copying runtime backups"
BACKUP_COUNT=0
for pattern in archive.precv5-\* logs.precv5-\* driver_results.precv5-\* chaos_log.jsonl.precv5-\* bonsai.db.precv5-\*; do
    # Use find to handle glob safely
    while IFS= read -r item; do
        if [[ -e "$item" ]]; then
            cp -r "$item" "$FREEZE_DIR/"
            info "Copied: $(basename "$item")"
            BACKUP_COUNT=$((BACKUP_COUNT + 1))
        fi
    done < <(find "$RUNTIME" -maxdepth 1 -name "$(basename "$pattern")" 2>/dev/null || true)
done

if [[ "$BACKUP_COUNT" -eq 0 ]]; then
    warn "No precv5 backup items found in runtime/ — freeze will be thin."
    warn "If cleanup_laptop.sh was run without any active runtime data, this is expected."
fi

# ── Copy bonsai.toml (gitignored runtime config) ──────────────────────────────
step "Copying runtime config"
if [[ -f "$REPO_ROOT/bonsai.toml" ]]; then
    cp "$REPO_ROOT/bonsai.toml" "$FREEZE_DIR/bonsai.toml.snapshot"
    info "Copied bonsai.toml -> bonsai.toml.snapshot"
else
    warn "bonsai.toml not found (may not be configured yet)."
fi

# ── Git state snapshot ─────────────────────────────────────────────────────────
step "Capturing git state"
{
    echo "# Pre-CV5 Freeze — Git State"
    echo "Timestamp: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "Unix TS: $TS"
    echo ""
    echo "## Branch"
    git -C "$REPO_ROOT" branch --show-current
    echo ""
    echo "## HEAD commit"
    git -C "$REPO_ROOT" log -1 --oneline
    echo ""
    echo "## Recent log (last 10)"
    git -C "$REPO_ROOT" log --oneline -10
    echo ""
    echo "## Status"
    git -C "$REPO_ROOT" status --short
} > "$FREEZE_DIR/git_state.txt"
info "Captured git state -> git_state.txt"

# ── Write RESTORE.md ─────────────────────────────────────────────────────────
step "Writing RESTORE.md"
cat > "$FREEZE_DIR/RESTORE.md" << EOF
# Pre-CV5 Freeze — Restore Instructions

Created: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
Freeze dir: pre_cv5_freeze_${TS}/

## What's in this freeze

- \`archive.precv5-${TS}/\` — chaos archive Parquet files from before CV5 cleanup (if any)
- \`logs.precv5-${TS}/\` — daily check and chaos runner logs from before cleanup
- \`driver_results.precv5-${TS}/\` — driver result files from before cleanup
- \`bonsai.toml.snapshot\` — runtime config snapshot (gitignored, may contain credentials)
- \`git_state.txt\` — git branch/log/status at freeze time

## Restore procedure

### Restore runtime data (if something went wrong in CV5 and you want to roll back)

\`\`\`bash
REPO_ROOT=\$(git rev-parse --show-toplevel)
FREEZE="pre_cv5_freeze_${TS}"

# Restore runtime dirs
[[ -d "\$FREEZE/archive.precv5-${TS}" ]] && cp -r "\$FREEZE/archive.precv5-${TS}" "\$REPO_ROOT/runtime/archive"
[[ -d "\$FREEZE/logs.precv5-${TS}" ]]    && cp -r "\$FREEZE/logs.precv5-${TS}" "\$REPO_ROOT/runtime/logs"
[[ -d "\$FREEZE/driver_results.precv5-${TS}" ]] && cp -r "\$FREEZE/driver_results.precv5-${TS}" "\$REPO_ROOT/runtime/driver_results"

# Restore config
[[ -f "\$FREEZE/bonsai.toml.snapshot" ]] && cp "\$FREEZE/bonsai.toml.snapshot" "\$REPO_ROOT/bonsai.toml"
\`\`\`

### Git rollback (if CV5 code changes need to be reverted)

\`\`\`bash
# See git_state.txt for the HEAD commit at freeze time
HEAD_COMMIT=\$(grep "HEAD commit" pre_cv5_freeze_${TS}/git_state.txt | awk '{print \$NF}')
git log --oneline | head -5   # confirm current state
git reset --hard \$HEAD_COMMIT  # only if needed; confirm with user first
\`\`\`

## Notes

- The pre-CV2 freeze (if present) is a separate and independent restoration point.
  This freeze supersedes it as the "known-good" baseline for CV5 work.
- \`bonsai.toml.snapshot\` contains plaintext credentials. Do not commit this file.
  The freeze directory itself is gitignored (pre_cv5_freeze_* pattern).
- Archive Parquet files in this freeze are the labeled training data accumulated through CV4.
  Do not delete them without reviewing their content first.
EOF
info "Wrote RESTORE.md"

# ── Verify freeze contents ────────────────────────────────────────────────────
step "Freeze summary"
echo ""
echo "Freeze directory: $FREEZE_DIR"
ls -lah "$FREEZE_DIR/"
echo ""
FREEZE_SIZE="$(du -sh "$FREEZE_DIR" | cut -f1)"
info "Total freeze size: $FREEZE_SIZE"
info "Pre-CV5 freeze complete. See $FREEZE_DIR/RESTORE.md for restore instructions."
info "Freeze dir is gitignored (pre_cv5_freeze_* pattern — add to .gitignore if missing)."

#!/usr/bin/env bash
# scripts/cloud/daily_sync.sh — Daily archive snapshot sync to GitHub.
#
# Compresses yesterday's archive partition + chaos CSVs + memory profile +
# a system snapshot, then pushes to a dedicated GitHub branch so the operator
# can pull anytime without SSH access.
#
# Runs on the cloud VM via cron (see deploy.sh Step 11).
# Can also be triggered manually.
#
# Usage:
#   bash scripts/cloud/daily_sync.sh
#   bash scripts/cloud/daily_sync.sh --dry-run   # show what would be uploaded
#   bash scripts/cloud/daily_sync.sh --force      # re-sync today even if done
#
# Requirements:
#   - GitHub personal access token (classic or fine-grained with repo push scope)
#   - GITHUB_TOKEN env var (add to ~/.bashrc or set in crontab env)
#   - git remote "origin" set to the bonsai repo
#
# Output: branch sync/cloud-spike/<YYYYMMDD> on origin

set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-/opt/bonsai}"
ARCHIVE_MOUNT="${ARCHIVE_MOUNT:-/mnt/bonsai-archive}"
GITHUB_TOKEN="${GITHUB_TOKEN:-}"
SYNC_BRANCH_PREFIX="sync/cloud-spike"
SNAPSHOTS_DIR="$ARCHIVE_MOUNT/snapshots"

DRY_RUN=false
FORCE=false
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --force)   FORCE=true ;;
        *) echo "Unknown arg: $arg" >&2; exit 1 ;;
    esac
done

_log() { echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $*"; }
_run() { "$DRY_RUN" && echo "[DRY-RUN] $*" || "$@"; }

YESTERDAY=$(date -u -d "yesterday" '+%Y-%m-%d' 2>/dev/null || date -u -v-1d '+%Y-%m-%d')
SNAPSHOT_NAME="snapshot-$YESTERDAY"
SNAPSHOT_DIR="$SNAPSHOTS_DIR/$SNAPSHOT_NAME"
SNAPSHOT_TAR="$SNAPSHOTS_DIR/$SNAPSHOT_NAME.tar.zst"
BRANCH="$SYNC_BRANCH_PREFIX/$YESTERDAY"

_log "=== Daily sync for $YESTERDAY ==="

# ── Skip check ────────────────────────────────────────────────────────────────

DONE_MARKER="$SNAPSHOTS_DIR/.synced-$YESTERDAY"
if [[ -f "$DONE_MARKER" ]] && ! "$FORCE"; then
    _log "Already synced for $YESTERDAY (use --force to re-sync)"
    exit 0
fi

# ── Collect snapshot artifacts ────────────────────────────────────────────────

mkdir -p "$SNAPSHOT_DIR"

# 1. Yesterday's Parquet archive files
ARCHIVE_PARQUET_DIR="$ARCHIVE_MOUNT/archive"
if [[ -d "$ARCHIVE_PARQUET_DIR" ]]; then
    PARQUET_COUNT=$(find "$ARCHIVE_PARQUET_DIR" -name "*.parquet" \
        -newer "$ARCHIVE_PARQUET_DIR" -mtime -2 2>/dev/null | wc -l)
    _log "  Parquet files (last 48h): $PARQUET_COUNT"
    find "$ARCHIVE_PARQUET_DIR" -name "*.parquet" -mtime -2 \
        -exec cp {} "$SNAPSHOT_DIR/" \; 2>/dev/null || true
fi

# 2. Chaos run CSVs
if [[ -d "$INSTALL_DIR/chaos_runs" ]]; then
    cp -r "$INSTALL_DIR/chaos_runs" "$SNAPSHOT_DIR/chaos_runs" 2>/dev/null || true
fi

# 3. Memory profile (from /api/operations)
if curl -sf "http://localhost:3000/api/operations" > "$SNAPSHOT_DIR/operations.json" 2>/dev/null; then
    _log "  Captured operations snapshot"
fi

# 4. System snapshot (CPU, RAM, disk, process list)
{
    echo "=== System snapshot: $YESTERDAY ==="
    echo ""
    echo "--- uptime ---"
    uptime
    echo ""
    echo "--- free -h ---"
    free -h
    echo ""
    echo "--- df -h ---"
    df -h
    echo ""
    echo "--- top (1 iteration, batch) ---"
    top -bn1 | head -20
    echo ""
    echo "--- bonsai service status ---"
    systemctl status bonsai --no-pager 2>/dev/null || true
    echo ""
    echo "--- containerlab inspect ---"
    sudo containerlab inspect 2>/dev/null || true
    echo ""
    echo "--- chaos runner status ---"
    bash "$INSTALL_DIR/scripts/chaos_runner.sh" --status 2>/dev/null || true
} > "$SNAPSHOT_DIR/system_snapshot.txt" 2>&1 || true

# 5. Detection baselines report
if command -v python3 &>/dev/null && [[ -f "$INSTALL_DIR/scripts/compute_detection_baselines.py" ]]; then
    python3 "$INSTALL_DIR/scripts/compute_detection_baselines.py" \
        --chaos-dir "$INSTALL_DIR/chaos_runs" \
        --archive-dir "$ARCHIVE_MOUNT/archive" \
        --dry-run > "$SNAPSHOT_DIR/detection_baselines.md" 2>/dev/null || true
fi

# 6. Archive integrity result
bash "$INSTALL_DIR/scripts/verify_archive.sh" "$ARCHIVE_MOUNT/archive" --json \
    > "$SNAPSHOT_DIR/archive_verify.json" 2>/dev/null || true

# 7. Bonsai build info
"$INSTALL_DIR/target/release/bonsai" --version > "$SNAPSHOT_DIR/bonsai_version.txt" 2>/dev/null || true
git -C "$INSTALL_DIR" rev-parse HEAD > "$SNAPSHOT_DIR/git_sha.txt" 2>/dev/null || true

_log "  Snapshot dir assembled: $SNAPSHOT_DIR"
ls "$SNAPSHOT_DIR" | _log "  Files: $(ls "$SNAPSHOT_DIR" | tr '\n' ' ')"

# ── Compress snapshot ─────────────────────────────────────────────────────────

_log "  Compressing to $SNAPSHOT_TAR..."
_run tar --zstd -cf "$SNAPSHOT_TAR" -C "$SNAPSHOTS_DIR" "$SNAPSHOT_NAME"

TARSIZE_KB=$(du -k "$SNAPSHOT_TAR" 2>/dev/null | cut -f1 || echo 0)
_log "  Compressed size: ${TARSIZE_KB} KiB"

# ── Push to GitHub branch ─────────────────────────────────────────────────────

if [[ -z "$GITHUB_TOKEN" ]]; then
    _log "WARN: GITHUB_TOKEN not set — skipping GitHub push"
    _log "  Export GITHUB_TOKEN in your shell or add to crontab environment"
    _log "  Snapshot available locally: $SNAPSHOT_TAR"
    touch "$DONE_MARKER"
    exit 0
fi

_log "  Pushing to branch: $BRANCH"

# Configure git credentials
_run git -C "$INSTALL_DIR" config credential.helper \
    "!f() { echo \"username=x-token\"; echo \"password=$GITHUB_TOKEN\"; }; f"

# Create or reset the branch (orphan so it doesn't balloon the repo history)
SYNC_WORK_DIR="$SNAPSHOTS_DIR/.git-sync-work"
rm -rf "$SYNC_WORK_DIR"
_run mkdir -p "$SYNC_WORK_DIR"

# Clone only the target branch (shallow) into work dir
REMOTE_URL=$(git -C "$INSTALL_DIR" remote get-url origin)
if ! _run git clone --depth 1 --branch "$BRANCH" \
    "https://x-token:$GITHUB_TOKEN@${REMOTE_URL#https://}" \
    "$SYNC_WORK_DIR" 2>/dev/null; then
    # Branch doesn't exist yet — init empty repo
    _run git -C "$SYNC_WORK_DIR" init -q
    _run git -C "$SYNC_WORK_DIR" remote add origin \
        "https://x-token:$GITHUB_TOKEN@${REMOTE_URL#https://}"
fi

# Copy tarball into work dir
_run cp "$SNAPSHOT_TAR" "$SYNC_WORK_DIR/"

# Write a minimal README for the branch
cat > "$SYNC_WORK_DIR/README.md" <<EOF
# Bonsai cloud spike archive — $YESTERDAY

Branch: \`$BRANCH\`
Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')

## Contents

Each \`snapshot-<date>.tar.zst\` contains:
- Parquet telemetry archive (last 48h)
- Chaos run CSVs
- Detection baseline report (Markdown)
- Operations JSON snapshot
- System resource snapshot
- Archive integrity result

## Pull a snapshot

\`\`\`bash
git fetch origin $BRANCH
git checkout $BRANCH
zstd -d snapshot-<date>.tar.zst
tar -xf snapshot-<date>.tar
\`\`\`
EOF

_run git -C "$SYNC_WORK_DIR" add -A
_run git -C "$SYNC_WORK_DIR" \
    -c user.name="bonsai-cloud-sync" \
    -c user.email="noreply@bonsai" \
    commit -m "chore: daily archive sync $YESTERDAY" || true
_run git -C "$SYNC_WORK_DIR" push --force origin "HEAD:$BRANCH"

# Cleanup credentials helper
_run git -C "$INSTALL_DIR" config --unset credential.helper || true
rm -rf "$SYNC_WORK_DIR"

touch "$DONE_MARKER"
_log "  Sync complete: $BRANCH"
_log ""
_log "Pull on laptop:"
_log "  git fetch origin $BRANCH && git checkout $BRANCH"

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
API_BASE="${API_BASE:-http://localhost:3000}"
ENV_FILE="${CLOUD_SYNC_ENV_FILE:-$INSTALL_DIR/.cloud_sync.env}"
if [[ -f "$ENV_FILE" ]]; then
    # shellcheck source=/dev/null
    source "$ENV_FILE"
fi

# Auto-source GITHUB_TOKEN from known env files when not already set.
# Cron and non-interactive subprocesses don't inherit interactive shell exports.
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    for _env_file in "$HOME/.bonsai.env" "/opt/bonsai/instance.env"; do
        if [[ -f "$_env_file" ]]; then
            # shellcheck source=/dev/null
            . "$_env_file"
            break
        fi
    done
fi

GITHUB_TOKEN="${GITHUB_TOKEN:-}"
SYNC_BRANCH_PREFIX="sync/cloud-spike"

# ── Laptop vs cloud detection ─────────────────────────────────────────────────
# Cloud has an OCI block mount at /mnt/bonsai-archive.
# Laptop parquet files live inside the Docker named volume (not on the host FS).
# When the mount is absent, stage into a local directory and pull parquet from
# the running bonsai container via docker exec.
LAPTOP_MODE=false
if [[ ! -d "$ARCHIVE_MOUNT" ]]; then
    LAPTOP_MODE=true
    ARCHIVE_MOUNT="$INSTALL_DIR/runtime/sync-staging"
    mkdir -p "$ARCHIVE_MOUNT/archive"
fi

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
_die() { _log "ERROR: $*"; exit 1; }
_maybe_sudo() {
    if command -v sudo >/dev/null 2>&1; then
        sudo -n "$@" 2>/dev/null || return 1
    else
        "$@" 2>/dev/null || return 1
    fi
}

# ── Preflight: GITHUB_TOKEN required ─────────────────────────────────────────
if [[ -z "$GITHUB_TOKEN" ]] && ! "$DRY_RUN"; then
    _die "GITHUB_TOKEN is not set. Cannot push to GitHub.
  Configure it with one of:
    1. export GITHUB_TOKEN=<token> in ~/.bashrc (then re-login or source ~/.bashrc)
    2. Add GITHUB_TOKEN=<token> to $ENV_FILE
    3. Add GITHUB_TOKEN=<token> as a CRON_ENV line in /etc/cron.d/bonsai
  Token needs: repo scope (classic PAT) or Contents: write (fine-grained)."
fi

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
if $LAPTOP_MODE; then
    # On laptop, parquet lives inside the Docker named volume — not on the host FS.
    # Pull it out via docker exec from the running bonsai container.
    if [[ -z "${BONSAI_CONTAINER:-}" ]]; then
        if docker inspect bonsai-bonsai-core-1 &>/dev/null 2>&1; then
            BONSAI_CONTAINER="bonsai-bonsai-core-1"
        else
            BONSAI_CONTAINER="bonsai-bonsai-lab-dc-1"
        fi
    fi
    if docker inspect "$BONSAI_CONTAINER" &>/dev/null 2>&1; then
        _log "  Laptop mode: extracting parquet from Docker volume ($BONSAI_CONTAINER)"
        # Container path: /app/runtime/archive/*.parquet
        # strip-components=3 removes app/runtime/archive → files land in ARCHIVE_PARQUET_DIR
        # tar out /app/runtime/archive; strip-components=3 drops app/runtime/archive prefix
        docker exec "$BONSAI_CONTAINER" tar -cf - /app/runtime/archive 2>/dev/null \
            | tar -xf - -C "$ARCHIVE_PARQUET_DIR" --strip-components=3 2>/dev/null || true
        PARQUET_COUNT=$(find "$ARCHIVE_PARQUET_DIR" -name "*.parquet" 2>/dev/null | wc -l)
        _log "  Parquet files extracted: $PARQUET_COUNT"
    else
        _log "  Laptop mode: container $BONSAI_CONTAINER not running — skipping parquet"
        PARQUET_COUNT=0
    fi
    find "$ARCHIVE_PARQUET_DIR" -name "*.parquet" \
        -exec cp {} "$SNAPSHOT_DIR/" \; 2>/dev/null || true
elif [[ -d "$ARCHIVE_PARQUET_DIR" ]]; then
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
if curl -sf "$API_BASE/api/operations" > "$SNAPSHOT_DIR/operations.json" 2>/dev/null; then
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
    if [[ -f "$INSTALL_DIR/lab/cloud-dc-6node.yml" ]]; then
        _maybe_sudo containerlab inspect -t "$INSTALL_DIR/lab/cloud-dc-6node.yml" || \
            containerlab inspect -t "$INSTALL_DIR/lab/cloud-dc-6node.yml" 2>/dev/null || \
            echo "containerlab inspect unavailable without sudo"
    else
        _maybe_sudo containerlab inspect || \
            containerlab inspect 2>/dev/null || \
            echo "containerlab inspect unavailable without sudo"
    fi
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
# Guard this call because some local builds can block while trying to start the
# full runtime instead of exiting immediately on --version.
if command -v timeout >/dev/null 2>&1; then
    timeout 5 "$INSTALL_DIR/target/release/bonsai" --version > "$SNAPSHOT_DIR/bonsai_version.txt" 2>/dev/null || true
else
    "$INSTALL_DIR/target/release/bonsai" --version > "$SNAPSHOT_DIR/bonsai_version.txt" 2>/dev/null || true
fi
git -C "$INSTALL_DIR" rev-parse HEAD > "$SNAPSHOT_DIR/git_sha.txt" 2>/dev/null || true

_log "  Snapshot dir assembled: $SNAPSHOT_DIR"
ls "$SNAPSHOT_DIR" | _log "  Files: $(ls "$SNAPSHOT_DIR" | tr '\n' ' ')"

# ── Compress snapshot ─────────────────────────────────────────────────────────

_log "  Compressing to $SNAPSHOT_TAR..."
if tar --help 2>/dev/null | grep -q -- "--zstd"; then
    _run tar --zstd -cf "$SNAPSHOT_TAR" -C "$SNAPSHOTS_DIR" "$SNAPSHOT_NAME"
else
    command -v zstd >/dev/null 2>&1 || _die "zstd not found; install zstd or use a tar build with --zstd"
    "$DRY_RUN" && echo "[DRY-RUN] tar -cf - -C $SNAPSHOTS_DIR $SNAPSHOT_NAME | zstd -T0 -f -o $SNAPSHOT_TAR" || \
        tar -cf - -C "$SNAPSHOTS_DIR" "$SNAPSHOT_NAME" | zstd -T0 -f -o "$SNAPSHOT_TAR"
fi

TARSIZE_KB=$(du -k "$SNAPSHOT_TAR" 2>/dev/null | cut -f1 || echo 0)
_log "  Compressed size: ${TARSIZE_KB} KiB"

if "$DRY_RUN"; then
    if [[ -z "$GITHUB_TOKEN" ]]; then
        _log "WARN: GITHUB_TOKEN not set — skipping GitHub push (dry-run mode)"
    else
        _log "DRY-RUN: GitHub push would target branch $BRANCH"
    fi
    _log "  Snapshot available locally: $SNAPSHOT_TAR"
    exit 0
fi

# ── Push to GitHub branch ─────────────────────────────────────────────────────

# GITHUB_TOKEN guard already enforced at startup.

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

PUSH_LOG="$(mktemp)"
if ! _run git -C "$SYNC_WORK_DIR" push --force origin "HEAD:$BRANCH" 2>"$PUSH_LOG"; then
    PUSH_ERR="$(cat "$PUSH_LOG")"
    rm -f "$PUSH_LOG"
    _log "PUSH FAILED to $BRANCH"
    _log "--- git push stderr ---"
    echo "$PUSH_ERR" | while IFS= read -r line; do _log "  $line"; done
    _log "---"
    _log "Diagnostics:"
    _log "  REMOTE_URL  = $REMOTE_URL"
    _log "  BRANCH      = $BRANCH"
    _log "  TOKEN_SET   = $([ -n "$GITHUB_TOKEN" ] && echo yes || echo no)"
    _log "  TOKEN_LEN   = ${#GITHUB_TOKEN}"
    _log "  git version = $(git --version 2>/dev/null || echo unknown)"
    _run git -C "$INSTALL_DIR" config --unset credential.helper || true
    rm -rf "$SYNC_WORK_DIR"
    _die "GitHub push failed — see diagnostics above. Snapshot preserved at: $SNAPSHOT_TAR"
fi
rm -f "$PUSH_LOG"

# Cleanup credentials helper
_run git -C "$INSTALL_DIR" config --unset credential.helper || true
rm -rf "$SYNC_WORK_DIR"

touch "$DONE_MARKER"
_log "  Sync complete: $BRANCH"
_log ""
_log "Pull on laptop:"
_log "  git fetch origin $BRANCH && git checkout $BRANCH"

#!/usr/bin/env bash
# Idempotent cron installer for bonsai cloud VM.
#
# Installs two crontab entries:
#   02:00 UTC — daily_sync.sh   (archive snapshot → GitHub branch)
#   02:30 UTC — bv5_daily_check.sh  (driver check against live bonsai)
#
# Usage:
#   bash scripts/cloud/install_cron.sh            # install
#   bash scripts/cloud/install_cron.sh --remove   # remove both entries
#   bash scripts/cloud/install_cron.sh --list     # show current crontab

set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-/opt/bonsai}"
ARCHIVE_MOUNT="${ARCHIVE_MOUNT:-/mnt/bonsai-archive}"
CRON_TAG_SYNC="bonsai-cloud-sync"
CRON_TAG_CHECK="bonsai-cloud-check"

# Detect environment: cloud has /mnt/bonsai-archive; laptop uses repo runtime dir.
# LAB_SCOPE can be overridden: LAB_SCOPE=cloud-dc bash install_cron.sh
if [[ -d "$ARCHIVE_MOUNT" ]] || [[ "$INSTALL_DIR" == "/opt/bonsai" ]]; then
    LAB_SCOPE="${LAB_SCOPE:-cloud-dc}"
    LOG_DIR="$ARCHIVE_MOUNT/logs"
    # Cloud: daily_sync.sh auto-sources GITHUB_TOKEN from instance.env internally.
    SYNC_LINE="0 2 * * * bash $INSTALL_DIR/scripts/cloud/daily_sync.sh >> $LOG_DIR/daily_sync.log 2>&1  # $CRON_TAG_SYNC"
else
    LAB_SCOPE="${LAB_SCOPE:-dc}"
    LOG_DIR="$INSTALL_DIR/runtime/logs"
    # Laptop: target the distributed Docker deployment explicitly.
    SYNC_LINE="0 2 * * * . \$HOME/.bonsai.env 2>/dev/null || true; API_BASE=http://127.0.0.1:3100 BONSAI_CONTAINER=bonsai-bonsai-core-1 bash $INSTALL_DIR/scripts/cloud/daily_sync.sh >> $LOG_DIR/daily_sync.log 2>&1  # $CRON_TAG_SYNC"
fi

# daily_check_push.sh: runs bv5_daily_check.sh then commits+pushes the report to GitHub.
# It auto-sources GITHUB_TOKEN from ~/.bonsai.env or /opt/bonsai/instance.env internally.
CHECK_LINE="30 2 * * * LAB_SCOPE=$LAB_SCOPE bash $INSTALL_DIR/scripts/cloud/daily_check_push.sh >> $LOG_DIR/daily_check.log 2>&1  # $CRON_TAG_CHECK"

mkdir -p "$LOG_DIR"

_usage() {
    echo "Usage: $0 [--remove|--list]"
    exit 0
}

case "${1:-}" in
    --list)
        crontab -l 2>/dev/null || echo "(no crontab)"
        exit 0
        ;;
    --remove)
        EXISTING="$(crontab -l 2>/dev/null || true)"
        FILTERED="$(echo "$EXISTING" | grep -v "# $CRON_TAG_SYNC" | grep -v "# $CRON_TAG_CHECK" || true)"
        if [[ "$EXISTING" == "$FILTERED" ]]; then
            echo "No bonsai cloud cron entries found — nothing to remove."
        else
            echo "$FILTERED" | crontab -
            echo "Removed bonsai cloud cron entries."
        fi
        exit 0
        ;;
    --help|-h) _usage ;;
    "") : ;;
    *) echo "Unknown argument: $1" >&2; _usage ;;
esac

# Validate GITHUB_TOKEN is set before installing
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
    echo "WARNING: GITHUB_TOKEN is not set in the current environment."
    echo "  The daily_sync cron job will fail at run time until it is configured."
    echo "  Add it to /etc/cron.d/bonsai or your crontab with:"
    echo "    GITHUB_TOKEN=<token>"
    echo ""
fi

EXISTING="$(crontab -l 2>/dev/null || true)"

ADDED=0
if echo "$EXISTING" | grep -q "# $CRON_TAG_SYNC"; then
    echo "daily_sync cron already installed — skipping."
else
    EXISTING="${EXISTING}"$'\n'"${SYNC_LINE}"
    ADDED=$((ADDED + 1))
fi

if echo "$EXISTING" | grep -q "# $CRON_TAG_CHECK"; then
    echo "daily_check cron already installed — skipping."
else
    EXISTING="${EXISTING}"$'\n'"${CHECK_LINE}"
    ADDED=$((ADDED + 1))
fi

if [[ "$ADDED" -gt 0 ]]; then
    echo "$EXISTING" | grep -v '^$' | crontab -
    echo "Installed $ADDED cron entry(s):"
    [[ "$ADDED" -gt 0 ]] && echo "  $SYNC_LINE"
    [[ "$ADDED" -gt 1 ]] && echo "  $CHECK_LINE"
    echo ""
    echo "Logs: $LOG_DIR/"
fi

echo "Run '$0 --list' to verify, '--remove' to uninstall."

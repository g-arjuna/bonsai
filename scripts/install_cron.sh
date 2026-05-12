#!/usr/bin/env bash
# Idempotent cron installer for bonsai daily check (laptop / developer workstation).
#
# Installs a single crontab entry that runs bv5_daily_check.sh at 06:00 local
# time every day and appends stdout+stderr to $REPO_ROOT/logs/daily_check.log.
#
# Usage:
#   bash scripts/install_cron.sh            # install
#   bash scripts/install_cron.sh --remove   # remove the entry
#   bash scripts/install_cron.sh --list     # show current crontab

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/bv5_daily_check.sh"
LOG_DIR="$REPO_ROOT/logs"
LOG_FILE="$LOG_DIR/daily_check.log"
CRON_TAG="bonsai-daily-check"
CRON_SCHEDULE="0 6 * * *"
CRON_CMD="bash $SCRIPT >> $LOG_FILE 2>&1"
CRON_LINE="$CRON_SCHEDULE $CRON_CMD  # $CRON_TAG"

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
        FILTERED="$(echo "$EXISTING" | grep -v "# $CRON_TAG" || true)"
        if [[ "$EXISTING" == "$FILTERED" ]]; then
            echo "No bonsai daily-check cron entry found — nothing to remove."
        else
            echo "$FILTERED" | crontab -
            echo "Removed bonsai daily-check cron entry."
        fi
        exit 0
        ;;
    --help|-h) _usage ;;
    "") : ;;
    *) echo "Unknown argument: $1" >&2; _usage ;;
esac

# Install (idempotent)
EXISTING="$(crontab -l 2>/dev/null || true)"
if echo "$EXISTING" | grep -q "# $CRON_TAG"; then
    echo "Cron entry already installed:"
    echo "$EXISTING" | grep "# $CRON_TAG"
    exit 0
fi

{ echo "$EXISTING"; echo "$CRON_LINE"; } | grep -v '^$' | crontab -

echo "Installed cron entry:"
echo "  $CRON_LINE"
echo ""
echo "Log file: $LOG_FILE"
echo "Run 'bash $0 --list' to verify, '--remove' to uninstall."

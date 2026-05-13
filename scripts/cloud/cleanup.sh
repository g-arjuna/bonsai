#!/usr/bin/env bash
# CV5 Sprint 1 — T1-2
# Cloud VM cleanup: tear everything bonsai-related down to zero before CV5 rebuild.
#
# Run ON THE CLOUD VM (SSH in first). Assumes bonsai is installed at INSTALL_DIR.
# Does NOT delete backup dirs — moves runtime state to dated backups for review.
#
# Usage:
#   bash scripts/cloud/cleanup.sh            # full teardown + backup
#   bash scripts/cloud/cleanup.sh --verify   # verify-only (no changes)
#   bash scripts/cloud/cleanup.sh --help

set -euo pipefail

INSTALL_DIR="${INSTALL_DIR:-/opt/bonsai}"
TS="$(date +%s)"

RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; NC='\033[0m'
info()  { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
error() { echo -e "${RED}[ERR ]${NC} $*" >&2; }
step()  { echo -e "\n${YELLOW}=== $* ===${NC}"; }

VERIFY_ONLY=false

case "${1:-}" in
    --verify) VERIFY_ONLY=true ;;
    --help|-h)
        echo "Usage: $0 [--verify]"
        echo "  (no args)  full teardown + back up runtime dirs"
        echo "  --verify   check current state, make no changes"
        echo ""
        echo "Environment:"
        echo "  INSTALL_DIR  bonsai install path (default: /opt/bonsai)"
        exit 0
        ;;
    "") : ;;
    *) error "Unknown argument: $1"; exit 1 ;;
esac

if [[ ! -d "$INSTALL_DIR" ]]; then
    warn "INSTALL_DIR=$INSTALL_DIR does not exist. Nothing to clean up?"
    warn "If bonsai is installed elsewhere, set INSTALL_DIR env var."
    if $VERIFY_ONLY; then exit 0; fi
fi

RUNTIME="$INSTALL_DIR/runtime"

# ── 1. Cron removal ──────────────────────────────────────────────────────────
step "Cron removal"
if $VERIFY_ONLY; then
    CRON_OUT="$(crontab -l 2>/dev/null || true)"
    if echo "$CRON_OUT" | grep -q "bonsai"; then
        warn "Active bonsai cron entries found:"
        echo "$CRON_OUT" | grep "bonsai"
    else
        info "No bonsai cron entries found."
    fi
else
    INSTALL_DIR="$INSTALL_DIR" bash "$INSTALL_DIR/scripts/cloud/install_cron.sh" --remove 2>/dev/null || true
    info "Cron removal attempted."
fi

# ── 2. Stop bonsai service / process ────────────────────────────────────────
step "Stop bonsai processes"
if $VERIFY_ONLY; then
    PROCS="$(ps aux | grep -E "bonsai|chaos_runner" | grep -v grep || true)"
    if [[ -n "$PROCS" ]]; then
        warn "Running bonsai/chaos_runner processes:"
        echo "$PROCS"
    else
        info "No bonsai/chaos_runner processes running."
    fi
else
    # Stop via systemctl first (clean exit — Restart=on-failure does NOT trigger on clean stop).
    # DO NOT kill -9 before systemctl stop: signal 9 looks like a crash and triggers restart.
    sudo systemctl stop bonsai 2>/dev/null && info "Stopped bonsai systemd service." || true
    sudo systemctl disable bonsai 2>/dev/null || true
    sleep 2  # let any in-progress RestartSec=10s cycle settle
    sudo systemctl stop bonsai 2>/dev/null || true  # second stop wins if mid-restart

    # Kill chaos_runner by PID (pkill -f can hit OCI pmie daemon or SSH session helpers)
    CHAOS_PIDS="$(ps aux | grep -E "chaos_runner\.sh|chaos_runner\.py" | grep -v grep | awk '{print $2}' | tr '\n' ' ')"
    if [[ -n "$CHAOS_PIDS" ]]; then
        # shellcheck disable=SC2086
        kill -9 $CHAOS_PIDS 2>/dev/null && info "Killed chaos_runner PIDs: $CHAOS_PIDS" || true
    fi
    rm -f "$RUNTIME/chaos_runner.pid" 2>/dev/null || true
    info "Process cleanup done."
fi

# ── 3. Destroy ContainerLab labs ────────────────────────────────────────────
step "Destroy ContainerLab labs"

CLOUD_CLAB_TOPOLOGIES=(
    "$INSTALL_DIR/lab/cloud-dc-6node.yml"
    "$INSTALL_DIR/lab/sp/sp-mpls-srte.clab.yml"
)

if $VERIFY_ONLY; then
    CLAB_STATUS="$(sudo containerlab inspect 2>/dev/null || true)"
    if [[ -n "$CLAB_STATUS" ]] && echo "$CLAB_STATUS" | grep -qv "^$"; then
        warn "ContainerLab reports active labs:"
        echo "$CLAB_STATUS"
    else
        info "No active ContainerLab labs detected."
    fi
else
    for topo in "${CLOUD_CLAB_TOPOLOGIES[@]}"; do
        if [[ -f "$topo" ]]; then
            info "Destroying: $topo"
            sudo containerlab destroy -t "$topo" 2>/dev/null && info "  -> destroyed" || warn "  -> returned non-zero (may already be down)"
        else
            warn "Topology not found (already cleaned?): $topo"
        fi
    done
    info "ContainerLab teardown done."
fi

# ── 4. Tear down Docker Compose stacks ──────────────────────────────────────
step "Tear down Docker Compose stacks"
COMPOSE_FILES=(
    "$INSTALL_DIR/docker/compose-external.yml"
    "$INSTALL_DIR/docker-compose.yml"
    "$INSTALL_DIR/docker/compose-netbox.yml"
)

if $VERIFY_ONLY; then
    RUNNING_CONTAINERS="$(docker ps --format "{{.Names}}" 2>/dev/null | grep -E "bonsai|clab|netbox|splunk|elastic|prometheus|grafana" || true)"
    if [[ -n "$RUNNING_CONTAINERS" ]]; then
        warn "Running containers matching bonsai infra:"
        echo "$RUNNING_CONTAINERS"
    else
        info "No matching containers running."
    fi
else
    for cf in "${COMPOSE_FILES[@]}"; do
        if [[ -f "$cf" ]]; then
            info "Downing compose file: $cf"
            docker compose -f "$cf" down -v 2>/dev/null && info "  -> done" || warn "  -> returned non-zero (may already be down)"
        fi
    done
    info "Docker Compose teardown done."
fi

# ── 5. Back up runtime state ─────────────────────────────────────────────────
step "Back up runtime state"

# DO NOT delete — keep for review (may have unique archive data not on laptop)
BACKUP_DIRS=(archive logs driver_results)
BACKUP_FILES=(chaos_log.jsonl chaos_runner.log)

if $VERIFY_ONLY; then
    for d in "${BACKUP_DIRS[@]}"; do
        if [[ -d "$RUNTIME/$d" ]]; then
            SZ="$(du -sh "$RUNTIME/$d" 2>/dev/null | cut -f1)"
            info "runtime/$d exists ($SZ)"
        fi
    done
    if [[ -d "$RUNTIME/archive" ]]; then
        PARQUET_COUNT="$(find "$RUNTIME/archive" -name "*.parquet" 2>/dev/null | wc -l)"
        info "Parquet files in archive: $PARQUET_COUNT"
    fi
else
    for d in "${BACKUP_DIRS[@]}"; do
        if [[ -d "$RUNTIME/$d" ]]; then
            DEST="$RUNTIME/${d}.precv5-${TS}"
            mv "$RUNTIME/$d" "$DEST"
            info "Backed up runtime/$d -> ${d}.precv5-${TS}"
        fi
    done
    for f in "${BACKUP_FILES[@]}"; do
        if [[ -f "$RUNTIME/$f" ]]; then
            mv "$RUNTIME/$f" "$RUNTIME/${f}.precv5-${TS}"
            info "Backed up runtime/$f"
        fi
    done
    # Move DB files
    for db in bonsai.db bonsai.db.wal; do
        if [[ -f "$RUNTIME/$db" ]]; then
            mv "$RUNTIME/$db" "$RUNTIME/${db}.precv5-${TS}"
            info "Backed up runtime/$db"
        fi
    done
    info "Runtime backup done. Review $RUNTIME/*.precv5-${TS} for unique archive data."
fi

# ── 6. Verification ─────────────────────────────────────────────────────────
step "Verification"

echo ""
echo "--- Docker containers (bonsai/clab related) ---"
docker ps -a --format "table {{.Names}}\t{{.Status}}" 2>/dev/null | grep -E "bonsai|clab|netbox|splunk|elastic|prometheus|grafana" || info "No matching containers."

echo ""
echo "--- ContainerLab inspect ---"
sudo containerlab inspect 2>/dev/null || info "(no active labs or clab not installed)"

echo ""
echo "--- bonsai/chaos_runner processes ---"
PROC_CHECK="$(ps aux | grep -E "bonsai|chaos_runner" | grep -v grep || true)"
if [[ -n "$PROC_CHECK" ]]; then
    warn "Processes still running:"
    echo "$PROC_CHECK"
else
    info "No bonsai/chaos_runner processes."
fi

echo ""
echo "--- runtime/ contents ---"
ls -la "$RUNTIME/" 2>/dev/null || info "(runtime/ does not exist)"

echo ""
echo "--- Cron ---"
CRON_REMAINING="$(crontab -l 2>/dev/null | grep "bonsai" || true)"
if [[ -n "$CRON_REMAINING" ]]; then
    warn "Remaining bonsai cron entries: $CRON_REMAINING"
else
    info "No bonsai cron entries in crontab."
fi

echo ""
echo "--- systemd bonsai service ---"
systemctl is-active bonsai 2>/dev/null && warn "bonsai service still active" || info "bonsai service not active."

if $VERIFY_ONLY; then
    info "Verify-only run complete. No changes made."
else
    info "Cloud cleanup complete. Backed-up dirs have suffix .precv5-${TS}"
    info "Review $RUNTIME/*.precv5-${TS} for unique archive data before next step."
    info "See docs/operations/cloud_cleanup.md for full checklist."
fi

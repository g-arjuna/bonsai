#!/usr/bin/env bash
# DV3 — D3-10 T3
# Laptop cleanup: tear everything bonsai-related down to zero for a clean DV3 rebuild.
#
# DESTRUCTIVE. Moves runtime/ paths to dated backups; does NOT delete them.
# Run from the machine where containerlab and Docker run.
#
# Usage:
#   bash scripts/cleanup_laptop.sh            # full teardown + backup
#   bash scripts/cleanup_laptop.sh --verify   # verify-only (no changes)
#   bash scripts/cleanup_laptop.sh --help

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
        exit 0
        ;;
    "") : ;;
    *) error "Unknown argument: $1"; exit 1 ;;
esac

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
    bash "$REPO_ROOT/scripts/install_cron.sh" --remove 2>/dev/null || true
    # Also remove any entries referencing bonsai scripts that lack the tag (manual installs)
    EXISTING="$(crontab -l 2>/dev/null || true)"
    FILTERED="$(echo "$EXISTING" | grep -v "bv5_daily_check\|bonsai.*daily\|chaos_runner" || true)"
    if [[ "$EXISTING" != "$FILTERED" ]]; then
        echo "$FILTERED" | crontab -
        info "Removed untagged bonsai cron entries."
    fi
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
    sudo systemctl stop bonsai 2>/dev/null && info "Stopped bonsai systemd service." || true
    pkill -9 -f "target/release/bonsai" 2>/dev/null && info "Killed bonsai binary." || true
    pkill -9 -f "chaos_runner" 2>/dev/null && info "Killed chaos_runner." || true
    # Remove installed bonsai binary if present
    if [[ -f /usr/local/bin/bonsai ]]; then
        sudo rm -f /usr/local/bin/bonsai && info "Removed /usr/local/bin/bonsai."
    fi
    # Clean up PID file if stale
    rm -f "$REPO_ROOT/runtime/chaos_runner.pid"
    info "Process cleanup done."
fi

# ── 3. Destroy ContainerLab labs ────────────────────────────────────────────
step "Destroy ContainerLab labs"

CLAB_TOPOLOGIES=(
    "$REPO_ROOT/lab/dc/dc-evpn-srv6.clab.yml"
    "$REPO_ROOT/lab/sp/sp-mpls-srte.clab.yml"
    "$REPO_ROOT/lab/sp/sp-mpls-srte-xrd.clab.yml"
    "$REPO_ROOT/lab/signal-test-lab/signal-test.clab.yml"
    "$REPO_ROOT/lab/fast-iteration/multivendor.clab.yml"
    "$REPO_ROOT/lab/fast-iteration/bonsai-phase4.clab.yml"
    "$REPO_ROOT/lab/fast-iteration/3node-srl.clab.yml"
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
    for topo in "${CLAB_TOPOLOGIES[@]}"; do
        if [[ -f "$topo" ]]; then
            info "Destroying: $topo"
            sudo containerlab destroy -t "$topo" 2>/dev/null && info "  -> destroyed" || warn "  -> destroy returned non-zero (may already be down)"
        fi
    done
    info "ContainerLab teardown done."
fi

# ── 4. Tear down Docker Compose stacks ──────────────────────────────────────
step "Tear down Docker Compose stacks"
COMPOSE_FILES=(
    "$REPO_ROOT/docker/compose-external.yml"
    "$REPO_ROOT/docker-compose.yml"
    "$REPO_ROOT/docker/compose-netbox.yml"
)

if $VERIFY_ONLY; then
    RUNNING_CONTAINERS="$(docker ps --filter "name=bonsai\|clab\|netbox\|splunk\|elastic\|prometheus\|grafana" --format "{{.Names}}" 2>/dev/null || true)"
    if [[ -n "$RUNNING_CONTAINERS" ]]; then
        warn "Running containers matching bonsai/clab/netbox/splunk/elastic:"
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

# ── 5. Clear vault and graph DB ──────────────────────────────────────────────
step "Clear vault and graph DB"
if $VERIFY_ONLY; then
    [[ -f "$REPO_ROOT/vault.age" ]]       && warn "vault.age present (will be removed on full cleanup)" || info "vault.age not present."
    [[ -f "$REPO_ROOT/runtime/bonsai.db" ]] && warn "runtime/bonsai.db present (will be removed on full cleanup)" || info "runtime/bonsai.db not present."
    [[ -f "$REPO_ROOT/bonsai_config.db" ]] && warn "bonsai_config.db present (will be removed on full cleanup)" || info "bonsai_config.db not present."
else
    rm -f "$REPO_ROOT/vault.age"            && info "Removed vault.age."         || true
    rm -f "$REPO_ROOT/runtime/bonsai.db"    && info "Removed runtime/bonsai.db." || true
    rm -f "$REPO_ROOT/runtime/bonsai.db-shm" "$REPO_ROOT/runtime/bonsai.db-wal" 2>/dev/null || true
    rm -f "$REPO_ROOT/bonsai_config.db"     && info "Removed bonsai_config.db."  || true
    info "Vault + DB clear done."
fi

# ── 7. Back up runtime state ─────────────────────────────────────────────────
step "Back up runtime state"
RUNTIME="$REPO_ROOT/runtime"

BACKUP_DIRS=(archive logs driver_results)
BACKUP_FILES=(chaos_log.jsonl chaos_runner.log)

if $VERIFY_ONLY; then
    for d in "${BACKUP_DIRS[@]}"; do
        if [[ -d "$RUNTIME/$d" ]]; then
            SZ="$(du -sh "$RUNTIME/$d" 2>/dev/null | cut -f1)"
            info "runtime/$d exists ($SZ)"
        fi
    done
    if [[ -f "$RUNTIME/bonsai.db.local" ]] || [[ -f "$RUNTIME/bonsai.db.wal.local" ]]; then
        warn "Local DB files present — will be moved on full cleanup."
    fi
else
    for d in "${BACKUP_DIRS[@]}"; do
        if [[ -d "$RUNTIME/$d" ]]; then
            DEST="$RUNTIME/${d}.predv3-${TS}"
            mv "$RUNTIME/$d" "$DEST"
            info "Backed up runtime/$d -> runtime/${d}.predv3-${TS}"
        fi
    done
    for f in "${BACKUP_FILES[@]}"; do
        if [[ -f "$RUNTIME/$f" ]]; then
            mv "$RUNTIME/$f" "$RUNTIME/${f}.predv3-${TS}"
            info "Backed up runtime/$f"
        fi
    done
    # Move local DB if present (not the primary DB used on Windows, but clean up just in case)
    for db in bonsai.db.local bonsai.db.wal.local; do
        if [[ -f "$RUNTIME/$db" ]]; then
            mv "$RUNTIME/$db" "$RUNTIME/${db}.predv3-${TS}"
            info "Backed up runtime/$db"
        fi
    done
    info "Runtime backup done."
fi

# ── 8. Verification ─────────────────────────────────────────────────────────
step "Verification"

echo ""
echo "--- Docker containers (bonsai/clab/netbox/splunk/elastic/grafana/prometheus) ---"
docker ps -a --filter "name=bonsai" --filter "name=clab" \
    --format "table {{.Names}}\t{{.Status}}" 2>/dev/null || true

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

if $VERIFY_ONLY; then
    echo ""
    info "Verify-only run complete. No changes made."
else
    echo ""
    info "Laptop cleanup complete. Backed-up dirs have suffix .predv3-${TS}"
    info "Re-run with --verify to confirm clean state before starting DV3 fresh-install flow."
fi

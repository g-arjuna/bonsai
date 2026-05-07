#!/usr/bin/env bash
# scripts/cloud/deploy.sh — Self-contained bonsai cloud spike deployment.
#
# Runs ON the Oracle Always Free ARM VM (not on your laptop).
# Idempotent: re-running picks up where it left off.
#
# What it does:
#   1. Installs system packages (Docker, git, containerlab)
#   2. Mounts data volume for archive storage
#   3. Clones / updates bonsai repo
#   4. Builds bonsai binary (cross-compiled ARM release)
#   5. Builds Svelte SPA
#   6. Starts external infra (NetBox + Prometheus via Docker Compose)
#   7. Writes bonsai.toml from env vars
#   8. Starts bonsai as a systemd service
#   9. Starts scaled-down 6-node DC ContainerLab topology
#  10. Starts the always-on chaos runner
#  11. Starts the daily archive sync cron (via crontab)
#
# Usage (on the VM):
#   # First time:
#   curl -sSL https://raw.githubusercontent.com/g-arjuna/bonsai/main/scripts/cloud/deploy.sh | bash
#
#   # After repo is cloned:
#   bash /opt/bonsai/scripts/cloud/deploy.sh
#
#   # With options:
#   bash deploy.sh --skip-build   # skip cargo build if binary is current
#   bash deploy.sh --skip-lab     # skip clab topology (for resource-constrained runs)
#   bash deploy.sh --dry-run

set -euo pipefail

# ── Config (override via env vars) ────────────────────────────────────────────

REPO_URL="${REPO_URL:-https://github.com/g-arjuna/bonsai.git}"
INSTALL_DIR="${INSTALL_DIR:-/opt/bonsai}"
ARCHIVE_MOUNT="${ARCHIVE_MOUNT:-/mnt/bonsai-archive}"
DATA_DEVICE="${DATA_DEVICE:-/dev/sdb}"           # Second block device (data volume)
BONSAI_PORT="${BONSAI_PORT:-3000}"
NETBOX_PORT="${NETBOX_PORT:-8080}"

SKIP_BUILD=false
SKIP_LAB=false
DRY_RUN=false

for arg in "$@"; do
    case "$arg" in
        --skip-build) SKIP_BUILD=true ;;
        --skip-lab)   SKIP_LAB=true ;;
        --dry-run)    DRY_RUN=true ;;
        *) echo "Unknown arg: $arg" >&2; exit 1 ;;
    esac
done

# ── Helpers ───────────────────────────────────────────────────────────────────

_log() { echo "[$(date -u '+%H:%M:%S')] $*"; }
_die() { echo "ERROR: $*" >&2; exit 1; }
_run() { "$DRY_RUN" && echo "[DRY-RUN] $*" || "$@"; }
_step() { _log ""; _log "=== Step $1: $2 ==="; }

# ── Step 1: System packages ───────────────────────────────────────────────────

_step 1 "System packages"

_run sudo dnf install -y -q \
    git curl wget jq \
    docker docker-compose \
    gcc make pkg-config openssl-devel \
    python3 python3-pip python3-venv \
    iproute-tc   # tc / netem for chaos injection

# Enable and start Docker
_run sudo systemctl enable --now docker
_run sudo usermod -aG docker "$USER" || true

# Install Rust (if not present)
if ! command -v cargo &>/dev/null; then
    _log "Installing Rust toolchain..."
    _run curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain stable --profile minimal
    source "$HOME/.cargo/env"
fi

# Install ContainerLab
if ! command -v containerlab &>/dev/null; then
    _log "Installing ContainerLab..."
    _run bash -c "$(curl -sL https://get.containerlab.dev)"
fi

# Install Node (for Svelte build) — using NodeSource 20.x
if ! command -v node &>/dev/null; then
    _log "Installing Node 20..."
    _run curl -fsSL https://rpm.nodesource.com/setup_20.x | sudo bash -
    _run sudo dnf install -y nodejs
fi

# ── Step 2: Data volume mount ─────────────────────────────────────────────────

_step 2 "Data volume mount ($DATA_DEVICE → $ARCHIVE_MOUNT)"

if ! mountpoint -q "$ARCHIVE_MOUNT" 2>/dev/null; then
    if [[ -b "$DATA_DEVICE" ]]; then
        # Format only if unformatted
        if ! sudo blkid "$DATA_DEVICE" &>/dev/null; then
            _log "Formatting $DATA_DEVICE as ext4..."
            _run sudo mkfs.ext4 -F "$DATA_DEVICE"
        fi
        _run sudo mkdir -p "$ARCHIVE_MOUNT"
        _run sudo mount "$DATA_DEVICE" "$ARCHIVE_MOUNT"
        _run sudo chown "$USER:$USER" "$ARCHIVE_MOUNT"

        # Add to fstab for persistence across reboots
        BLKID=$(sudo blkid -s UUID -o value "$DATA_DEVICE")
        grep -q "$BLKID" /etc/fstab 2>/dev/null || \
            echo "UUID=$BLKID  $ARCHIVE_MOUNT  ext4  defaults,nofail  0 2" | \
            sudo tee -a /etc/fstab > /dev/null
        _log "  Mounted: $DATA_DEVICE → $ARCHIVE_MOUNT"
    else
        _log "  WARN: $DATA_DEVICE not found — archive will use local disk"
        sudo mkdir -p "$ARCHIVE_MOUNT"
        sudo chown "$USER:$USER" "$ARCHIVE_MOUNT"
    fi
else
    _log "  Already mounted: $ARCHIVE_MOUNT"
fi

# ── Step 3: Clone / update repo ───────────────────────────────────────────────

_step 3 "Repository ($REPO_URL → $INSTALL_DIR)"

if [[ -d "$INSTALL_DIR/.git" ]]; then
    _log "  Pulling latest..."
    _run git -C "$INSTALL_DIR" pull --ff-only
else
    _log "  Cloning..."
    _run git clone "$REPO_URL" "$INSTALL_DIR"
fi

cd "$INSTALL_DIR"

# ── Step 4: Build bonsai binary ────────────────────────────────────────────────

_step 4 "Cargo build (--release)"

if "$SKIP_BUILD" && [[ -f "target/release/bonsai" ]]; then
    _log "  Skipping build (--skip-build, binary exists)"
else
    source "$HOME/.cargo/env" 2>/dev/null || true
    _run cargo build --release
fi

# ── Step 5: Build Svelte SPA ──────────────────────────────────────────────────

_step 5 "Svelte SPA (npm run build)"

if [[ -d "ui" ]]; then
    _run npm --prefix ui ci --silent
    _run npm --prefix ui run build
else
    _log "  No ui/ directory — skipping SPA build"
fi

# ── Step 6: External infrastructure (NetBox + Prometheus) ─────────────────────

_step 6 "External infra (docker compose)"

# Use a minimal compose override for cloud (no Splunk/Elastic to save RAM)
cat > /tmp/compose-cloud-override.yml <<'COMPOSE'
# Cloud spike overlay: disable resource-heavy adapters
services:
  splunk:
    profiles: [disabled]
  elastic:
    profiles: [disabled]
  kibana:
    profiles: [disabled]
COMPOSE

if [[ -f "compose-external.yml" ]]; then
    _run docker compose \
        -f compose-external.yml \
        -f /tmp/compose-cloud-override.yml \
        up -d --remove-orphans
    _log "  External infra started"
else
    _log "  No compose-external.yml — skipping infra"
fi

# ── Step 7: Write bonsai.toml ─────────────────────────────────────────────────

_step 7 "bonsai.toml configuration"

if [[ ! -f "$INSTALL_DIR/bonsai.toml" ]]; then
    _log "  Writing bonsai.toml from template..."
    # Derive archive path from the mounted data volume
    sed \
        -e "s|^# archive_path.*|archive_path = \"$ARCHIVE_MOUNT/archive\"|" \
        -e "s|^# file_path.*|file_path = \"$ARCHIVE_MOUNT/logs/bonsai.log\"|" \
        "$INSTALL_DIR/bonsai.toml.example" > "$INSTALL_DIR/bonsai.toml"
    _log "  Written: $INSTALL_DIR/bonsai.toml"
    _log "  NOTE: Add device credentials to bonsai.toml before starting collection."
else
    _log "  bonsai.toml already exists — not overwriting"
fi

# Create archive directories
mkdir -p "$ARCHIVE_MOUNT/archive" "$ARCHIVE_MOUNT/logs" "$ARCHIVE_MOUNT/snapshots"

# ── Step 8: Systemd service for bonsai ────────────────────────────────────────

_step 8 "Systemd service (bonsai)"

sudo tee /etc/systemd/system/bonsai.service > /dev/null <<SERVICE
[Unit]
Description=Bonsai Network State Engine
After=network.target docker.service
Wants=docker.service

[Service]
Type=simple
User=$USER
WorkingDirectory=$INSTALL_DIR
ExecStart=$INSTALL_DIR/target/release/bonsai --config $INSTALL_DIR/bonsai.toml
Restart=on-failure
RestartSec=10s
StandardOutput=append:$ARCHIVE_MOUNT/logs/bonsai.log
StandardError=append:$ARCHIVE_MOUNT/logs/bonsai.log
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
SERVICE

_run sudo systemctl daemon-reload
_run sudo systemctl enable bonsai
_run sudo systemctl restart bonsai
_log "  bonsai.service started"

# Wait for bonsai HTTP
_log "  Waiting for bonsai HTTP on :$BONSAI_PORT..."
for i in $(seq 1 24); do
    curl -sf "http://localhost:$BONSAI_PORT/api/topology" &>/dev/null && break || sleep 5
done
curl -sf "http://localhost:$BONSAI_PORT/api/topology" &>/dev/null && \
    _log "  bonsai is up" || \
    _log "  WARN: bonsai not responding on :$BONSAI_PORT — check $ARCHIVE_MOUNT/logs/bonsai.log"

# ── Step 9: ContainerLab 6-node DC topology ────────────────────────────────────

_step 9 "ContainerLab 6-node DC topology"

if "$SKIP_LAB"; then
    _log "  Skipping (--skip-lab)"
elif [[ -f "$INSTALL_DIR/lab/cloud-dc-6node.yml" ]]; then
    # Bring up if not running
    if ! sudo containerlab inspect --name bonsai-cloud-dc &>/dev/null 2>&1; then
        _log "  Starting 6-node DC lab..."
        _run sudo containerlab deploy \
            --topo "$INSTALL_DIR/lab/cloud-dc-6node.yml" \
            --reconfigure
    else
        _log "  Lab already running"
    fi
else
    _log "  WARN: lab/cloud-dc-6node.yml not found — skipping lab"
    _log "  Create a scaled-down 6-node topology and place it at lab/cloud-dc-6node.yml"
fi

# ── Step 10: Always-on chaos runner ────────────────────────────────────────────

_step 10 "Chaos runner daemon"

CHAOS_STATUS=$(bash "$INSTALL_DIR/scripts/chaos_runner.sh" --status 2>/dev/null || echo "stopped")
if echo "$CHAOS_STATUS" | grep -q "RUNNING"; then
    _log "  Chaos runner already running"
else
    _log "  Starting chaos runner daemon..."
    _run bash "$INSTALL_DIR/scripts/chaos_runner.sh"
fi

# ── Step 11: Daily archive sync cron ─────────────────────────────────────────

_step 11 "Daily archive sync cron"

CRON_ENTRY="0 3 * * * bash $INSTALL_DIR/scripts/cloud/daily_sync.sh >> $ARCHIVE_MOUNT/logs/daily_sync.log 2>&1"
# Only add if not already present
if ! crontab -l 2>/dev/null | grep -qF "daily_sync.sh"; then
    (crontab -l 2>/dev/null; echo "$CRON_ENTRY") | crontab -
    _log "  Cron installed: 03:00 UTC daily"
else
    _log "  Cron already present"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

_log ""
_log "=== Deploy complete ==="
_log ""
_log "Bonsai UI:       http://$(curl -s ifconfig.me 2>/dev/null || echo '<public-ip>'):$BONSAI_PORT"
_log "Archive:         $ARCHIVE_MOUNT/archive/"
_log "Logs:            $ARCHIVE_MOUNT/logs/bonsai.log"
_log "Chaos log:       $INSTALL_DIR/runtime/chaos_runner.log"
_log ""
_log "Useful commands:"
_log "  sudo systemctl status bonsai"
_log "  bash scripts/chaos_runner.sh --status"
_log "  bash scripts/cloud/daily_sync.sh --dry-run"
_log "  bash scripts/verify_archive.sh $ARCHIVE_MOUNT/archive"

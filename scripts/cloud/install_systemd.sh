#!/usr/bin/env bash
# CV7 T2-2 — Idempotent installer for the three systemd units that operate
# bonsai on cloud: bonsai.service, bonsai-rules-sidecar.service, bonsai-chaos.service.
#
# Run as root on the OCI ARM64 instance. Refuses on Mac.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'

ENV_DETECTED="$(bash scripts/dev/whichenv.sh 2>/dev/null || echo unknown)"
if [[ "$ENV_DETECTED" == "mac-dev" ]]; then
  echo "${RED}Refused.${RESET} This script is cloud-only. See dev_vs_ops_boundary.md." >&2
  exit 2
fi

if [[ "$(id -u)" -ne 0 ]]; then
  echo "${RED}This script must run as root (use sudo).${RESET}" >&2
  exit 1
fi

UNITS=(
  "bonsai.service"
  "bonsai-rules-sidecar.service"
  "bonsai-chaos.service"
)

SRC_DIR="$REPO_ROOT/deploy/systemd"
DEST_DIR="/etc/systemd/system"
ENV_DIR="/etc/bonsai"

# ── Pre-flight ────────────────────────────────────────────────────────────────
for u in "${UNITS[@]}"; do
  [[ -f "$SRC_DIR/$u" ]] || { echo "${RED}missing source unit: $SRC_DIR/$u${RESET}" >&2; exit 1; }
done

# Create bonsai user if absent.
if ! id -u bonsai >/dev/null 2>&1; then
  echo "${YELLOW}creating 'bonsai' system user${RESET}"
  useradd --system --home /opt/bonsai --shell /usr/sbin/nologin bonsai
fi

mkdir -p "$ENV_DIR"
chmod 750 "$ENV_DIR"

# Create empty env files if absent. The actual values stay out of git.
for env_file in bonsai.env sidecar.env chaos.env; do
  if [[ ! -f "$ENV_DIR/$env_file" ]]; then
    echo "${YELLOW}creating empty $ENV_DIR/$env_file${RESET}"
    cat > "$ENV_DIR/$env_file" <<EOF
# /etc/bonsai/$env_file — environment overrides for the matching systemd unit.
# Populate with site-specific values. This file is NOT in git.
EOF
    chmod 640 "$ENV_DIR/$env_file"
    chown root:bonsai "$ENV_DIR/$env_file"
  fi
done

# Default: require the rules sidecar so /health reports degraded if it's missing.
if ! grep -q BONSAI_REQUIRE_SIDECAR "$ENV_DIR/bonsai.env"; then
  echo "BONSAI_REQUIRE_SIDECAR=rules" >> "$ENV_DIR/bonsai.env"
fi

# ── Verify Python venv exists (sidecar prereq) ────────────────────────────────
# bonsai-rules-sidecar.service expects /opt/bonsai/.venv/bin/python. We refuse
# to "successfully install" units that won't start. The operator creates the
# venv as part of cloud bringup (see scripts/cloud/cloud_init.sh).
PYTHON_BIN="/opt/bonsai/.venv/bin/python"
if [[ ! -x "$PYTHON_BIN" ]]; then
  echo "${RED}Missing $PYTHON_BIN — bonsai-rules-sidecar.service won't start.${RESET}" >&2
  echo "Create it with:" >&2
  echo "  sudo -u bonsai python3 -m venv /opt/bonsai/.venv" >&2
  echo "  sudo -u bonsai /opt/bonsai/.venv/bin/pip install -e /opt/bonsai/python" >&2
  echo "Re-run this installer after the venv is in place." >&2
  exit 1
fi

# ── Install units ─────────────────────────────────────────────────────────────
for u in "${UNITS[@]}"; do
  echo "installing $u"
  install -m 0644 "$SRC_DIR/$u" "$DEST_DIR/$u"
done

systemctl daemon-reload

# ── Enable + (re)start ────────────────────────────────────────────────────────
for u in "${UNITS[@]}"; do
  systemctl enable "$u"
done

systemctl restart bonsai.service
sleep 2
# Sidecar BindsTo=bonsai → starts automatically. Chaos is independent.
systemctl restart bonsai-chaos.service

echo
echo "${GREEN}Done. systemctl status of each unit:${RESET}"
for u in "${UNITS[@]}"; do
  systemctl --no-pager status "$u" 2>&1 | head -7
  echo
done

echo "Health endpoint (should show ok or degraded with reason):"
curl -fsS http://localhost:3000/health || echo "  (not reachable yet — first start)"

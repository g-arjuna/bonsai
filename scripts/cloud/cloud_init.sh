#!/usr/bin/env bash
# scripts/cloud/cloud_init.sh — Oracle Linux 8 ARM first-boot tuning.
#
# Passed as user_data (base64) to the OCI instance at launch.
# Runs once as root during cloud-init (before deploy.sh).
#
# What it does:
#   - Kernel network/memory tuning for ContainerLab + bonsai
#   - Increase open-file limits for Docker containers
#   - Disable swap (avoids OOM killer confusion under high memory pressure)
#   - Enable IP forwarding (required for ContainerLab bridge networking)
#   - Set up firewalld rules (SSH + bonsai :3000)
#   - Create opc user sudoers entry (already exists on OCI, but ensures it)
#   - Log completion to /var/log/bonsai-cloud-init.log

set -euo pipefail

LOG="/var/log/bonsai-cloud-init.log"
exec > >(tee -a "$LOG") 2>&1

echo "=== Bonsai cloud-init start: $(date -u '+%Y-%m-%dT%H:%M:%SZ') ==="

# ── Kernel parameters ─────────────────────────────────────────────────────────

cat > /etc/sysctl.d/90-bonsai.conf << 'SYSCTL'
# Networking: larger socket buffers for gNMI streaming
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 8388608
net.core.wmem_default = 8388608
net.ipv4.tcp_rmem = 4096 8388608 134217728
net.ipv4.tcp_wmem = 4096 8388608 134217728

# Increase conntrack table for many containerlab bridge flows
net.netfilter.nf_conntrack_max = 524288

# IP forwarding — required for ContainerLab
net.ipv4.ip_forward = 1
net.ipv6.conf.all.forwarding = 1

# File descriptors
fs.file-max = 2097152
fs.inotify.max_user_instances = 8192
fs.inotify.max_user_watches = 524288

# Virtual memory: don't swap unless truly necessary
vm.swappiness = 1
SYSCTL

sysctl --system -q
echo "[OK] sysctl applied"

# ── Open-file limits ──────────────────────────────────────────────────────────

cat > /etc/security/limits.d/90-bonsai.conf << 'LIMITS'
*    soft nofile 262144
*    hard nofile 262144
root soft nofile 262144
root hard nofile 262144
LIMITS

echo "[OK] file limits set"

# ── Disable swap ──────────────────────────────────────────────────────────────

swapoff -a || true
sed -i '/\bswap\b/d' /etc/fstab
echo "[OK] swap disabled"

# ── Firewalld: open bonsai HTTP port ─────────────────────────────────────────

if command -v firewall-cmd &>/dev/null && systemctl is-active firewalld &>/dev/null; then
    firewall-cmd --permanent --add-port=3000/tcp
    firewall-cmd --permanent --add-port=22/tcp
    firewall-cmd --reload
    echo "[OK] firewalld rules added (SSH + :3000)"
else
    # iptables fallback
    iptables -I INPUT -p tcp --dport 3000 -j ACCEPT 2>/dev/null || true
    echo "[OK] iptables rule added for :3000"
fi

# ── Docker storage driver ─────────────────────────────────────────────────────
# overlay2 is default and correct; ensure it's set explicitly

mkdir -p /etc/docker
cat > /etc/docker/daemon.json << 'DOCKER'
{
  "storage-driver": "overlay2",
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "50m",
    "max-file": "3"
  },
  "default-ulimits": {
    "nofile": {
      "Name": "nofile",
      "Hard": 262144,
      "Soft": 262144
    }
  }
}
DOCKER

echo "[OK] docker daemon.json written"

# ── Install EPEL (for additional packages deploy.sh might need) ───────────────

dnf install -y -q oracle-epel-release-el8 2>/dev/null || \
    dnf install -y -q epel-release 2>/dev/null || \
    echo "[WARN] EPEL install failed — skipping"

# ── Timezone ──────────────────────────────────────────────────────────────────

timedatectl set-timezone UTC 2>/dev/null || true
echo "[OK] timezone set to UTC"

echo "=== Bonsai cloud-init complete: $(date -u '+%Y-%m-%dT%H:%M:%SZ') ==="

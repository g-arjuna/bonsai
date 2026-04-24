#!/usr/bin/env bash
# backup_volumes.sh — Snapshot bonsai Docker volumes to a local directory.
#
# Usage:
#   scripts/backup_volumes.sh [output-dir]
#
# Output directory defaults to ./backups/<timestamp>.
# Each volume is written as a .tar.gz archive.
# Restore with: scripts/restore_volumes.sh <backup-dir>

set -euo pipefail

TIMESTAMP=$(date -u +%Y%m%dT%H%M%SZ)
OUTPUT_DIR="${1:-./backups/${TIMESTAMP}}"
mkdir -p "${OUTPUT_DIR}"

backup_volume() {
    local volume="$1"
    local out="${OUTPUT_DIR}/${volume}.tar.gz"

    # Check volume exists
    if ! docker volume inspect "${volume}" &>/dev/null; then
        echo "  [skip] volume '${volume}' does not exist"
        return
    fi

    echo "  Backing up ${volume} → ${out}"
    docker run --rm \
        -v "${volume}:/data:ro" \
        -v "$(realpath "${OUTPUT_DIR}"):/backup" \
        debian:trixie-slim \
        tar czf "/backup/${volume}.tar.gz" -C /data .

    local size
    size=$(du -sh "${out}" | cut -f1)
    echo "    done (${size})"
}

echo "=== Bonsai volume backup: ${TIMESTAMP} ==="
echo "Output: ${OUTPUT_DIR}"
echo ""

# bonsai_creds: encrypted credential vault — most important to back up
backup_volume "bonsai_creds"
backup_volume "bonsai_creds_dev"

# bonsai_graph / bonsai_graph_dev: LadybugDB — regenerates from telemetry,
# but useful to snapshot for forensics or quick restores
backup_volume "bonsai_graph"
backup_volume "bonsai_graph_dev"

# collector queues: in-flight telemetry buffers — ephemeral, skip by default
# Uncomment if you want to preserve in-flight records during a planned outage:
# backup_volume "collector_1_queue"
# backup_volume "collector_2_queue"

echo ""
echo "=== Backup complete ==="
ls -lh "${OUTPUT_DIR}"
echo ""
echo "To restore:"
echo "  scripts/restore_volumes.sh ${OUTPUT_DIR}"

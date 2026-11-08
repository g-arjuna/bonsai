#!/usr/bin/env bash
# backup_volumes.sh — Snapshot bonsai Docker volumes to a local directory.
#
# Usage:
#   scripts/backup_volumes.sh [output-dir]
#
# Output directory defaults to ./backups/<timestamp>.
# Each volume is written as a .tar.gz archive.
# Restore with: scripts/restore_volumes.sh <backup-dir>
#
# Volume names: compose prefixes declared volumes with the project name
# (e.g. bonsai_bonsai_graph_dev). This script resolves the logical volume
# names below to the real Docker volume, trying in order:
#   1. the literal name (unprefixed/custom-named volumes)
#   2. <COMPOSE_PROJECT_NAME or cwd basename>_<name>
#   3. a unique *_<name> suffix match across all volumes

set -euo pipefail

TIMESTAMP=$(date -u +%Y%m%dT%H%M%SZ)
OUTPUT_DIR="${1:-./backups/${TIMESTAMP}}"
mkdir -p "${OUTPUT_DIR}"

resolve_volume() {
    local logical="$1"
    if docker volume inspect "${logical}" &>/dev/null; then
        printf '%s\n' "${logical}"
        return 0
    fi
    local project="${COMPOSE_PROJECT_NAME:-$(basename "$(pwd)")}"
    local candidate="${project}_${logical}"
    if docker volume inspect "${candidate}" &>/dev/null; then
        printf '%s\n' "${candidate}"
        return 0
    fi
    local match="" count=0 v
    while IFS= read -r v; do
        [[ -n "$v" ]] || continue
        match="$v"
        count=$((count + 1))
    done < <(docker volume ls -q | grep -E "(^|_)${logical}\$" || true)
    if [[ "${count}" -eq 1 ]]; then
        printf '%s\n' "${match}"
        return 0
    fi
    return 1
}

backup_volume() {
    local logical="$1"
    local volume
    if ! volume="$(resolve_volume "${logical}")"; then
        echo "  [skip] volume '${logical}' does not exist"
        return
    fi
    local out="${OUTPUT_DIR}/${volume}.tar.gz"

    echo "  Backing up ${volume} (logical: ${logical}) → ${out}"
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

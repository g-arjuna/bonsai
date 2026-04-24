#!/usr/bin/env bash
# restore_volumes.sh — Restore bonsai Docker volumes from a backup directory.
#
# Usage:
#   scripts/restore_volumes.sh <backup-dir>
#
# WARNING: This overwrites the contents of existing volumes.
# Stop bonsai services before restoring to avoid data corruption:
#   docker compose --profile two-collector down

set -euo pipefail

BACKUP_DIR="${1:?Usage: $0 <backup-dir>}"

if [[ ! -d "${BACKUP_DIR}" ]]; then
    echo "Error: backup directory '${BACKUP_DIR}' not found" >&2
    exit 1
fi

restore_volume() {
    local volume="$1"
    local archive="${BACKUP_DIR}/${volume}.tar.gz"

    if [[ ! -f "${archive}" ]]; then
        echo "  [skip] no archive for '${volume}' in ${BACKUP_DIR}"
        return
    fi

    echo "  Restoring ${volume} ← ${archive}"
    # Ensure volume exists
    docker volume create "${volume}" &>/dev/null || true

    docker run --rm \
        -v "${volume}:/data" \
        -v "$(realpath "${BACKUP_DIR}"):/backup:ro" \
        debian:trixie-slim \
        bash -c "rm -rf /data/* /data/..?* /data/.[!.]* 2>/dev/null; tar xzf /backup/${volume}.tar.gz -C /data"

    echo "    done"
}

echo "=== Bonsai volume restore from: ${BACKUP_DIR} ==="
echo ""
echo "WARNING: Make sure bonsai services are stopped before continuing."
read -rp "Continue? [y/N] " confirm
if [[ "${confirm}" != "y" && "${confirm}" != "Y" ]]; then
    echo "Aborted."
    exit 0
fi

restore_volume "bonsai_creds"
restore_volume "bonsai_creds_dev"
restore_volume "bonsai_graph"
restore_volume "bonsai_graph_dev"

echo ""
echo "=== Restore complete ==="
echo "Start services: docker compose --profile two-collector up -d"

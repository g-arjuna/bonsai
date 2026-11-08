#!/usr/bin/env bash
# restore_volumes.sh — Restore bonsai Docker volumes from a backup directory.
#
# Usage:
#   scripts/restore_volumes.sh <backup-dir>
#
# WARNING: This overwrites the contents of existing volumes.
# Stop bonsai services before restoring to avoid data corruption:
#   docker compose --profile standalone down
#
# Volume names: archives are named after the real (project-prefixed) Docker
# volume, e.g. bonsai_bonsai_graph_dev.tar.gz. Logical names are resolved the
# same way as backup_volumes.sh when an exact-name archive is absent.

set -euo pipefail

BACKUP_DIR="${1:?Usage: $0 <backup-dir>}"

if [[ ! -d "${BACKUP_DIR}" ]]; then
    echo "Error: backup directory '${BACKUP_DIR}' not found" >&2
    exit 1
fi

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

find_archive() {
    local logical="$1"
    # Prefer an archive named after the resolved real volume.
    local volume
    if volume="$(resolve_volume "${logical}" 2>/dev/null)"; then
        [[ -f "${BACKUP_DIR}/${volume}.tar.gz" ]] && {
            printf '%s\n' "${volume}"
            return 0
        }
    fi
    # Fall back to the literal logical name archive.
    [[ -f "${BACKUP_DIR}/${logical}.tar.gz" ]] && {
        printf '%s\n' "${logical}"
        return 0
    }
    # Volume may not exist yet (fresh deploy) — match archive by suffix,
    # preferring the compose project prefix.
    local project="${COMPOSE_PROJECT_NAME:-$(basename "$(pwd)")}"
    local prefixed="${project}_${logical}"
    [[ -f "${BACKUP_DIR}/${prefixed}.tar.gz" ]] && {
        printf '%s\n' "${prefixed}"
        return 0
    }
    local match="" count=0 f base
    for f in "${BACKUP_DIR}/"*.tar.gz; do
        [[ -e "$f" ]] || continue
        base="$(basename "$f" .tar.gz)"
        if [[ "$base" == *_"${logical}" ]]; then
            match="$base"
            count=$((count + 1))
        fi
    done
    if [[ "${count}" -eq 1 ]]; then
        printf '%s\n' "${match}"
        return 0
    fi
    return 1
}

restore_volume() {
    local logical="$1"
    local volume
    if ! volume="$(find_archive "${logical}")"; then
        echo "  [skip] no archive for '${logical}' in ${BACKUP_DIR}"
        return
    fi
    local archive="${BACKUP_DIR}/${volume}.tar.gz"

    echo "  Restoring ${volume} (logical: ${logical}) ← ${archive}"
    # Ensure the target volume exists under the resolved name.
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
echo "Start services: docker compose --profile standalone up -d"

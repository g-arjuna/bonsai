# Bonsai Deployment Volumes

This document outlines the Docker volumes used in a Bonsai deployment, their roles, and the impact of data loss.

| Volume | Host | Contains | Loss impact | Backup priority |
|---|---|---|---|---|
| `bonsai_creds` | core | Encrypted credential vault (`bonsai-credentials*`) | Permanent — operators must re-enter all device credentials. | **High** — back up regularly. |
| `bonsai_graph` | core | LadybugDB files (`bonsai.db*`) | Rebuilds from live telemetry within minutes. Minor loss of short-lived state transitions. | Low — regenerates automatically. |
| `bonsai_graph_dev` | core (dev profile) | Same as bonsai_graph | Same as above. | Low. |
| `collector_1_queue` / `collector_2_queue` | collector | Disk-backed in-flight queue during core outages | Transient — in-flight records lost. No long-term impact if core was unreachable < queue TTL. | Ephemeral — no backup required. |

## Backup recipes

Automated backup using `scripts/backup_volumes.sh`:

```bash
# Snapshot all volumes to ./backups/<timestamp>/
scripts/backup_volumes.sh

# Or specify a custom output directory
scripts/backup_volumes.sh /mnt/nas/bonsai-backups/$(date +%F)
```

Each volume is written as a `.tar.gz` archive using a disposable `debian:trixie-slim` container — no host tooling required beyond Docker.

### Manual one-liner (single volume)

```bash
# Backup bonsai_creds to current directory
docker run --rm \
  -v bonsai_creds:/data:ro \
  -v "$(pwd):/backup" \
  debian:trixie-slim \
  tar czf /backup/bonsai_creds.tar.gz -C /data .
```

### Restore

```bash
# Interactive restore (prompts before overwriting)
scripts/restore_volumes.sh ./backups/20260424T120000Z

# Manual restore of a single volume
docker compose --profile two-collector down
docker run --rm \
  -v bonsai_creds:/data \
  -v "$(pwd):/backup:ro" \
  debian:trixie-slim \
  bash -c "rm -rf /data/*; tar xzf /backup/bonsai_creds.tar.gz -C /data"
docker compose --profile two-collector up -d
```

## Recommendations

1. **Back up `bonsai_creds` first.** This is the only volume that cannot be regenerated. Store the backup alongside a copy of `BONSAI_VAULT_PASSPHRASE` in your secret manager (e.g., 1Password, Bitwarden, HashiCorp Vault). Losing the passphrase means losing access to the vault even with the backup file.

2. **Collectors are disposable.** Collector queue volumes can be deleted without long-term data loss. If a collector restarts with a fresh queue it will simply re-subscribe to all assigned devices.

3. **Graph recovery is automatic.** If a core node is lost, restore `bonsai_creds`, set `BONSAI_VAULT_PASSPHRASE`, and start the stack. The graph rebuilds within seconds as collectors reconnect and telemetry flows.

4. **Backup frequency.** For the credential vault, daily snapshots are sufficient for most lab deployments. For production-shaped deployments, see `docs/deployment_segmentation.md` for offsite replication recommendations.

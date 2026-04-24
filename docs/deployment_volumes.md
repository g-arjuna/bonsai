# Bonsai Deployment Volumes

This document outlines the Docker volumes used in a Bonsai deployment, their roles, and the impact of data loss.

| Volume | Host | Contains | Loss impact | Backup Strategy |
|---|---|---|---|---|
| `bonsai_graph` | core | LadybugDB files (`bonsai.db*`) | Regenerates from telemetry within minutes. Minor loss of short-lived state transitions. | Not strictly necessary, but can be snapshot if needed. |
| `bonsai_archive` | collector | Parquet files containing historical telemetry | Permanent loss of historical telemetry data. Affects ML model training (GNN) and long-term reporting. | Periodic off-site replication (e.g., to S3 or remote storage) using tools like `rsync` or native S3 export when available. |
| `bonsai_creds` | core | Encrypted credential vault (`bonsai-credentials*`) | Permanent. Operators must re-enter and re-authenticate credentials for all devices via the UI or CLI. | Important. Backup the vault directory regularly. Ensure the passphrase is stored securely in a secret manager. |
| `collector_queue` | collector | Disk-backed event queue (`runtime/collector-queue`) | Transient. In-flight telemetry collected during a core outage is lost. No impact on long-term data if the network is stable. | Ephemeral. No backup required. |

## Recommendations

1.  **Core Node Backup:** The most critical state resides on the core node (`bonsai_creds`). Always ensure this volume is backed up.
2.  **Collector Ephemerality:** Collectors are designed to be semi-ephemeral. Their state (`collector_queue`) can be lost without major consequence, except for the `bonsai_archive` if local Parquet storage is the primary historical record.
3.  **Restoring from Backup:** If a core node is lost, restore the `bonsai_creds` volume and provide the `BONSAI_VAULT_PASSPHRASE` environment variable. The graph will automatically rebuild itself as collectors reconnect and devices stream telemetry.

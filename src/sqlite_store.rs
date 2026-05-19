//! SQLite-based config store for registry, enrichers, adapters, settings.
//!
//! Replaces JSON file persistence with SQLite for:
//! - Device registry (ApiRegistry)
//! - Enricher configs (EnricherRegistry)
//! - Output adapter configs (OutputAdapterRegistry)
//! - Streaming settings (surgical TOML replacement)
//!
//! Provides:
//! - Versioned schema with migrations
//! - Audit trail for all mutations
//! - Transactional writes
//! - Migration path from existing JSON files

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DB_FILE: &str = "bonsai_config.db";
const CURRENT_SCHEMA_VERSION: i64 = 1;

/// SQLite config store with versioned schema and audit trail.
pub struct SqliteStore {
    db: Arc<Mutex<Connection>>,
    runtime_dir: PathBuf,
    config_replicator: Option<Arc<crate::ha_coordinator::ConfigReplicator>>,
}

impl SqliteStore {
    /// Open or create the config store, running migrations if needed.
    pub fn open(runtime_dir: &Path) -> Result<Self> {
        let db_path = runtime_dir.join(DB_FILE);
        let db = Connection::open(&db_path)
            .with_context(|| format!("failed to open SQLite DB at '{}'", db_path.display()))?;

        let store = Self {
            db: Arc::new(Mutex::new(db)),
            runtime_dir: runtime_dir.to_path_buf(),
            config_replicator: None,
        };

        store.init_schema()?;
        store.enable_wal()?;

        Ok(store)
    }

    /// Run schema migrations to bring DB to current version.
    fn migrate(&self) -> Result<()> {
        let db = self.db.lock().unwrap();
        let user_version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        if user_version == CURRENT_SCHEMA_VERSION {
            return Ok(());
        }

        tracing::info!(from = user_version, to = CURRENT_SCHEMA_VERSION, "migrating config store schema");

        // Version 1: Initial schema
        if user_version < 1 {
            db.execute_batch(r#"
                CREATE TABLE IF NOT EXISTS devices (
                    address TEXT PRIMARY KEY,
                    hostname TEXT,
                    vendor TEXT,
                    role TEXT,
                    site TEXT,
                    collector_id TEXT,
                    username TEXT,
                    password_env TEXT,
                    ca_cert TEXT,
                    paths TEXT, -- JSON array
                    optional INTEGER DEFAULT 0,
                    created_at_ns INTEGER,
                    updated_at_ns INTEGER,
                    created_by TEXT,
                    updated_by TEXT,
                    last_operator_action TEXT
                );

                CREATE TABLE IF NOT EXISTS enrichers (
                    name TEXT PRIMARY KEY,
                    enricher_type TEXT,
                    enabled INTEGER DEFAULT 0,
                    base_url TEXT,
                    credential_alias TEXT,
                    poll_interval_secs INTEGER DEFAULT 0,
                    environment_scope TEXT, -- JSON array
                    extra TEXT, -- JSON
                    created_at_ns INTEGER,
                    updated_at_ns INTEGER
                );

                CREATE TABLE IF NOT EXISTS adapters (
                    name TEXT PRIMARY KEY,
                    adapter_type TEXT,
                    enabled INTEGER DEFAULT 0,
                    endpoint_url TEXT,
                    credential_alias TEXT,
                    topic TEXT,
                    environment_scope TEXT, -- JSON array
                    extra TEXT, -- JSON
                    created_at_ns INTEGER,
                    updated_at_ns INTEGER
                );

                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT, -- JSON
                    updated_at_ns INTEGER
                );

                CREATE TABLE IF NOT EXISTS audit_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    table_name TEXT,
                    operation TEXT, -- INSERT, UPDATE, DELETE
                    record_key TEXT,
                    actor TEXT,
                    action TEXT,
                    timestamp_ns INTEGER,
                    old_value TEXT, -- JSON
                    new_value TEXT -- JSON
                );

                CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp_ns);

                -- G5: Collector registration audit log
                CREATE TABLE IF NOT EXISTS collector_registrations (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    collector_id TEXT,
                    hostname TEXT,
                    protocol_version INTEGER,
                    peer_ip TEXT,
                    cert_fingerprint TEXT,
                    timestamp_ns INTEGER,
                    success INTEGER DEFAULT 1,
                    rejection_reason TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_collector_reg_timestamp ON collector_registrations(timestamp_ns);
            "#).context("failed to create schema v1")?;

            db.execute("PRAGMA user_version=1", [])
                .context("failed to set schema version")?;
        }

        Ok(())
    }

    /// Migrate existing JSON files to SQLite on first run.
    pub fn migrate_from_json(&self, runtime_dir: &Path) -> Result<()> {
        let db = self.db.lock().unwrap();
        let tx = db.transaction()?;

        // Check if we already have data
        let device_count: i64 = tx.query_row("SELECT COUNT(*) FROM devices", [], |row| row.get(0))
            .unwrap_or(0);
        if device_count > 0 {
            tracing::info!("config store already has data, skipping JSON migration");
            return Ok(());
        }

        tracing::info!("migrating JSON files to SQLite config store");

        // Migrate registry JSON
        let registry_path = runtime_dir.join("bonsai-registry.json");
        if registry_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&registry_path) {
                if let Ok(registry_state) = serde_json::from_str::<crate::registry::RegistryState>(&raw) {
                    for (address, target) in registry_state.targets {
                        let paths_json = serde_json::to_string(&target.paths).unwrap_or_default();
                        tx.execute(
                            "INSERT INTO devices (address, hostname, vendor, role, site, collector_id, username, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                            params![
                                address,
                                target.hostname,
                                target.vendor,
                                target.role,
                                target.site,
                                target.collector_id,
                                target.username,
                                target.password_env,
                                target.ca_cert,
                                paths_json,
                                target.optional,
                                target.created_at_ns,
                                target.updated_at_ns,
                                target.created_by,
                                target.updated_by,
                                target.last_operator_action,
                            ],
                        ).ok();
                    }
                    tracing::info!("migrated {} devices from JSON", registry_state.targets.len());
                }
            }
        }

        // Migrate enricher configs JSON
        let enricher_path = runtime_dir.join("enrichment_configs.json");
        if enricher_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&enricher_path) {
                if let Ok(configs) = serde_json::from_str::<Vec<crate::enrichment::EnricherConfig>>(&raw) {
                    for config in configs {
                        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
                        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();
                        tx.execute(
                            "INSERT INTO enrichers (name, enricher_type, enabled, base_url, credential_alias, poll_interval_secs, environment_scope, extra) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                config.name,
                                config.enricher_type,
                                config.enabled,
                                config.base_url,
                                config.credential_alias,
                                config.poll_interval_secs,
                                scope_json,
                                extra_json,
                            ],
                        ).ok();
                    }
                    tracing::info!("migrated {} enrichers from JSON", configs.len());
                }
            }
        }

        // Migrate adapter configs JSON
        let adapter_path = runtime_dir.join("adapter_configs.json");
        if adapter_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&adapter_path) {
                if let Ok(configs) = serde_json::from_str::<Vec<crate::output::OutputAdapterConfig>>(&raw) {
                    for config in configs {
                        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
                        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();
                        tx.execute(
                            "INSERT INTO adapters (name, adapter_type, enabled, endpoint_url, credential_alias, topic, environment_scope, extra) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                config.name,
                                config.adapter_type,
                                config.enabled,
                                config.endpoint_url,
                                config.credential_alias,
                                config.topic,
                                scope_json,
                                extra_json,
                            ],
                        ).ok();
                    }
                    tracing::info!("migrated {} adapters from JSON", configs.len());
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Write an audit log entry.
    pub fn audit(&self, table: &str, operation: &str, key: &str, actor: &str, action: &str, old: Option<&str>, new: Option<&str>) -> Result<()> {
        let db = self.db.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        db.execute(
            "INSERT INTO audit_log (table_name, operation, record_key, actor, action, timestamp_ns, old_value, new_value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![table, operation, key, actor, action, now, old, new],
        )?;
        Ok(())
    }

    // ── Device registry ─────────────────────────────────────────────────────────

    pub fn list_devices(&self) -> Result<Vec<crate::config::TargetConfig>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT address, hostname, vendor, role, site, collector_id, username, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action FROM devices")?;
        let rows = stmt.query_map([], |row| {
            let paths_json: String = row.get(9)?;
            let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
            Ok(crate::config::TargetConfig {
                address: row.get(0)?,
                hostname: row.get(1)?,
                vendor: row.get(2)?,
                role: row.get(3)?,
                site: row.get(4)?,
                collector_id: row.get(5)?,
                username: row.get(6)?,
                password_env: row.get(7)?,
                ca_cert: row.get(8)?,
                paths,
                optional: row.get(10)?,
                created_at_ns: row.get(11)?,
                updated_at_ns: row.get(12)?,
                created_by: row.get(13)?,
                updated_by: row.get(14)?,
                last_operator_action: row.get(15)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| anyhow!("failed to collect devices: {e}"))
    }

    pub fn get_device(&self, address: &str) -> Result<Option<crate::config::TargetConfig>> {
        let db = self.db.lock().unwrap();
        let result = db.query_row(
            "SELECT address, hostname, vendor, role, site, collector_id, username, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action FROM devices WHERE address = ?1",
            params![address],
            |row| {
                let paths_json: String = row.get(9)?;
                let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
                Ok(crate::config::TargetConfig {
                    address: row.get(0)?,
                    hostname: row.get(1)?,
                    vendor: row.get(2)?,
                    role: row.get(3)?,
                    site: row.get(4)?,
                    collector_id: row.get(5)?,
                    username: row.get(6)?,
                    password_env: row.get(7)?,
                    ca_cert: row.get(8)?,
                    paths,
                    optional: row.get(10)?,
                    created_at_ns: row.get(11)?,
                    updated_at_ns: row.get(12)?,
                    created_by: row.get(13)?,
                    updated_by: row.get(14)?,
                    last_operator_action: row.get(15)?,
                })
            },
        );
        match result {
            Ok(d) => Ok(Some(d)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert_device(&self, device: &crate::config::TargetConfig, actor: &str, action: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        let old = self.get_device(&device.address)?.map(|d| serde_json::to_string(&d).unwrap());
        let new = serde_json::to_string(device).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let paths_json = serde_json::to_string(&device.paths).unwrap_or_default();

        db.execute(
            "INSERT INTO devices (address, hostname, vendor, role, site, collector_id, username, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT(address) DO UPDATE SET hostname=?2, vendor=?3, role=?4, site=?5, collector_id=?6, username=?7, password_env=?8, ca_cert=?9, paths=?10, optional=?11, updated_at_ns=?13, updated_by=?14, last_operator_action=?15",
            params![
                device.address,
                device.hostname,
                device.vendor,
                device.role,
                device.site,
                device.collector_id,
                device.username,
                device.password_env,
                device.ca_cert,
                paths_json,
                device.optional,
                device.created_at_ns,
                now,
                device.created_by,
                actor,
                action,
            ],
        )?;

        self.audit("devices", "UPSERT", &device.address, actor, action, old.as_deref(), Some(&new))?;

        // G3 Session 5: Publish config change to etcd
        if let Some(ref replicator) = self.config_replicator {
            let change = crate::ha_coordinator::ConfigChange {
                change_type: crate::ha_coordinator::ConfigChangeType::Upsert,
                table: "devices".to_string(),
                key: device.address.clone(),
                value: Some(new),
                timestamp_ns: now,
                node_id: replicator.node_id.clone(),
            };
            let replicator = Arc::clone(replicator);
            tokio::spawn(async move {
                if let Err(e) = replicator.publish_change(change).await {
                    tracing::error!(error = %e, "failed to publish config change");
                }
            });
        }

        Ok(())
    }

    pub fn delete_device(&self, address: &str, actor: &str) -> Result<()> {
        let old = self.get_device(address)?.map(|d| serde_json::to_string(&d).unwrap());
        let db = self.db.lock().unwrap();
        let rows = db.execute("DELETE FROM devices WHERE address = ?1", params![address])?;
        if rows > 0 {
            self.audit("devices", "DELETE", address, actor, "registry_remove_device", old.as_deref(), None)?;

            // G3 Session 5: Publish config change to etcd
            if let Some(ref replicator) = self.config_replicator {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                let change = crate::ha_coordinator::ConfigChange {
                    change_type: crate::ha_coordinator::ConfigChangeType::Delete,
                    table: "devices".to_string(),
                    key: address.to_string(),
                    value: None,
                    timestamp_ns: now,
                    node_id: replicator.node_id.clone(),
                };
                let replicator = Arc::clone(replicator);
                tokio::spawn(async move {
                    if let Err(e) = replicator.publish_change(change).await {
                        tracing::error!(error = %e, "failed to publish config change");
                    }
                });
            }
        }
        Ok(())
    }

    // ── Enricher registry ───────────────────────────────────────────────────────

    pub fn list_enrichers(&self) -> Result<Vec<crate::enrichment::EnricherConfig>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT name, enricher_type, enabled, base_url, credential_alias, poll_interval_secs, environment_scope, extra, created_at_ns, updated_at_ns FROM enrichers")?;
        let rows = stmt.query_map([], |row| {
            let scope_json: String = row.get(6)?;
            let scope: Vec<String> = serde_json::from_str(&scope_json).unwrap_or_default();
            let extra_json: String = row.get(7)?;
            let extra: serde_json::Value = serde_json::from_str(&extra_json).unwrap_or(serde_json::Value::Null);
            Ok(crate::enrichment::EnricherConfig {
                name: row.get(0)?,
                enricher_type: row.get(1)?,
                enabled: row.get(2)?,
                base_url: row.get(3)?,
                credential_alias: row.get(4)?,
                poll_interval_secs: row.get(5)?,
                environment_scope: scope,
                extra,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| anyhow!("failed to collect enrichers: {e}"))
    }

    pub fn upsert_enricher(&self, config: &crate::enrichment::EnricherConfig, actor: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        let old = self.list_enrichers()?.iter().find(|e| e.name == config.name).map(|d| serde_json::to_string(d).unwrap());
        let new = serde_json::to_string(config).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();

        db.execute(
            "INSERT INTO enrichers (name, enricher_type, enabled, base_url, credential_alias, poll_interval_secs, environment_scope, extra, created_at_ns, updated_at_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(name) DO UPDATE SET enricher_type=?2, enabled=?3, base_url=?4, credential_alias=?5, poll_interval_secs=?6, environment_scope=?7, extra=?8, updated_at_ns=?10",
            params![
                config.name,
                config.enricher_type,
                config.enabled,
                config.base_url,
                config.credential_alias,
                config.poll_interval_secs,
                scope_json,
                extra_json,
                now,
                now,
            ],
        )?;

        self.audit("enrichers", "UPSERT", &config.name, actor, "enricher_upsert", old.as_deref(), Some(&new))?;

        // G3 Session 5: Publish config change to etcd
        if let Some(ref replicator) = self.config_replicator {
            let change = crate::ha_coordinator::ConfigChange {
                change_type: crate::ha_coordinator::ConfigChangeType::Upsert,
                table: "enrichers".to_string(),
                key: config.name.clone(),
                value: Some(new),
                timestamp_ns: now,
                node_id: replicator.node_id.clone(),
            };
            let replicator = Arc::clone(replicator);
            tokio::spawn(async move {
                if let Err(e) = replicator.publish_change(change).await {
                    tracing::error!(error = %e, "failed to publish config change");
                }
            });
        }

        Ok(())
    }

    pub fn delete_enricher(&self, name: &str, actor: &str) -> Result<()> {
        let old = self.list_enrichers()?.iter().find(|e| e.name == name).map(|d| serde_json::to_string(d).unwrap());
        let db = self.db.lock().unwrap();
        let rows = db.execute("DELETE FROM enrichers WHERE name = ?1", params![name])?;
        if rows > 0 {
            self.audit("enrichers", "DELETE", name, actor, "enricher_remove", old.as_deref(), None)?;

            // G3 Session 5: Publish config change to etcd
            if let Some(ref replicator) = self.config_replicator {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                let change = crate::ha_coordinator::ConfigChange {
                    change_type: crate::ha_coordinator::ConfigChangeType::Delete,
                    table: "enrichers".to_string(),
                    key: name.to_string(),
                    value: None,
                    timestamp_ns: now,
                    node_id: replicator.node_id.clone(),
                };
                let replicator = Arc::clone(replicator);
                tokio::spawn(async move {
                    if let Err(e) = replicator.publish_change(change).await {
                        tracing::error!(error = %e, "failed to publish config change");
                    }
                });
            }
        }
        Ok(())
    }

    // ── Adapter registry ───────────────────────────────────────────────────────

    pub fn list_adapters(&self) -> Result<Vec<crate::output::OutputAdapterConfig>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT name, adapter_type, enabled, endpoint_url, credential_alias, topic, environment_scope, extra, created_at_ns, updated_at_ns FROM adapters")?;
        let rows = stmt.query_map([], |row| {
            let scope_json: String = row.get(6)?;
            let scope: Vec<String> = serde_json::from_str(&scope_json).unwrap_or_default();
            let extra_json: String = row.get(7)?;
            let extra: serde_json::Value = serde_json::from_str(&extra_json).unwrap_or(serde_json::Value::Null);
            Ok(crate::output::OutputAdapterConfig {
                name: row.get(0)?,
                adapter_type: row.get(1)?,
                enabled: row.get(2)?,
                endpoint_url: row.get(3)?,
                credential_alias: row.get(4)?,
                topic: row.get(5)?,
                environment_scope: scope,
                extra,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| anyhow!("failed to collect adapters: {e}"))
    }

    pub fn upsert_adapter(&self, config: &crate::output::OutputAdapterConfig, actor: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        let old = self.list_adapters()?.iter().find(|a| a.name == config.name).map(|d| serde_json::to_string(d).unwrap());
        let new = serde_json::to_string(config).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();

        db.execute(
            "INSERT INTO adapters (name, adapter_type, enabled, endpoint_url, credential_alias, topic, environment_scope, extra, created_at_ns, updated_at_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(name) DO UPDATE SET adapter_type=?2, enabled=?3, endpoint_url=?4, credential_alias=?5, topic=?6, environment_scope=?7, extra=?8, updated_at_ns=?10",
            params![
                config.name,
                config.adapter_type,
                config.enabled,
                config.endpoint_url,
                config.credential_alias,
                config.topic,
                scope_json,
                extra_json,
                now,
                now,
            ],
        )?;

        self.audit("adapters", "UPSERT", &config.name, actor, "adapter_upsert", old.as_deref(), Some(&new))?;

        // G3 Session 5: Publish config change to etcd
        if let Some(ref replicator) = self.config_replicator {
            let change = crate::ha_coordinator::ConfigChange {
                change_type: crate::ha_coordinator::ConfigChangeType::Upsert,
                table: "adapters".to_string(),
                key: config.name.clone(),
                value: Some(new),
                timestamp_ns: now,
                node_id: replicator.node_id.clone(),
            };
            let replicator = Arc::clone(replicator);
            tokio::spawn(async move {
                if let Err(e) = replicator.publish_change(change).await {
                    tracing::error!(error = %e, "failed to publish config change");
                }
            });
        }

        Ok(())
    }

    pub fn delete_adapter(&self, name: &str, actor: &str) -> Result<()> {
        let old = self.list_adapters()?.iter().find(|a| a.name == name).map(|d| serde_json::to_string(d).unwrap());
        let db = self.db.lock().unwrap();
        let rows = db.execute("DELETE FROM adapters WHERE name = ?1", params![name])?;
        if rows > 0 {
            self.audit("adapters", "DELETE", name, actor, "adapter_remove", old.as_deref(), None)?;

            // G3 Session 5: Publish config change to etcd
            if let Some(ref replicator) = self.config_replicator {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                    .unwrap_or(0);
                let change = crate::ha_coordinator::ConfigChange {
                    change_type: crate::ha_coordinator::ConfigChangeType::Delete,
                    table: "adapters".to_string(),
                    key: name.to_string(),
                    value: None,
                    timestamp_ns: now,
                    node_id: replicator.node_id.clone(),
                };
                let replicator = Arc::clone(replicator);
                tokio::spawn(async move {
                    if let Err(e) = replicator.publish_change(change).await {
                        tracing::error!(error = %e, "failed to publish config change");
                    }
                });
            }
        }
        Ok(())
    }

    // ── Collector registration audit ─────────────────────────────────────────────

    pub fn log_collector_registration(
        &self,
        collector_id: &str,
        hostname: &str,
        protocol_version: u32,
        peer_ip: Option<&str>,
        cert_fingerprint: Option<&str>,
        success: bool,
        rejection_reason: Option<&str>,
    ) -> Result<()> {
        let db = self.db.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        db.execute(
            "INSERT INTO collector_registrations (collector_id, hostname, protocol_version, peer_ip, cert_fingerprint, timestamp_ns, success, rejection_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                collector_id,
                hostname,
                protocol_version as i64,
                peer_ip,
                cert_fingerprint,
                now,
                success,
                rejection_reason,
            ],
        )?;
        Ok(())
    }
}

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
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// File name of the config database inside a runtime directory.
pub const CONFIG_DB_FILE_NAME: &str = "bonsai_config.db";
const DB_FILE: &str = CONFIG_DB_FILE_NAME;
/// Schema version the management service migrates to. Read-only
/// consumers (routing service) compare against this and fail closed
/// when the database is older.
pub const CURRENT_SCHEMA_VERSION: i64 = 4;

/// SQLite config store with versioned schema and audit trail.
pub struct SqliteStore {
    db: Arc<Mutex<Connection>>,
    config_replicator: Option<Arc<crate::ha_coordinator::ConfigReplicator>>,
}

impl SqliteStore {
    /// Open or create the config store at an explicit database file path,
    /// running migrations if needed. Used by the management service.
    pub fn open_file(db_path: &Path) -> Result<Self> {
        let db = Connection::open(db_path)
            .with_context(|| format!("failed to open SQLite DB at '{}'", db_path.display()))?;

        let store = Self {
            db: Arc::new(Mutex::new(db)),
            config_replicator: None,
        };

        store.init_schema()?;
        store.enable_wal()?;

        Ok(store)
    }

    /// Open the config store READ-ONLY for consumers that must never
    /// mutate the shared management database (BN-021c routing service).
    ///
    /// Unlike [`Self::open_file`] this performs NO schema migration and
    /// does NOT enable WAL — a read-only handle cannot create tables,
    /// bump `user_version`, or change journal mode even accidentally.
    /// The database is expected to already be at the current schema
    /// (managed by the `bonsai` service); callers can inspect
    /// [`Self::schema_version`] and must fail closed when binding
    /// tables are absent. Deferred lifecycle limitation: binding
    /// changes are only visible to NEW connections — an already
    /// resolved `exp:*` speaker keeps its scope until reconnect.
    pub fn open_file_readonly(db_path: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| {
            format!(
                "failed to open SQLite DB read-only at '{}'",
                db_path.display()
            )
        })?;

        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            config_replicator: None,
        })
    }

    /// Current schema version recorded in the database (`user_version`).
    /// Read-only safe; returns 0 for a pre-versioned/empty database.
    pub fn schema_version(&self) -> Result<i64> {
        let db = self.db.lock().unwrap();
        db.query_row("PRAGMA user_version", [], |row| row.get(0))
            .context("failed to read SQLite schema version")
    }

    /// Open or create the config store, running migrations if needed.
    pub fn open(runtime_dir: &Path) -> Result<Self> {
        Self::open_file(&runtime_dir.join(DB_FILE))
    }

    fn init_schema(&self) -> Result<()> {
        self.migrate()
    }

    fn enable_wal(&self) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(())
    }

    /// Run schema migrations to bring DB to current version.
    fn migrate(&self) -> Result<()> {
        let db = self.db.lock().unwrap();
        let user_version: i64 = db
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        if user_version == CURRENT_SCHEMA_VERSION {
            return Ok(());
        }

        tracing::info!(
            from = user_version,
            to = CURRENT_SCHEMA_VERSION,
            "migrating config store schema"
        );

        // Version 1: Initial schema
        if user_version < 1 {
            db.execute_batch(r#"
                CREATE TABLE IF NOT EXISTS devices (
                    address TEXT PRIMARY KEY,
                    enabled INTEGER DEFAULT 1,
                    hostname TEXT,
                    tls_domain TEXT,
                    vendor TEXT,
                    credential_alias TEXT,
                    username_env TEXT,
                    role TEXT,
                    site TEXT,
                    collector_id TEXT,
                    username TEXT,
                    password TEXT,
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
        }

        if user_version < 2 {
            let _ = db.execute(
                "ALTER TABLE devices ADD COLUMN enabled INTEGER DEFAULT 1",
                [],
            );
            let _ = db.execute("ALTER TABLE devices ADD COLUMN tls_domain TEXT", []);
            let _ = db.execute("ALTER TABLE devices ADD COLUMN credential_alias TEXT", []);
            let _ = db.execute("ALTER TABLE devices ADD COLUMN username_env TEXT", []);
            let _ = db.execute("ALTER TABLE devices ADD COLUMN password TEXT", []);

            db.execute("PRAGMA user_version=2", [])
                .context("failed to set schema version")?;
        }

        // Version 3: BN-021b canonical device identity + trusted exporter bindings.
        // Additive only — existing `devices` rows and address-keyed consumers
        // are untouched.
        if user_version < 3 {
            db.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS canonical_devices (
                    device_id TEXT PRIMARY KEY,
                    address TEXT NOT NULL,
                    site TEXT,
                    hostname TEXT,
                    created_at_ns INTEGER,
                    updated_at_ns INTEGER,
                    created_by TEXT,
                    updated_by TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_canonical_devices_address
                    ON canonical_devices(address);

                CREATE TABLE IF NOT EXISTS exporter_bindings (
                    binding_id TEXT PRIMARY KEY,
                    scope TEXT NOT NULL,
                    protocol TEXT NOT NULL,
                    exporter_id TEXT NOT NULL,
                    device_id TEXT NOT NULL,
                    match_transport TEXT,
                    match_reported_identity TEXT,
                    match_exporter_claim TEXT,
                    match_observation_domain INTEGER,
                    provenance TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at_ns INTEGER,
                    updated_at_ns INTEGER,
                    created_by TEXT,
                    updated_by TEXT
                );

                -- One canonical exporter identity per (scope, protocol).
                CREATE UNIQUE INDEX IF NOT EXISTS idx_exporter_bindings_canonical
                    ON exporter_bindings(scope, protocol, exporter_id);
                CREATE INDEX IF NOT EXISTS idx_exporter_bindings_device
                    ON exporter_bindings(device_id);
            "#,
            )
            .context("failed to create schema v3")?;

            db.execute("PRAGMA user_version=3", [])
                .context("failed to set schema version")?;
        }

        // Version 4: BN-021f verified-ingress credential pinning.
        // Additive only — `match_ingress_credential` stores the
        // "sha256:<hex>" fingerprint of the authorized mTLS client
        // certificate. NULL means the binding can never authorize a
        // verified-ingress resolution (and remains claim-matchable in
        // legacy compatibility mode).
        if user_version < 4 {
            let _ = db.execute(
                "ALTER TABLE exporter_bindings ADD COLUMN match_ingress_credential TEXT",
                [],
            );

            db.execute("PRAGMA user_version=4", [])
                .context("failed to set schema version")?;
        }

        Ok(())
    }

    /// Migrate existing JSON files to SQLite on first run.
    pub fn migrate_from_json(&self, runtime_dir: &Path) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;

        // Check if we already have data
        let device_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM devices", [], |row| row.get(0))
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
                if let Ok(registry_state) =
                    serde_json::from_str::<crate::registry::RegistryState>(&raw)
                {
                    let count = registry_state.targets.len();
                    for (address, target) in registry_state.targets {
                        let paths_json = serde_json::to_string(&target.paths).unwrap_or_default();
                        tx.execute(
                            "INSERT INTO devices (address, enabled, hostname, tls_domain, vendor, credential_alias, username_env, role, site, collector_id, username, password, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                            params![
                                address,
                                target.enabled,
                                target.hostname,
                                target.tls_domain,
                                target.vendor,
                                target.credential_alias,
                                target.username_env,
                                target.role,
                                target.site,
                                target.collector_id,
                                target.username,
                                target.password,
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
                    tracing::info!("migrated {} devices from JSON", count);
                }
            }
        }

        // Migrate enricher configs JSON
        let enricher_path = runtime_dir.join("enrichment_configs.json");
        if enricher_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&enricher_path) {
                if let Ok(configs) =
                    serde_json::from_str::<Vec<crate::enrichment::EnricherConfig>>(&raw)
                {
                    let count = configs.len();
                    for config in configs {
                        let scope_json =
                            serde_json::to_string(&config.environment_scope).unwrap_or_default();
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
                    tracing::info!("migrated {} enrichers from JSON", count);
                }
            }
        }

        // Migrate adapter configs JSON
        let adapter_path = runtime_dir.join("adapter_configs.json");
        if adapter_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&adapter_path) {
                if let Ok(configs) =
                    serde_json::from_str::<Vec<crate::output::OutputAdapterConfig>>(&raw)
                {
                    let count = configs.len();
                    for config in configs {
                        let scope_json =
                            serde_json::to_string(&config.environment_scope).unwrap_or_default();
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
                    tracing::info!("migrated {} adapters from JSON", count);
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Write an audit log entry.
    pub fn audit(
        &self,
        table: &str,
        operation: &str,
        key: &str,
        actor: &str,
        action: &str,
        old: Option<&str>,
        new: Option<&str>,
    ) -> Result<()> {
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
        let mut stmt = db.prepare("SELECT address, enabled, hostname, tls_domain, vendor, credential_alias, username_env, role, site, collector_id, username, password, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action FROM devices")?;
        let rows = stmt.query_map([], |row| {
            let paths_json: String = row.get(14)?;
            let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
            Ok(crate::config::TargetConfig {
                address: row.get(0)?,
                enabled: row.get(1)?,
                hostname: row.get(2)?,
                tls_domain: row.get(3)?,
                vendor: row.get(4)?,
                credential_alias: row.get(5)?,
                username_env: row.get(6)?,
                role: row.get(7)?,
                site: row.get(8)?,
                collector_id: row.get(9)?,
                username: row.get(10)?,
                password: row.get(11)?,
                password_env: row.get(12)?,
                ca_cert: row.get(13)?,
                paths,
                optional: row.get(15)?,
                created_at_ns: row.get(16)?,
                updated_at_ns: row.get(17)?,
                created_by: row.get(18)?,
                updated_by: row.get(19)?,
                last_operator_action: row.get(20)?,
                ..Default::default()
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("failed to collect devices: {e}"))
    }

    pub fn get_device(&self, address: &str) -> Result<Option<crate::config::TargetConfig>> {
        let db = self.db.lock().unwrap();
        let result = db.query_row(
            "SELECT address, enabled, hostname, tls_domain, vendor, credential_alias, username_env, role, site, collector_id, username, password, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action FROM devices WHERE address = ?1",
            params![address],
            |row| {
                let paths_json: String = row.get(14)?;
                let paths: Vec<String> = serde_json::from_str(&paths_json).unwrap_or_default();
                Ok(crate::config::TargetConfig {
                    address: row.get(0)?,
                    enabled: row.get(1)?,
                    hostname: row.get(2)?,
                    tls_domain: row.get(3)?,
                    vendor: row.get(4)?,
                    credential_alias: row.get(5)?,
                    username_env: row.get(6)?,
                    role: row.get(7)?,
                    site: row.get(8)?,
                    collector_id: row.get(9)?,
                    username: row.get(10)?,
                    password: row.get(11)?,
                    password_env: row.get(12)?,
                    ca_cert: row.get(13)?,
                    paths,
                    optional: row.get(15)?,
                    created_at_ns: row.get(16)?,
                    updated_at_ns: row.get(17)?,
                    created_by: row.get(18)?,
                    updated_by: row.get(19)?,
                    last_operator_action: row.get(20)?,
                    ..Default::default()
                })
            },
        );
        match result {
            Ok(d) => Ok(Some(d)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert_device(
        &self,
        device: &crate::config::TargetConfig,
        actor: &str,
        action: &str,
    ) -> Result<()> {
        let old = self
            .get_device(&device.address)?
            .map(|d| serde_json::to_string(&d).unwrap());
        let new = serde_json::to_string(device).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let paths_json = serde_json::to_string(&device.paths).unwrap_or_default();

        {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO devices (address, enabled, hostname, tls_domain, vendor, credential_alias, username_env, role, site, collector_id, username, password, password_env, ca_cert, paths, optional, created_at_ns, updated_at_ns, created_by, updated_by, last_operator_action) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21) ON CONFLICT(address) DO UPDATE SET enabled=?2, hostname=?3, tls_domain=?4, vendor=?5, credential_alias=?6, username_env=?7, role=?8, site=?9, collector_id=?10, username=?11, password=?12, password_env=?13, ca_cert=?14, paths=?15, optional=?16, updated_at_ns=?18, updated_by=?20, last_operator_action=?21",
                params![
                    device.address,
                    device.enabled,
                    device.hostname,
                    device.tls_domain,
                    device.vendor,
                    device.credential_alias,
                    device.username_env,
                    device.role,
                    device.site,
                    device.collector_id,
                    device.username,
                    device.password,
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
        }

        self.audit(
            "devices",
            "UPSERT",
            &device.address,
            actor,
            action,
            old.as_deref(),
            Some(&new),
        )?;

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
        let old = self
            .get_device(address)?
            .map(|d| serde_json::to_string(&d).unwrap());
        let rows = {
            let db = self.db.lock().unwrap();
            db.execute("DELETE FROM devices WHERE address = ?1", params![address])?
        };
        if rows > 0 {
            self.audit(
                "devices",
                "DELETE",
                address,
                actor,
                "registry_remove_device",
                old.as_deref(),
                None,
            )?;

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
            let extra: serde_json::Value =
                serde_json::from_str(&extra_json).unwrap_or(serde_json::Value::Null);
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
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("failed to collect enrichers: {e}"))
    }

    pub fn upsert_enricher(
        &self,
        config: &crate::enrichment::EnricherConfig,
        actor: &str,
    ) -> Result<()> {
        let old = self
            .list_enrichers()?
            .iter()
            .find(|e| e.name == config.name)
            .map(|d| serde_json::to_string(d).unwrap());
        let new = serde_json::to_string(config).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();

        {
            let db = self.db.lock().unwrap();
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
        }

        self.audit(
            "enrichers",
            "UPSERT",
            &config.name,
            actor,
            "enricher_upsert",
            old.as_deref(),
            Some(&new),
        )?;

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
        let old = self
            .list_enrichers()?
            .iter()
            .find(|e| e.name == name)
            .map(|d| serde_json::to_string(d).unwrap());
        let rows = {
            let db = self.db.lock().unwrap();
            db.execute("DELETE FROM enrichers WHERE name = ?1", params![name])?
        };
        if rows > 0 {
            self.audit(
                "enrichers",
                "DELETE",
                name,
                actor,
                "enricher_remove",
                old.as_deref(),
                None,
            )?;

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
            let extra: serde_json::Value =
                serde_json::from_str(&extra_json).unwrap_or(serde_json::Value::Null);
            Ok(crate::output::OutputAdapterConfig {
                name: row.get(0)?,
                adapter_type: row.get(1)?,
                enabled: row.get(2)?,
                endpoint_url: row.get(3)?,
                credential_alias: row.get(4)?,
                topic: row.get(5)?,
                environment_scope: scope,
                extra,
                flush_interval_secs: 30,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("failed to collect adapters: {e}"))
    }

    pub fn upsert_adapter(
        &self,
        config: &crate::output::OutputAdapterConfig,
        actor: &str,
    ) -> Result<()> {
        let old = self
            .list_adapters()?
            .iter()
            .find(|a| a.name == config.name)
            .map(|d| serde_json::to_string(d).unwrap());
        let new = serde_json::to_string(config).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        let scope_json = serde_json::to_string(&config.environment_scope).unwrap_or_default();
        let extra_json = serde_json::to_string(&config.extra).unwrap_or_default();

        {
            let db = self.db.lock().unwrap();
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
        }

        self.audit(
            "adapters",
            "UPSERT",
            &config.name,
            actor,
            "adapter_upsert",
            old.as_deref(),
            Some(&new),
        )?;

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
        let old = self
            .list_adapters()?
            .iter()
            .find(|a| a.name == name)
            .map(|d| serde_json::to_string(d).unwrap());
        let rows = {
            let db = self.db.lock().unwrap();
            db.execute("DELETE FROM adapters WHERE name = ?1", params![name])?
        };
        if rows > 0 {
            self.audit(
                "adapters",
                "DELETE",
                name,
                actor,
                "adapter_remove",
                old.as_deref(),
                None,
            )?;

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

    // ── Canonical device identity (BN-021b) ─────────────────────────────────
    //
    // `canonical_devices` maps an opaque, stable `device_id` to the
    // device's CURRENT address. The `devices` table remains address-keyed
    // for existing consumers; these tables are additive and do not
    // re-key it.

    /// Generate a fresh canonical device id.
    pub fn new_canonical_device_id() -> String {
        format!("dev-{}", uuid::Uuid::new_v4())
    }

    /// Generate a fresh binding id.
    pub fn new_binding_id() -> String {
        format!("bind-{}", uuid::Uuid::new_v4())
    }

    /// Register or update a canonical device. `device_id` is the stable
    /// key: re-upserting with the same `device_id` and a new `address`
    /// models a management-address change — same canonical identity, no
    /// duplicate device. An empty `device_id` mints a new one.
    pub fn upsert_canonical_device(
        &self,
        device: &crate::telemetry_identity::CanonicalDevice,
        actor: &str,
    ) -> Result<crate::telemetry_identity::CanonicalDevice> {
        use crate::telemetry_identity::CanonicalDevice;
        let now = now_ns();
        let existing = if device.device_id.is_empty() {
            None
        } else {
            self.get_canonical_device(&device.device_id)?
        };

        let stored = CanonicalDevice {
            device_id: if device.device_id.is_empty() {
                Self::new_canonical_device_id()
            } else {
                device.device_id.clone()
            },
            address: device.address.trim().to_string(),
            site: device.site.clone(),
            hostname: device.hostname.clone(),
            created_at_ns: existing.as_ref().map(|d| d.created_at_ns).unwrap_or(now),
            updated_at_ns: now,
            created_by: existing
                .as_ref()
                .map(|d| d.created_by.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| actor.to_string()),
            updated_by: actor.to_string(),
        };
        if stored.address.is_empty() {
            anyhow::bail!("canonical device address must not be empty");
        }

        let old = existing
            .as_ref()
            .map(|d| serde_json::to_string(d).unwrap_or_default());
        let new = serde_json::to_string(&stored).unwrap_or_default();

        {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO canonical_devices (device_id, address, site, hostname, created_at_ns, updated_at_ns, created_by, updated_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(device_id) DO UPDATE SET address=?2, site=?3, hostname=?4, updated_at_ns=?6, updated_by=?8",
                params![
                    stored.device_id,
                    stored.address,
                    stored.site,
                    stored.hostname,
                    stored.created_at_ns,
                    stored.updated_at_ns,
                    stored.created_by,
                    stored.updated_by,
                ],
            )?;
        }

        self.audit(
            "canonical_devices",
            "UPSERT",
            &stored.device_id,
            actor,
            "canonical_device_upsert",
            old.as_deref(),
            Some(&new),
        )?;

        Ok(stored)
    }

    pub fn get_canonical_device(
        &self,
        device_id: &str,
    ) -> Result<Option<crate::telemetry_identity::CanonicalDevice>> {
        use crate::telemetry_identity::CanonicalDevice;
        let db = self.db.lock().unwrap();
        let result = db.query_row(
            "SELECT device_id, address, site, hostname, created_at_ns, updated_at_ns, created_by, updated_by FROM canonical_devices WHERE device_id = ?1",
            params![device_id],
            |row| {
                Ok(CanonicalDevice {
                    device_id: row.get(0)?,
                    address: row.get(1)?,
                    site: row.get(2)?,
                    hostname: row.get(3)?,
                    created_at_ns: row.get(4)?,
                    updated_at_ns: row.get(5)?,
                    created_by: row.get(6)?,
                    updated_by: row.get(7)?,
                })
            },
        );
        match result {
            Ok(d) => Ok(Some(d)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// All canonical devices currently registered at `address`. May
    /// return multiple rows across scopes/sites — address is evidence,
    /// not identity.
    pub fn find_canonical_devices_by_address(
        &self,
        address: &str,
    ) -> Result<Vec<crate::telemetry_identity::CanonicalDevice>> {
        use crate::telemetry_identity::CanonicalDevice;
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT device_id, address, site, hostname, created_at_ns, updated_at_ns, created_by, updated_by FROM canonical_devices WHERE address = ?1",
        )?;
        let rows = stmt.query_map(params![address], |row| {
            Ok(CanonicalDevice {
                device_id: row.get(0)?,
                address: row.get(1)?,
                site: row.get(2)?,
                hostname: row.get(3)?,
                created_at_ns: row.get(4)?,
                updated_at_ns: row.get(5)?,
                created_by: row.get(6)?,
                updated_by: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("failed to collect canonical devices: {e}"))
    }

    pub fn list_canonical_devices(
        &self,
    ) -> Result<Vec<crate::telemetry_identity::CanonicalDevice>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT device_id, address, site, hostname, created_at_ns, updated_at_ns, created_by, updated_by FROM canonical_devices",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::telemetry_identity::CanonicalDevice {
                device_id: row.get(0)?,
                address: row.get(1)?,
                site: row.get(2)?,
                hostname: row.get(3)?,
                created_at_ns: row.get(4)?,
                updated_at_ns: row.get(5)?,
                created_by: row.get(6)?,
                updated_by: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow!("failed to collect canonical devices: {e}"))
    }

    // ── Trusted exporter bindings (BN-021b) ─────────────────────────────────
    //
    // Bindings are created ONLY through these provisioning methods
    // (operator/inventory paths). Telemetry ingestion must never call
    // them; there is no API that derives a binding from observed data.

    /// Create or update a trusted exporter binding.
    ///
    /// Validation (fail-closed):
    /// - scope/protocol/exporter_id/device_id must be non-empty;
    /// - `device_id` must reference an existing canonical device;
    /// - no other ACTIVE binding may already own (scope, protocol,
    ///   exporter_id) — canonical exporter ids are unique per scope;
    /// - the new binding must not overlap an existing ACTIVE binding
    ///   that resolves to a different (device_id, exporter_id) pair —
    ///   overlapping claims are rejected rather than arbitrarily
    ///   first-match selected.
    ///
    /// Re-upserting the same `binding_id` updates match constraints
    /// idempotently.
    pub fn upsert_exporter_binding(
        &self,
        binding: &crate::telemetry_identity::TrustedBinding,
        actor: &str,
    ) -> Result<crate::telemetry_identity::TrustedBinding> {
        use crate::telemetry_identity::{BindingStatus, TrustedBinding};

        for (name, value) in [
            ("scope", binding.scope.as_str()),
            ("protocol", binding.protocol.as_str()),
            ("exporter_id", binding.exporter_id.as_str()),
            ("device_id", binding.device_id.as_str()),
        ] {
            if value.trim().is_empty() {
                anyhow::bail!("exporter binding {name} must not be empty");
            }
        }
        if self.get_canonical_device(&binding.device_id)?.is_none() {
            anyhow::bail!(
                "exporter binding references unknown canonical device '{}'",
                binding.device_id
            );
        }

        let now = now_ns();
        let existing = self.get_exporter_binding(&binding.binding_id)?;

        let stored = TrustedBinding {
            binding_id: binding.binding_id.clone(),
            scope: binding.scope.trim().to_string(),
            protocol: binding.protocol.trim().to_string(),
            exporter_id: binding.exporter_id.trim().to_string(),
            device_id: binding.device_id.trim().to_string(),
            match_transport: binding.match_transport.clone(),
            match_reported_identity: binding.match_reported_identity.clone(),
            match_exporter_claim: binding.match_exporter_claim.clone(),
            match_observation_domain: binding.match_observation_domain,
            match_ingress_credential: binding.match_ingress_credential.clone(),
            provenance: binding.provenance,
            status: BindingStatus::Active,
            created_at_ns: existing.as_ref().map(|b| b.created_at_ns).unwrap_or(now),
            updated_at_ns: now,
            created_by: existing
                .as_ref()
                .map(|b| b.created_by.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| actor.to_string()),
            updated_by: actor.to_string(),
        };

        // Reject conflicting claims against other active bindings.
        for other in
            self.list_exporter_bindings(Some(&stored.scope), Some(&stored.protocol), true)?
        {
            if other.binding_id == stored.binding_id || !other.is_active() {
                continue;
            }
            let same_canonical = other.scope == stored.scope
                && other.protocol == stored.protocol
                && other.exporter_id == stored.exporter_id;
            if same_canonical {
                anyhow::bail!(
                    "exporter identity '{}'/{} is already bound by '{}'",
                    stored.protocol,
                    stored.exporter_id,
                    other.binding_id
                );
            }
            if crate::telemetry_identity::bindings_overlap(&stored, &other)
                && (other.device_id != stored.device_id || other.exporter_id != stored.exporter_id)
            {
                anyhow::bail!(
                    "binding overlaps active binding '{}' with a different canonical pair",
                    other.binding_id
                );
            }
        }

        let old = existing
            .as_ref()
            .map(|b| serde_json::to_string(b).unwrap_or_default());
        let new = serde_json::to_string(&stored).unwrap_or_default();

        {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO exporter_bindings (binding_id, scope, protocol, exporter_id, device_id, match_transport, match_reported_identity, match_exporter_claim, match_observation_domain, match_ingress_credential, provenance, status, created_at_ns, updated_at_ns, created_by, updated_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) ON CONFLICT(binding_id) DO UPDATE SET scope=?2, protocol=?3, exporter_id=?4, device_id=?5, match_transport=?6, match_reported_identity=?7, match_exporter_claim=?8, match_observation_domain=?9, match_ingress_credential=?10, provenance=?11, status=?12, updated_at_ns=?14, updated_by=?16",
                params![
                    stored.binding_id,
                    stored.scope,
                    stored.protocol,
                    stored.exporter_id,
                    stored.device_id,
                    stored.match_transport,
                    stored.match_reported_identity,
                    stored.match_exporter_claim,
                    stored.match_observation_domain.map(|d| d as i64),
                    stored.match_ingress_credential,
                    provenance_str(stored.provenance),
                    status_str(stored.status),
                    stored.created_at_ns,
                    stored.updated_at_ns,
                    stored.created_by,
                    stored.updated_by,
                ],
            )?;
        }

        self.audit(
            "exporter_bindings",
            "UPSERT",
            &stored.binding_id,
            actor,
            "exporter_binding_upsert",
            old.as_deref(),
            Some(&new),
        )?;

        Ok(stored)
    }

    pub fn get_exporter_binding(
        &self,
        binding_id: &str,
    ) -> Result<Option<crate::telemetry_identity::TrustedBinding>> {
        let db = self.db.lock().unwrap();
        let result = db.query_row(
            "SELECT binding_id, scope, protocol, exporter_id, device_id, match_transport, match_reported_identity, match_exporter_claim, match_observation_domain, match_ingress_credential, provenance, status, created_at_ns, updated_at_ns, created_by, updated_by FROM exporter_bindings WHERE binding_id = ?1",
            params![binding_id],
            |row| binding_from_row(row),
        );
        match result {
            Ok(b) => Ok(Some(b)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List bindings, optionally filtered by scope/protocol. Revoked
    /// bindings are excluded unless `include_revoked` is set — revoked
    /// bindings must never claim observations.
    pub fn list_exporter_bindings(
        &self,
        scope: Option<&str>,
        protocol: Option<&str>,
        include_revoked: bool,
    ) -> Result<Vec<crate::telemetry_identity::TrustedBinding>> {
        let db = self.db.lock().unwrap();
        let mut sql = String::from(
            "SELECT binding_id, scope, protocol, exporter_id, device_id, match_transport, match_reported_identity, match_exporter_claim, match_observation_domain, match_ingress_credential, provenance, status, created_at_ns, updated_at_ns, created_by, updated_by FROM exporter_bindings WHERE 1=1",
        );
        if !include_revoked {
            sql.push_str(" AND status != 'revoked'");
        }
        if scope.is_some() {
            sql.push_str(" AND scope = ?1");
        }
        if protocol.is_some() {
            sql.push_str(if scope.is_some() {
                " AND protocol = ?2"
            } else {
                " AND protocol = ?1"
            });
        }
        let mut stmt = db.prepare(&sql)?;
        let params_vec: Vec<rusqlite::types::Value> = match (scope, protocol) {
            (Some(s), Some(p)) => vec![
                rusqlite::types::Value::Text(s.to_string()),
                rusqlite::types::Value::Text(p.to_string()),
            ],
            (Some(s), None) => vec![rusqlite::types::Value::Text(s.to_string())],
            (None, Some(p)) => vec![rusqlite::types::Value::Text(p.to_string())],
            (None, None) => vec![],
        };
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
            binding_from_row(row)
        })?;
        rows.collect::<Result<Vec<crate::telemetry_identity::TrustedBinding>, _>>()
            .map_err(|e| anyhow!("failed to collect exporter bindings: {e}"))
    }

    /// Explicitly invalidate a binding. Revocation is the only supported
    /// "unbind" operation — bindings are never silently deleted, keeping
    /// the audit trail intact.
    pub fn revoke_exporter_binding(
        &self,
        binding_id: &str,
        actor: &str,
    ) -> Result<crate::telemetry_identity::TrustedBinding> {
        use crate::telemetry_identity::BindingStatus;
        let existing = self
            .get_exporter_binding(binding_id)?
            .ok_or_else(|| anyhow!("exporter binding '{binding_id}' does not exist"))?;
        let now = now_ns();
        let mut updated = existing.clone();
        updated.status = BindingStatus::Revoked;
        updated.updated_at_ns = now;
        updated.updated_by = actor.to_string();

        {
            let db = self.db.lock().unwrap();
            db.execute(
                "UPDATE exporter_bindings SET status = ?2, updated_at_ns = ?3, updated_by = ?4 WHERE binding_id = ?1",
                params![
                    binding_id,
                    status_str(updated.status),
                    updated.updated_at_ns,
                    updated.updated_by,
                ],
            )?;
        }

        self.audit(
            "exporter_bindings",
            "REVOKE",
            binding_id,
            actor,
            "exporter_binding_revoke",
            Some(&serde_json::to_string(&existing).unwrap_or_default()),
            Some(&serde_json::to_string(&updated).unwrap_or_default()),
        )?;

        Ok(updated)
    }
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn provenance_str(p: crate::telemetry_identity::BindingProvenance) -> &'static str {
    use crate::telemetry_identity::BindingProvenance::*;
    match p {
        OperatorProvisioned => "operator_provisioned",
        InventoryImport => "inventory_import",
        Migration => "migration",
    }
}

fn provenance_from_str(s: &str) -> crate::telemetry_identity::BindingProvenance {
    use crate::telemetry_identity::BindingProvenance::*;
    match s {
        "inventory_import" => InventoryImport,
        "migration" => Migration,
        _ => OperatorProvisioned,
    }
}

fn status_str(s: crate::telemetry_identity::BindingStatus) -> &'static str {
    use crate::telemetry_identity::BindingStatus::*;
    match s {
        Active => "active",
        Revoked => "revoked",
    }
}

fn status_from_str(s: &str) -> crate::telemetry_identity::BindingStatus {
    use crate::telemetry_identity::BindingStatus::*;
    match s {
        "revoked" => Revoked,
        _ => Active,
    }
}

fn binding_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::telemetry_identity::TrustedBinding> {
    let domain: Option<i64> = row.get(8)?;
    Ok(crate::telemetry_identity::TrustedBinding {
        binding_id: row.get(0)?,
        scope: row.get(1)?,
        protocol: row.get(2)?,
        exporter_id: row.get(3)?,
        device_id: row.get(4)?,
        match_transport: row.get(5)?,
        match_reported_identity: row.get(6)?,
        match_exporter_claim: row.get(7)?,
        match_observation_domain: domain.map(|d| d as u32),
        match_ingress_credential: row.get(9)?,
        provenance: provenance_from_str(&row.get::<_, String>(10)?),
        status: status_from_str(&row.get::<_, String>(11)?),
        created_at_ns: row.get(12)?,
        updated_at_ns: row.get(13)?,
        created_by: row.get(14)?,
        updated_by: row.get(15)?,
    })
}

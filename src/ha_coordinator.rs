//! Core High Availability (HA) readiness module.
//!
//! Provides:
//! - Leader election using distributed consensus (etcd-based or simple health-based)
//! - Config change notification for replication
//! - Collector re-homing strategy when leader changes
//!
//! Full HA requires:
//! - External etcd cluster for leader election
//! - Config replication between cores
//! - Collector failover coordination

use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;

/// HA mode configuration
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HAMode {
    /// Single node - no HA
    Standalone,
    /// Multi-node with leader election
    Cluster { node_id: String },
}

/// etcd configuration for cluster mode
#[derive(Clone, Debug)]
pub struct EtcdConfig {
    pub endpoints: Vec<String>,
    pub election_ttl_secs: i64,
    pub config_prefix: String,
}

impl Default for EtcdConfig {
    fn default() -> Self {
        Self {
            endpoints: vec!["127.0.0.1:2379".to_string()],
            election_ttl_secs: 10,
            config_prefix: "/bonsai/config".to_string(),
        }
    }
}

/// Leader election state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaderState {
    /// This node is the leader
    Leader,
    /// This node is a follower
    Follower { leader_id: String },
    /// Election in progress
    Electing,
}

/// HA coordinator for leader election and failover coordination
pub struct HACoordinator {
    mode: HAMode,
    state: Arc<RwLock<LeaderState>>,
    etcd_config: Option<EtcdConfig>,
    shutdown_signal: Arc<RwLock<bool>>,
}

impl HACoordinator {
    pub fn new(mode: HAMode) -> Self {
        Self {
            mode,
            state: Arc::new(RwLock::new(LeaderState::Electing)),
            etcd_config: None,
            shutdown_signal: Arc::new(RwLock::new(false)),
        }
    }

    pub fn with_etcd_config(mut self, config: EtcdConfig) -> Self {
        self.etcd_config = Some(config);
        self
    }

    /// Get current leader state
    pub async fn state(&self) -> LeaderState {
        self.state.read().await.clone()
    }

    /// Check if this node is the leader
    pub async fn is_leader(&self) -> bool {
        matches!(*self.state.read().await, LeaderState::Leader)
    }

    /// Set leader state (called by election loop)
    pub async fn set_state(&self, new_state: LeaderState) {
        *self.state.write().await = new_state;
    }

    /// Shutdown the HA coordinator
    pub async fn shutdown(&self) {
        *self.shutdown_signal.write().await = true;
    }

    /// Get the shutdown signal for external use
    pub fn shutdown_signal(&self) -> Arc<RwLock<bool>> {
        self.shutdown_signal.clone()
    }

    /// Start leader election loop
    ///
    /// In standalone mode, immediately becomes leader.
    /// In cluster mode, uses etcd for distributed election.
    pub async fn start_election(&self) {
        match &self.mode {
            HAMode::Standalone => {
                self.set_state(LeaderState::Leader).await;
                tracing::info!("HA mode: standalone, assumed leader");
            }
            HAMode::Cluster { node_id } => {
                // G3 Session 2: Implement etcd-based leader election
                if let Some(ref etcd_config) = self.etcd_config {
                    match self.run_etcd_election(node_id.clone(), etcd_config).await {
                        Ok(_) => {
                            tracing::info!(%node_id, "HA mode: cluster, leader election completed");
                        }
                        Err(e) => {
                            tracing::error!(%node_id, error = %e, "HA mode: cluster, leader election failed, falling back to leader assumption");
                            self.set_state(LeaderState::Leader).await; // Fallback
                        }
                    }
                } else {
                    tracing::warn!(%node_id, "HA mode: cluster but no etcd config provided");
                    self.set_state(LeaderState::Leader).await; // Fallback
                }
            }
        }
    }

    /// Run etcd-based leader election
    async fn run_etcd_election(&self, node_id: String, config: &EtcdConfig) -> Result<()> {
        use etcd_client::{Client, ConnectOptions, LeaseClient, ElectionClient};
        use tokio::time::{interval, Duration};

        let endpoints: Vec<&str> = config.endpoints.iter().map(|s| s.as_str()).collect();
        let client = Client::connect(endpoints, Some(ConnectOptions::default()))
            .await
            .context("failed to connect to etcd")?;

        let lease_id = client.lease_grant(config.election_ttl_secs, None).await?.id();
        let election_key = format!("{}/leader", config.config_prefix);

        tracing::info!(
            %node_id,
            lease_id,
            %election_key,
            "starting etcd leader election"
        );

        // Campaign for leadership
        let campaign_result = client.election(election_key.clone(), lease_id).campaign(node_id.clone()).await;

        match campaign_result {
            Ok(_) => {
                self.set_state(LeaderState::Leader).await;
                tracing::info!(%node_id, "won leader election");
            }
            Err(e) => {
                tracing::error!(%node_id, error = %e, "failed to campaign for leadership");
                // Check if there's already a leader
                match client.election(election_key.clone()).leader().await {
                    Ok(Some(leader)) => {
                        self.set_state(LeaderState::Follower { leader_id: leader }).await;
                        tracing::info!(%node_id, %leader, "became follower");
                    }
                    Ok(None) => {
                        tracing::warn!(%node_id, "no leader found, retrying election");
                        return Err(anyhow::anyhow!("no leader found"));
                    }
                    Err(e) => {
                        tracing::error!(%node_id, error = %e, "failed to get leader");
                        return Err(e.into());
                    }
                }
                return Ok(());
            }
        }

        // Maintain lease (heartbeat)
        let mut ticker = interval(Duration::from_secs(config.election_ttl_secs as u64 / 2));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    // Keep lease alive
                    if let Err(e) = client.lease_keep_alive(lease_id, None).await {
                        tracing::error!(error = %e, "failed to keep lease alive, resigning leadership");
                        self.set_state(LeaderState::Electing).await;
                        return Err(e.into());
                    }
                }
                _ = self.watch_shutdown_signal() => {
                    tracing::info!(%node_id, "shutdown requested, resigning leadership");
                    if let Err(e) = client.lease_revoke(lease_id, None).await {
                        tracing::error!(error = %e, "failed to revoke lease");
                    }
                    self.set_state(LeaderState::Electing).await;
                    return Ok(());
                }
            }
        }
    }

    /// Watch for shutdown signal
    async fn watch_shutdown_signal(&self) {
        while !*self.shutdown_signal.read().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Test etcd connection
    async fn test_etcd_connection(&self, config: &EtcdConfig) -> Result<()> {
        use etcd_client::{Client, ConnectOptions};

        let endpoints: Vec<&str> = config.endpoints.iter().map(|s| s.as_str()).collect();
        let client = Client::connect(endpoints, Some(ConnectOptions::default()))
            .await
            .context("failed to connect to etcd")?;

        // Test by getting cluster status
        let status = client.status().await.context("failed to get etcd status")?;
        tracing::debug!(leader = ?status.leader, "etcd connection test successful");

        Ok(())
    }
}

/// Config change notification for replication
#[derive(Clone, Debug)]
pub struct ConfigChange {
    pub change_type: ConfigChangeType,
    pub table: String,
    pub key: String,
    pub value: Option<String>, // JSON
    pub timestamp_ns: i64,
    pub node_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigChangeType {
    Upsert,
    Delete,
}

/// Config replication coordinator
///
/// In a full HA setup, this would:
/// - Subscribe to config changes from SQLite
/// - Publish changes to a replication channel (etcd, Kafka, etc.)
/// - Apply incoming changes from other nodes
pub struct ConfigReplicator {
    node_id: String,
    etcd_config: Option<EtcdConfig>,
    sqlite_store: Option<std::sync::Arc<crate::sqlite_store::SqliteStore>>,
}

impl ConfigReplicator {
    pub fn new(node_id: String) -> Self {
        Self {
            node_id,
            etcd_config: None,
            sqlite_store: None,
        }
    }

    pub fn with_etcd_config(mut self, config: EtcdConfig) -> Self {
        self.etcd_config = Some(config);
        self
    }

    pub fn with_sqlite_store(mut self, store: std::sync::Arc<crate::sqlite_store::SqliteStore>) -> Self {
        self.sqlite_store = Some(store);
        self
    }

    /// Publish a config change for replication
    pub async fn publish_change(&self, change: ConfigChange) -> anyhow::Result<()> {
        if let Some(ref etcd_config) = self.etcd_config {
            use etcd_client::{Client, ConnectOptions};

            let endpoints: Vec<&str> = etcd_config.endpoints.iter().map(|s| s.as_str()).collect();
            let client = Client::connect(endpoints, Some(ConnectOptions::default()))
                .await
                .context("failed to connect to etcd for config publish")?;

            // Serialize config change to JSON
            let change_json = serde_json::to_string(&change)
                .context("failed to serialize config change")?;

            // Write to etcd with key: /bonsai/config/<table>/<key>
            let etcd_key = format!("{}/{}/{}", etcd_config.config_prefix, change.table, change.key);
            
            let mut kv_client = client.kv_client();
            let put_response = kv_client.put(etcd_key, change_json, None).await
                .context("failed to publish config change to etcd")?;

            tracing::info!(
                change_type = ?change.change_type,
                table = %change.table,
                key = %change.key,
                revision = put_response.revision(),
                "config change published to etcd"
            );
        } else {
            tracing::debug!(
                change_type = ?change.change_type,
                table = %change.table,
                key = %change.key,
                "config change published for replication (no etcd config)"
            );
        }
        Ok(())
    }

    /// Apply a config change from another node
    pub async fn apply_change(&self, change: ConfigChange) -> anyhow::Result<()> {
        // G3 Session 6: Apply config change from etcd to local SQLite store
        tracing::info!(
            change_type = ?change.change_type,
            table = %change.table,
            key = %change.key,
            from_node = %change.node_id,
            timestamp_ns = change.timestamp_ns,
            "applying replicated config change"
        );
        
        // Apply to local SQLite store if available
        if let Some(ref store) = self.sqlite_store {
            // Skip changes from self to avoid loops
            if change.node_id == self.node_id {
                tracing::debug!("skipping change from self");
                return Ok(());
            }
            
            match change.table.as_str() {
                "devices" => {
                    if change.change_type == ConfigChangeType::Upsert {
                        if let Some(ref value) = change.value {
                            if let Ok(device) = serde_json::from_str::<crate::config::TargetConfig>(value) {
                                store.upsert_device(&device, "replication", "apply_change")?;
                            }
                        }
                    } else {
                        store.delete_device(&change.key, "replication")?;
                    }
                }
                "enrichers" => {
                    if change.change_type == ConfigChangeType::Upsert {
                        if let Some(ref value) = change.value {
                            if let Ok(config) = serde_json::from_str::<crate::enrichment::EnricherConfig>(value) {
                                store.upsert_enricher(&config, "replication")?;
                            }
                        }
                    } else {
                        store.delete_enricher(&change.key, "replication")?;
                    }
                }
                "adapters" => {
                    if change.change_type == ConfigChangeType::Upsert {
                        if let Some(ref value) = change.value {
                            if let Ok(config) = serde_json::from_str::<crate::output::OutputAdapterConfig>(value) {
                                store.upsert_adapter(&config, "replication")?;
                            }
                        }
                    } else {
                        store.delete_adapter(&change.key, "replication")?;
                    }
                }
                _ => {
                    tracing::warn!(table = %change.table, "unknown config table");
                }
            }
        } else {
            tracing::warn!("no SQLite store configured, cannot apply config change");
        }
        
        Ok(())
    }

    /// Watch for config changes from etcd and apply them
    pub async fn watch_and_apply_changes(&self, shutdown_signal: Arc<RwLock<bool>>) -> anyhow::Result<()> {
        if let Some(ref etcd_config) = self.etcd_config {
            use etcd_client::{Client, ConnectOptions, Watcher};
            use tokio::time::{interval, Duration};

            let endpoints: Vec<&str> = etcd_config.endpoints.iter().map(|s| s.as_str()).collect();
            let client = Client::connect(endpoints, Some(ConnectOptions::default()))
                .await
                .context("failed to connect to etcd for config watch")?;

            let watch_prefix = format!("{}/", etcd_config.config_prefix);
            
            tracing::info!(
                %watch_prefix,
                node_id = %self.node_id,
                "starting config change watcher"
            );

            let mut watcher = client.watcher(watch_prefix, None).await
                .context("failed to create etcd watcher")?;

            let mut ticker = interval(Duration::from_secs(5));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // Periodic health check
                        tracing::debug!("config watcher healthy");
                    }
                    result = watcher.next() => {
                        match result {
                            Ok(Some(response)) => {
                                for event in response.events() {
                                    if let Some(kv) = event.kv() {
                                        let key = String::from_utf8_lossy(kv.key());
                                        let value = kv.value().map(|v| String::from_utf8_lossy(v).to_string());
                                        
                                        // Parse key: /bonsai/config/<table>/<key>
                                        let parts: Vec<&str> = key.trim_start_matches(&format!("{}/", etcd_config.config_prefix)).split('/').collect();
                                        if parts.len() >= 2 {
                                            let table = parts[0].to_string();
                                            let config_key = parts[1..].join("/");
                                            
                                            let change_type = if event.is_delete() {
                                                ConfigChangeType::Delete
                                            } else {
                                                ConfigChangeType::Upsert
                                            };
                                            
                                            if let Some(ref val) = value {
                                                if let Ok(deserialized) = serde_json::from_str::<ConfigChange>(val) {
                                                    // Apply the change
                                                    if let Err(e) = self.apply_change(deserialized).await {
                                                        tracing::error!(error = %e, "failed to apply replicated config change");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                tracing::warn!("etcd watcher returned None, reconnecting");
                                // Re-create watcher
                                watcher = client.watcher(watch_prefix, None).await
                                    .context("failed to recreate etcd watcher")?;
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "etcd watcher error, reconnecting");
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                watcher = client.watcher(watch_prefix, None).await
                                    .context("failed to recreate etcd watcher after error")?;
                            }
                        }
                    }
                    _ = self.watch_shutdown_signal(shutdown_signal.clone()) => {
                        tracing::info!("shutdown requested, stopping config watcher");
                        return Ok(());
                    }
                }
            }
        } else {
            tracing::warn!("no etcd config provided, config watcher not started");
            Ok(())
        }
    }

    /// Watch for shutdown signal
    async fn watch_shutdown_signal(&self, shutdown_signal: Arc<RwLock<bool>>) {
        while !*shutdown_signal.read().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

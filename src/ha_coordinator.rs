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

/// HA mode configuration
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HAMode {
    /// Single node - no HA
    Standalone,
    /// Multi-node with leader election
    Cluster { node_id: String },
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
}

impl HACoordinator {
    pub fn new(mode: HAMode) -> Self {
        Self {
            mode,
            state: Arc::new(RwLock::new(LeaderState::Electing)),
        }
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

    /// Start leader election loop
    ///
    /// In standalone mode, immediately becomes leader.
    /// In cluster mode, would use etcd for distributed election.
    pub async fn start_election(&self) {
        match &self.mode {
            HAMode::Standalone => {
                self.set_state(LeaderState::Leader).await;
                tracing::info!("HA mode: standalone, assumed leader");
            }
            HAMode::Cluster { node_id } => {
                // TODO: Implement etcd-based leader election
                // For now, use simple health-based election
                tracing::info!(%node_id, "HA mode: cluster, election not yet implemented (needs etcd)");
                self.set_state(LeaderState::Leader).await; // Temporary: assume leader
            }
        }
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
}

impl ConfigReplicator {
    pub fn new(node_id: String) -> Self {
        Self { node_id }
    }

    /// Publish a config change for replication
    pub async fn publish_change(&self, change: ConfigChange) {
        // TODO: Publish to replication channel (etcd, Kafka, etc.)
        tracing::debug!(
            change_type = ?change.change_type,
            table = %change.table,
            key = %change.key,
            "config change published for replication"
        );
    }

    /// Apply a config change from another node
    pub async fn apply_change(&self, change: ConfigChange) -> anyhow::Result<()> {
        // TODO: Apply to local SQLite store
        tracing::debug!(
            change_type = ?change.change_type,
            table = %change.table,
            key = %change.key,
            from_node = %change.node_id,
            "applying replicated config change"
        );
        Ok(())
    }
}

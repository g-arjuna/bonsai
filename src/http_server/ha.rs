use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::ha_coordinator::{HACoordinator, LeaderState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HAStatusResponse {
    pub mode: String, // "standalone" or "cluster"
    pub node_id: String,
    pub leader_state: String, // "leader", "follower", "electing"
    pub leader_id: Option<String>,
    pub is_leader: bool,
    pub etcd_connected: bool,
    pub etcd_endpoints: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HASettingsResponse {
    pub mode: String,
    pub node_id: String,
    pub etcd_endpoints: String,
    pub election_ttl_secs: u64,
    pub config_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HAPatchSettingsRequest {
    pub mode: Option<String>,
    pub node_id: Option<String>,
    pub etcd_endpoints: Option<String>,
    pub election_ttl_secs: Option<u64>,
    pub config_prefix: Option<String>,
}

pub async fn ha_status_handler(
    State(ha_coordinator): State<Option<Arc<HACoordinator>>>,
) -> Result<Json<HAStatusResponse>, StatusCode> {
    if let Some(ha) = ha_coordinator {
        let state = ha.state().await;
        let leader_state_str = match state {
            LeaderState::Leader => "leader".to_string(),
            LeaderState::Follower { ref leader_id } => {
                format!("follower ({})", leader_id)
            }
            LeaderState::Electing => "electing".to_string(),
        };
        
        let leader_id = match state {
            LeaderState::Leader => None,
            LeaderState::Follower { ref leader_id } => Some(leader_id.clone()),
            LeaderState::Electing => None,
        };
        
        let mode = match ha.mode {
            crate::ha_coordinator::HAMode::Standalone => "standalone".to_string(),
            crate::ha_coordinator::HAMode::Cluster { ref node_id } => {
                format!("cluster ({})", node_id)
            }
        };
        
        let is_leader = matches!(state, LeaderState::Leader);
        let etcd_connected = ha.etcd_config.is_some();
        let etcd_endpoints = ha.etcd_config.as_ref().map(|c| c.endpoints.clone());
        
        Ok(Json(HAStatusResponse {
            mode,
            node_id: if let crate::ha_coordinator::HAMode::Cluster { ref node_id } = ha.mode {
                node_id.clone()
            } else {
                "N/A".to_string()
            },
            leader_state: leader_state_str,
            leader_id,
            is_leader,
            etcd_connected,
            etcd_endpoints,
        }))
    } else {
        Ok(Json(HAStatusResponse {
            mode: "standalone".to_string(),
            node_id: "N/A".to_string(),
            leader_state: "standalone".to_string(),
            leader_id: None,
            is_leader: false,
            etcd_connected: false,
            etcd_endpoints: None,
        }))
    }
}

pub async fn ha_settings_handler(
    State(ha_coordinator): State<Option<Arc<HACoordinator>>>,
) -> Result<Json<HASettingsResponse>, StatusCode> {
    if let Some(ha) = ha_coordinator {
        let etcd_config = ha.etcd_config.as_ref();
        Ok(Json(HASettingsResponse {
            mode: if matches!(ha.mode, crate::ha_coordinator::HAMode::Cluster { .. }) {
                "cluster".to_string()
            } else {
                "standalone".to_string()
            },
            node_id: if let crate::ha_coordinator::HAMode::Cluster { ref node_id } = ha.mode {
                node_id.clone()
            } else {
                "N/A".to_string()
            },
            etcd_endpoints: etcd_config.map(|c| c.endpoints.clone()).unwrap_or_default(),
            election_ttl_secs: etcd_config.map(|c| c.election_ttl_secs).unwrap_or(10),
            config_prefix: etcd_config.map(|c| c.config_prefix.clone()).unwrap_or_else(|| "/bonsai/config".to_string()),
        }))
    } else {
        Ok(Json(HASettingsResponse {
            mode: "standalone".to_string(),
            node_id: "N/A".to_string(),
            etcd_endpoints: String::new(),
            election_ttl_secs: 10,
            config_prefix: "/bonsai/config".to_string(),
        }))
    }
}

pub async fn ha_patch_settings_handler(
    State(ha_coordinator): State<Option<Arc<HACoordinator>>>,
    Json(req): Json<HAPatchSettingsRequest>,
) -> Result<Json<HASettingsResponse>, StatusCode> {
    // Note: HA settings changes require process restart to take effect
    // This endpoint validates and returns the effective settings
    // Actual implementation would require bonsai.toml hot-reload or restart signal
    
    let current = if let Some(ha) = ha_coordinator {
        let etcd_config = ha.etcd_config.as_ref();
        HASettingsResponse {
            mode: if matches!(ha.mode, crate::ha_coordinator::HAMode::Cluster { .. }) {
                "cluster".to_string()
            } else {
                "standalone".to_string()
            },
            node_id: if let crate::ha_coordinator::HAMode::Cluster { ref node_id } = ha.mode {
                node_id.clone()
            } else {
                "N/A".to_string()
            },
            etcd_endpoints: etcd_config.map(|c| c.endpoints.clone()).unwrap_or_default(),
            election_ttl_secs: etcd_config.map(|c| c.election_ttl_secs).unwrap_or(10),
            config_prefix: etcd_config.map(|c| c.config_prefix.clone()).unwrap_or_else(|| "/bonsai/config".to_string()),
        }
    } else {
        HASettingsResponse {
            mode: "standalone".to_string(),
            node_id: "N/A".to_string(),
            etcd_endpoints: String::new(),
            election_ttl_secs: 10,
            config_prefix: "/bonsai/config".to_string(),
        }
    };

    // Apply requested changes (for display purposes - requires restart)
    let effective = HASettingsResponse {
        mode: req.mode.unwrap_or(current.mode),
        node_id: req.node_id.unwrap_or(current.node_id),
        etcd_endpoints: req.etcd_endpoints.unwrap_or(current.etcd_endpoints),
        election_ttl_secs: req.election_ttl_secs.unwrap_or(current.election_ttl_secs),
        config_prefix: req.config_prefix.unwrap_or(current.config_prefix),
    };

    Ok(Json(effective))
}

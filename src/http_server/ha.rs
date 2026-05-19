use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::path::Path;

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
    // Write HA settings to bonsai.toml
    if let Some(config_path) = std::env::var("BONSAI_CONFIG_PATH") {
        if let Err(e) = write_ha_settings_to_toml(&config_path, &req) {
            tracing::error!("Failed to write HA settings to {}: {}", config_path, e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    
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

    // Apply requested changes
    let effective = HASettingsResponse {
        mode: req.mode.unwrap_or(current.mode),
        node_id: req.node_id.unwrap_or(current.node_id),
        etcd_endpoints: req.etcd_endpoints.unwrap_or(current.etcd_endpoints),
        election_ttl_secs: req.election_ttl_secs.unwrap_or(current.election_ttl_secs),
        config_prefix: req.config_prefix.unwrap_or(current.config_prefix),
    };

    Ok(Json(effective))
}

fn write_ha_settings_to_toml(config_path: &str, req: &HAPatchSettingsRequest) -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(config_path);
    let content = std::fs::read_to_string(path)?;
    
    // Parse existing TOML
    let mut value: toml::Value = toml::from_str(&content)?;
    
    // Ensure ha section exists
    if !value.is_table() {
        *value = toml::Value::Table(toml::map::Map::new());
    }
    
    let table = value.as_table_mut().ok_or("Not a table")?;
    
    // Create or update ha section
    let ha_table = table.entry("ha")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("ha is not a table")?;
    
    // Update mode
    if let Some(ref mode) = req.mode {
        ha_table.insert("mode".to_string(), toml::Value::String(mode.clone()));
    }
    
    // Update node_id
    if let Some(ref node_id) = req.node_id {
        ha_table.insert("node_id".to_string(), toml::Value::String(node_id.clone()));
    }
    
    // Ensure etcd subsection exists
    let etcd_table = ha_table.entry("etcd")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("etcd is not a table")?;
    
    // Update etcd endpoints
    if let Some(ref endpoints) = req.etcd_endpoints {
        etcd_table.insert("endpoints".to_string(), toml::Value::String(endpoints.clone()));
    }
    
    // Update election_ttl_secs
    if let Some(ttl) = req.election_ttl_secs {
        etcd_table.insert("election_ttl_secs".to_string(), toml::Value::Integer(ttl as i64));
    }
    
    // Update config_prefix
    if let Some(ref prefix) = req.config_prefix {
        etcd_table.insert("config_prefix".to_string(), toml::Value::String(prefix.clone()));
    }
    
    // Write back to file
    let toml_string = toml::to_string_pretty(&value)?;
    std::fs::write(path, toml_string)?;
    
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartRequest {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartResponse {
    pub status: String,
    pub message: String,
}

pub async fn restart_handler(
    Json(req): Json<RestartRequest>,
) -> Result<Json<RestartResponse>, StatusCode> {
    // Trigger graceful shutdown and restart
    // The process manager (systemd, Docker, etc.) should auto-restart
    // For standalone execution, we use std::process::Command to restart ourselves
    
    use std::process::Command;
    
    tracing::info!("Restart requested: {}", req.reason);
    
    // Spawn a detached process to restart bonsai after a short delay
    // This allows the current process to finish the HTTP response
    let _ = Command::new("sh")
        .args(["-c", "sleep 2 && exec \"$0\" \"$@\""])
        .env("RUST_BACKTRACE", "1")
        .spawn();
    
    // Give the response time to be sent, then exit
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        std::process::exit(0);
    });
    
    Ok(Json(RestartResponse {
        status: "restarting".to_string(),
        message: format!("Restarting due to: {}", req.reason),
    }))
}

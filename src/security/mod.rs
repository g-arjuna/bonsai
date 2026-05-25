//! Security Module
//! Comprehensive security features including authentication, authorization, database security,
//! threat detection, and incident response

pub mod database;
pub mod mfa;
pub mod session;
pub mod threat_intel;
pub mod incident_response;
pub mod anomaly_detection;

pub use database::{DatabaseSecurityManager, DatabaseSecurityConfig, DatabaseSecurityEvent};
pub use mfa::{MfaManager, MfaConfig, MfaMethod, MfaChallenge};
pub use session::{SessionManager, SessionConfig, SessionInfo};
pub use threat_intel::{ThreatIntelManager, ThreatIntelConfig, ThreatIndicator};
pub use incident_response::{IncidentResponseManager, IncidentResponseConfig, IncidentWorkflow};
pub use anomaly_detection::{AnomalyDetectionManager, AnomalyDetectionConfig, SecurityEvent, AnomalyResult};

/// Security module configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecurityConfig {
    pub database: DatabaseSecurityConfig,
    pub mfa: MfaConfig,
    pub session: SessionConfig,
    pub threat_intel: ThreatIntelConfig,
    pub incident_response: IncidentResponseConfig,
    pub anomaly_detection: AnomalyDetectionConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            database: DatabaseSecurityConfig::default(),
            mfa: MfaConfig::default(),
            session: SessionConfig::default(),
            threat_intel: ThreatIntelConfig::default(),
            incident_response: IncidentResponseConfig::default(),
            anomaly_detection: AnomalyDetectionConfig::default(),
        }
    }
}

/// Initialize all security components
pub async fn initialize_security(config: SecurityConfig) -> anyhow::Result<()> {
    tracing::info!("Initializing security module with selective feature enablement");
    
    // Initialize database security
    if config.database.enabled {
        database::initialize_database_security(config.database)?;
        tracing::info!("Database security enabled");
    } else {
        tracing::info!("Database security disabled");
    }
    
    // Initialize MFA
    if config.mfa.enabled {
        mfa::initialize_mfa(config.mfa).await?;
        tracing::info!("MFA enabled");
    } else {
        tracing::info!("MFA disabled");
    }
    
    // Initialize session management
    if config.session.enabled {
        session::initialize_session_manager(config.session).await?;
        tracing::info!("Session management enabled");
    } else {
        tracing::info!("Session management disabled");
    }
    
    // Initialize threat intelligence
    if config.threat_intel.enabled {
        threat_intel::initialize_threat_intel(config.threat_intel).await?;
        tracing::info!("Threat intelligence enabled");
    } else {
        tracing::info!("Threat intelligence disabled");
    }
    
    // Initialize incident response
    if config.incident_response.enabled {
        incident_response::initialize_incident_response(config.incident_response).await?;
        tracing::info!("Incident response enabled");
    } else {
        tracing::info!("Incident response disabled");
    }
    
    // Initialize anomaly detection
    if config.anomaly_detection.enabled {
        anomaly_detection::initialize_anomaly_detection(config.anomaly_detection)?;
        tracing::info!("Anomaly detection enabled");
    } else {
        tracing::info!("Anomaly detection disabled");
    }
    
    tracing::info!("Security module initialization completed");
    Ok(())
}

/// Get overall security health status
pub async fn get_security_health() -> serde_json::Value {
    let mut health = serde_json::json!({
        "status": "healthy",
        "components": {},
        "timestamp": crate::graph::common::now_ns()
    });

    // Database security health
    if let Some(_db_manager) = database::get_database_security_manager() {
        health["components"]["database"] = serde_json::json!({
            "status": "healthy",
            "encryption_enabled": true,
            "audit_enabled": true,
            "access_control_enabled": true
        });
    } else {
        health["components"]["database"] = serde_json::json!({
            "status": "uninitialized"
        });
        health["status"] = "degraded".into();
    }

    // MFA health
    if let Some(mfa_manager) = mfa::get_mfa_manager() {
        health["components"]["mfa"] = serde_json::json!({
            "status": "healthy",
            "enabled_methods": mfa_manager.get_enabled_methods()
        });
    } else {
        health["components"]["mfa"] = serde_json::json!({
            "status": "uninitialized"
        });
        health["status"] = "degraded".into();
    }

    // Session management health
    if let Some(session_manager) = session::get_session_manager() {
        health["components"]["session"] = serde_json::json!({
            "status": "healthy",
            "active_sessions": session_manager.get_active_session_count()
        });
    } else {
        health["components"]["session"] = serde_json::json!({
            "status": "uninitialized"
        });
        health["status"] = "degraded".into();
    }

    // Threat intelligence health
    if let Some(threat_manager) = threat_intel::get_threat_intel_manager() {
        health["components"]["threat_intel"] = serde_json::json!({
            "status": "healthy",
            "indicators_count": threat_manager.get_indicators_count()
        });
    } else {
        health["components"]["threat_intel"] = serde_json::json!({
            "status": "uninitialized"
        });
        health["status"] = "degraded".into();
    }

    // Incident response health
    if let Some(incident_manager) = incident_response::get_incident_response_manager() {
        health["components"]["incident_response"] = serde_json::json!({
            "status": "healthy",
            "active_workflows": incident_manager.get_active_workflows_count()
        });
    } else {
        health["components"]["incident_response"] = serde_json::json!({
            "status": "uninitialized"
        });
        health["status"] = "degraded".into();
    }

    health
}

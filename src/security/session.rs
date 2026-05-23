//! Session Management Module
//! Provides JWT session creation, validation, and revocation

use anyhow::{Context, Result};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// Session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub enabled: bool,
    pub jwt_secret: String,
    pub token_expiry_hours: u64,
    pub refresh_token_expiry_hours: u64,
    pub max_sessions_per_user: usize,
    pub idle_timeout_hours: u64,
    pub revoked_token_cleanup_hours: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            jwt_secret: "default-secret-change-in-production".to_string(),
            token_expiry_hours: 24,
            refresh_token_expiry_hours: 168, // 7 days
            max_sessions_per_user: 5,
            idle_timeout_hours: 8,
            revoked_token_cleanup_hours: 24,
        }
    }
}

/// JWT claims with additional session information
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Subject: username
    pub sub: String,
    /// Bonsai role string
    pub role: String,
    /// Session ID for revocation tracking
    pub jti: String,
    /// Issued-at (Unix seconds)
    pub iat: i64,
    /// Expiry (Unix seconds)
    pub exp: i64,
    /// Not before (Unix seconds)
    pub nbf: i64,
    /// Issuer
    pub iss: String,
    /// Audience
    pub aud: String,
    /// User agent
    pub ua: Option<String>,
    /// Client IP
    pub ip: Option<String>,
    /// Session type (access|refresh)
    pub typ: String,
}

/// Session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub user_id: String,
    pub role: String,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub last_activity: Instant,
    pub user_agent: Option<String>,
    pub client_ip: Option<String>,
    pub is_active: bool,
}

/// Revoked token entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokedToken {
    pub jti: String,
    pub user_id: String,
    pub revoked_at: Instant,
    pub expires_at: Instant,
    pub reason: String,
}

/// Session manager
pub struct SessionManager {
    config: SessionConfig,
    active_sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    revoked_tokens: Arc<Mutex<HashMap<String, RevokedToken>>>,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl SessionManager {
    pub fn new(config: SessionConfig) -> Result<Self> {
        let encoding_key = EncodingKey::from_secret(config.jwt_secret.as_ref());
        let decoding_key = DecodingKey::from_secret(config.jwt_secret.as_ref());
        
        Ok(Self {
            config,
            active_sessions: Arc::new(Mutex::new(HashMap::new())),
            revoked_tokens: Arc::new(Mutex::new(HashMap::new())),
            encoding_key,
            decoding_key,
        })
    }

    /// Create new session (access token)
    pub fn create_session(
        &self,
        user_id: &str,
        role: &str,
        user_agent: Option<String>,
        client_ip: Option<String>,
    ) -> Result<(String, String)> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::hours(self.config.token_expiry_hours as i64);
        
        // Create access token
        let access_claims = SessionClaims {
            sub: user_id.to_string(),
            role: role.to_string(),
            jti: session_id.clone(),
            iat: now.timestamp(),
            exp: expires_at.timestamp(),
            nbf: now.timestamp(),
            iss: "bonsai".to_string(),
            aud: "bonsai-users".to_string(),
            ua: user_agent.clone(),
            ip: client_ip.clone(),
            typ: "access".to_string(),
        };
        
        let access_token = encode(&Header::default(), &access_claims, &self.encoding_key)
            .context("Failed to encode access token")?;
        
        // Create refresh token
        let refresh_expires_at = now + chrono::Duration::hours(self.config.refresh_token_expiry_hours as i64);
        let refresh_claims = SessionClaims {
            sub: user_id.to_string(),
            role: role.to_string(),
            jti: format!("refresh-{}", session_id),
            iat: now.timestamp(),
            exp: refresh_expires_at.timestamp(),
            nbf: now.timestamp(),
            iss: "bonsai".to_string(),
            aud: "bonsai-users".to_string(),
            ua: user_agent.clone(),
            ip: client_ip.clone(),
            typ: "refresh".to_string(),
        };
        
        let refresh_token = encode(&Header::default(), &refresh_claims, &self.encoding_key)
            .context("Failed to encode refresh token")?;
        
        // Store session info
        let session_info = SessionInfo {
            session_id: session_id.clone(),
            user_id: user_id.to_string(),
            role: role.to_string(),
            created_at: Instant::now(),
            expires_at: Instant::now() + Duration::from_secs(self.config.token_expiry_hours * 3600),
            last_activity: Instant::now(),
            user_agent,
            client_ip,
            is_active: true,
        };
        
        let mut sessions = self.active_sessions.lock().unwrap();
        
        // Check session limit per user
        let user_sessions: Vec<_> = sessions.values()
            .filter(|s| s.user_id == user_id && s.is_active)
            .collect();
        
        if user_sessions.len() >= self.config.max_sessions_per_user {
            // Remove oldest session
            if let Some(oldest_session) = user_sessions.iter().min_by_key(|s| s.created_at) {
                sessions.remove(&oldest_session.session_id);
                info!("Removed oldest session for user: {}", user_id);
            }
        }
        
        sessions.insert(session_id.clone(), session_info);
        
        // Log security event
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "session_created",
            "session",
            "success",
            Some(&format!("user: {}, session_id: {}", user_id, session_id)),
        )?;
        
        info!("Created new session for user: {}", user_id);
        Ok((access_token, refresh_token))
    }

    /// Validate and refresh session
    pub fn refresh_session(&self, refresh_token: &str) -> Result<String> {
        let validation = Validation::new(Algorithm::HS256);
        let token_data = decode::<SessionClaims>(refresh_token, &self.decoding_key, &validation)
            .context("Invalid refresh token")?;
        
        let claims = token_data.claims;
        
        // Check if it's a refresh token
        if claims.typ != "refresh" {
            return Err(anyhow::anyhow!("Not a refresh token"));
        }
        
        // Check if token is revoked
        if self.is_token_revoked(&claims.jti)? {
            return Err(anyhow::anyhow!("Refresh token has been revoked"));
        }
        
        // Create new access token
        let now = chrono::Utc::now();
        let expires_at = now + chrono::Duration::hours(self.config.token_expiry_hours as i64);
        
        let new_access_claims = SessionClaims {
            sub: claims.sub.clone(),
            role: claims.role.clone(),
            jti: uuid::Uuid::new_v4().to_string(),
            iat: now.timestamp(),
            exp: expires_at.timestamp(),
            nbf: now.timestamp(),
            iss: "bonsai".to_string(),
            aud: "bonsai-users".to_string(),
            ua: claims.ua.clone(),
            ip: claims.ip.clone(),
            typ: "access".to_string(),
        };
        
        let new_access_token = encode(&Header::default(), &new_access_claims, &self.encoding_key)
            .context("Failed to encode new access token")?;
        
        // Update session activity
        self.update_session_activity(&claims.sub)?;
        
        info!("Refreshed session for user: {}", claims.sub);
        Ok(new_access_token)
    }

    /// Validate session token
    pub fn validate_session(&self, token: &str) -> Result<SessionClaims> {
        let validation = Validation::new(Algorithm::HS256);
        let token_data = decode::<SessionClaims>(token, &self.decoding_key, &validation)
            .context("Invalid session token")?;
        
        let claims = token_data.claims;
        
        // Check if token is revoked
        if self.is_token_revoked(&claims.jti)? {
            return Err(anyhow::anyhow!("Token has been revoked"));
        }
        
        // Check if session is still active
        if let Some(session_info) = self.get_session_info(&claims.jti)? {
            if !session_info.is_active {
                return Err(anyhow::anyhow!("Session is inactive"));
            }
            
            // Update last activity
            self.update_session_activity(&claims.sub)?;
        }
        
        Ok(claims)
    }

    /// Revoke session
    pub fn revoke_session(&self, session_id: &str, user_id: &str, reason: &str) -> Result<()> {
        let mut sessions = self.active_sessions.lock().unwrap();
        if let Some(session) = sessions.get_mut(session_id) {
            session.is_active = false;
            
            // Add to revoked tokens
            let revoked_token = RevokedToken {
                jti: session_id.to_string(),
                user_id: user_id.to_string(),
                revoked_at: Instant::now(),
                expires_at: session.expires_at,
                reason: reason.to_string(),
            };
            
            let mut revoked = self.revoked_tokens.lock().unwrap();
            revoked.insert(session_id.to_string(), revoked_token);
            
            // Log security event
            append_security_event(
                std::path::Path::new("/tmp"),
                crate::graph::common::now_ns(),
                "session_revoked",
                "session",
                "success",
                Some(&format!("user: {}, session_id: {}, reason: {}", user_id, session_id, reason)),
            )?;
            
            info!("Revoked session: {} for user: {} (reason: {})", session_id, user_id, reason);
        }
        
        Ok(())
    }

    /// Revoke all sessions for user
    pub fn revoke_all_user_sessions(&self, user_id: &str, reason: &str) -> Result<usize> {
        let mut sessions = self.active_sessions.lock().unwrap();
        let mut revoked_count = 0;
        
        let user_sessions: Vec<String> = sessions.values()
            .filter(|s| s.user_id == user_id && s.is_active)
            .map(|s| s.session_id.clone())
            .collect();
        
        for session_id in user_sessions {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.is_active = false;
                
                // Add to revoked tokens
                let revoked_token = RevokedToken {
                    jti: session_id.clone(),
                    user_id: user_id.to_string(),
                    revoked_at: Instant::now(),
                    expires_at: session.expires_at,
                    reason: reason.to_string(),
                };
                
                let mut revoked = self.revoked_tokens.lock().unwrap();
                revoked.insert(session_id.clone(), revoked_token);
                revoked_count += 1;
            }
        }
        
        // Log security event
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "all_sessions_revoked",
            "session",
            "success",
            Some(&format!("user: {}, count: {}, reason: {}", user_id, revoked_count, reason)),
        )?;
        
        info!("Revoked {} sessions for user: {} (reason: {})", revoked_count, user_id, reason);
        Ok(revoked_count)
    }

    /// Check if token is revoked
    fn is_token_revoked(&self, jti: &str) -> Result<bool> {
        let revoked = self.revoked_tokens.lock().unwrap();
        Ok(revoked.contains_key(jti))
    }

    /// Get session info
    fn get_session_info(&self, session_id: &str) -> Result<Option<SessionInfo>> {
        let sessions = self.active_sessions.lock().unwrap();
        Ok(sessions.get(session_id).cloned())
    }

    /// Update session activity
    fn update_session_activity(&self, user_id: &str) -> Result<()> {
        let mut sessions = self.active_sessions.lock().unwrap();
        for session in sessions.values_mut() {
            if session.user_id == user_id && session.is_active {
                session.last_activity = Instant::now();
            }
        }
        Ok(())
    }

    /// Get active sessions for user
    pub fn get_user_sessions(&self, user_id: &str) -> Vec<SessionInfo> {
        let sessions = self.active_sessions.lock().unwrap();
        sessions.values()
            .filter(|s| s.user_id == user_id && s.is_active)
            .cloned()
            .collect()
    }

    /// Get all active sessions
    pub fn get_all_active_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.active_sessions.lock().unwrap();
        sessions.values()
            .filter(|s| s.is_active)
            .cloned()
            .collect()
    }

    /// Get active session count
    pub fn get_active_session_count(&self) -> usize {
        let sessions = self.active_sessions.lock().unwrap();
        sessions.values().filter(|s| s.is_active).count()
    }

    /// Cleanup expired sessions and revoked tokens
    pub fn cleanup_expired(&self) -> Result<()> {
        let now = Instant::now();
        
        // Cleanup expired sessions
        {
            let mut sessions = self.active_sessions.lock().unwrap();
            sessions.retain(|_, session| session.expires_at > now);
        }
        
        // Cleanup expired revoked tokens
        {
            let mut revoked = self.revoked_tokens.lock().unwrap();
            revoked.retain(|_, token| token.expires_at > now);
        }
        
        // Cleanup idle sessions
        {
            let idle_timeout = Duration::from_secs(self.config.idle_timeout_hours * 3600);
            let mut sessions = self.active_sessions.lock().unwrap();
            
            for session in sessions.values_mut() {
                if session.is_active && (now - session.last_activity) > idle_timeout {
                    session.is_active = false;
                    
                    // Add to revoked tokens
                    let revoked_token = RevokedToken {
                        jti: session.session_id.clone(),
                        user_id: session.user_id.clone(),
                        revoked_at: now,
                        expires_at: session.expires_at,
                        reason: "idle_timeout".to_string(),
                    };
                    
                    let mut revoked = self.revoked_tokens.lock().unwrap();
                    revoked.insert(session.session_id.clone(), revoked_token);
                }
            }
        }
        
        info!("Session cleanup completed");
        Ok(())
    }

    /// Get session statistics
    pub fn get_session_stats(&self) -> serde_json::Value {
        let sessions = self.active_sessions.lock().unwrap();
        let revoked = self.revoked_tokens.lock().unwrap();
        
        let active_count = sessions.values().filter(|s| s.is_active).count();
        let idle_count = sessions.values()
            .filter(|s| s.is_active && (Instant::now() - s.last_activity) > Duration::from_secs(self.config.idle_timeout_hours * 3600))
            .count();
        
        serde_json::json!({
            "active_sessions": active_count,
            "idle_sessions": idle_count,
            "revoked_tokens": revoked.len(),
            "max_sessions_per_user": self.config.max_sessions_per_user,
            "token_expiry_hours": self.config.token_expiry_hours,
            "idle_timeout_hours": self.config.idle_timeout_hours
        })
    }
}

/// Global session manager instance
static SESSION_MANAGER: std::sync::OnceLock<std::sync::Arc<SessionManager>> = std::sync::OnceLock::new();

/// Initialize global session manager
pub async fn initialize_session_manager(config: SessionConfig) -> Result<()> {
    let manager = Arc::new(SessionManager::new(config)?);
    SESSION_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("Session manager already initialized"))?;
    Ok(())
}

/// Get global session manager
pub fn get_session_manager() -> Option<Arc<SessionManager>> {
    SESSION_MANAGER.get().cloned()
}

/// Background task to cleanup expired sessions
pub async fn cleanup_task() {
    let mut interval = tokio::time::interval(Duration::from_secs(3600)); // Every hour
    loop {
        interval.tick().await;
        if let Some(manager) = get_session_manager() {
            if let Err(e) = manager.cleanup_expired() {
                error!("Session cleanup error: {}", e);
            }
        }
    }
}

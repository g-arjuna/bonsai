//! Multi-Factor Authentication Module
//! Supports TOTP, SMS, and email-based MFA

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// MFA configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaConfig {
    pub enabled: bool,
    pub totp_enabled: bool,
    pub sms_enabled: bool,
    pub email_enabled: bool,
    pub backup_codes_enabled: bool,
    pub issuer: String,
    pub totp_window: u64, // Time window in seconds
    pub sms_provider: Option<String>,
    pub email_provider: Option<String>,
}

impl Default for MfaConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            totp_enabled: false,
            sms_enabled: false,
            email_enabled: false,
            backup_codes_enabled: false,
            issuer: "Bonsai Network Monitor".to_string(),
            totp_window: 30,
            sms_provider: None,
            email_provider: None,
        }
    }
}

/// MFA methods
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MfaMethod {
    Totp,
    Sms,
    Email,
    BackupCode,
}

/// MFA challenge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaChallenge {
    pub user_id: String,
    pub challenge_id: String,
    pub method: MfaMethod,
    pub code: Option<String>,
    pub expires_at: Instant,
    pub attempts: u32,
    pub max_attempts: u32,
}

/// User MFA settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMfaSettings {
    pub user_id: String,
    pub totp_secret: Option<String>,
    pub phone_number: Option<String>,
    pub email_address: Option<String>,
    pub backup_codes: Vec<String>,
    pub enabled_methods: Vec<MfaMethod>,
    pub last_used: Option<MfaMethod>,
}

/// MFA manager
pub struct MfaManager {
    config: MfaConfig,
    user_settings: Arc<Mutex<HashMap<String, UserMfaSettings>>>,
    active_challenges: Arc<Mutex<HashMap<String, MfaChallenge>>>,
}

impl MfaManager {
    pub fn new(config: MfaConfig) -> Self {
        Self {
            config,
            user_settings: Arc::new(Mutex::new(HashMap::new())),
            active_challenges: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generate TOTP secret for user
    pub fn generate_totp_secret(&self, user_id: &str) -> Result<String> {
        use base32::Alphabet;
        use rand::{thread_rng, Rng};
        
        let mut rng = thread_rng();
        let mut bytes = [0u8; 20]; // 160 bits for TOTP
        rng.fill(&mut bytes);
        
        let secret = base32::encode(Alphabet::RFC4648 { padding: true }, &bytes);
        
        // Store user settings
        let mut settings = self.user_settings.lock().unwrap();
        let user_settings = settings.entry(user_id.to_string()).or_insert_with(|| UserMfaSettings {
            user_id: user_id.to_string(),
            totp_secret: None,
            phone_number: None,
            email_address: None,
            backup_codes: Vec::new(),
            enabled_methods: Vec::new(),
            last_used: None,
        });
        
        user_settings.totp_secret = Some(secret.clone());
        if !user_settings.enabled_methods.contains(&MfaMethod::Totp) {
            user_settings.enabled_methods.push(MfaMethod::Totp);
        }
        
        info!("Generated TOTP secret for user: {}", user_id);
        Ok(secret)
    }

    /// Generate QR code for TOTP setup
    pub fn generate_totp_qr_code(&self, user_id: &str, secret: &str) -> Result<String> {
        use url::Url;
        
        let settings = self.user_settings.lock().unwrap();
        let user_settings = settings.get(user_id)
            .ok_or_else(|| anyhow::anyhow!("User settings not found"))?;
        
        let totp_url = format!(
            "otpauth://totp/{}:{}?secret={}&issuer={}",
            urlencoding::encode(&self.config.issuer),
            urlencoding::encode(user_id),
            secret,
            urlencoding::encode(&self.config.issuer)
        );
        
        Ok(totp_url)
    }

    /// Generate backup codes for user
    pub fn generate_backup_codes(&self, user_id: &str) -> Result<Vec<String>> {
        use rand::{thread_rng, Rng};
        
        let mut rng = thread_rng();
        let mut codes = Vec::new();
        
        for _ in 0..10 {
            let code: String = (0..8)
                .map(|_| rng.gen_range(0..10).to_string())
                .collect();
            codes.push(format!("{}-{}-{}-{}", 
                &code[0..2], &code[2..4], &code[4..6], &code[6..8]));
        }
        
        // Store backup codes
        let mut settings = self.user_settings.lock().unwrap();
        if let Some(user_settings) = settings.get_mut(user_id) {
            user_settings.backup_codes = codes.clone();
            if !user_settings.enabled_methods.contains(&MfaMethod::BackupCode) {
                user_settings.enabled_methods.push(MfaMethod::BackupCode);
            }
        }
        
        info!("Generated backup codes for user: {}", user_id);
        Ok(codes)
    }

    /// Create MFA challenge
    pub async fn create_challenge(&self, user_id: &str, method: MfaMethod) -> Result<String> {
        let challenge_id = format!("mfa-{}-{}", user_id, uuid::Uuid::new_v4());
        let expires_at = Instant::now() + Duration::from_secs(300); // 5 minutes
        
        let code = match method {
            MfaMethod::Totp => None, // User provides TOTP code
            MfaMethod::Sms => {
                let code = self.generate_sms_code()?;
                self.send_sms_code(user_id, &code).await?;
                Some(code)
            },
            MfaMethod::Email => {
                let code = self.generate_email_code()?;
                self.send_email_code(user_id, &code).await?;
                Some(code)
            },
            MfaMethod::BackupCode => None, // User provides backup code
        };
        
        let challenge = MfaChallenge {
            user_id: user_id.to_string(),
            challenge_id: challenge_id.clone(),
            method,
            code,
            expires_at,
            attempts: 0,
            max_attempts: 3,
        };
        
        let mut challenges = self.active_challenges.lock().unwrap();
        challenges.insert(challenge_id.clone(), challenge);
        
        info!("Created MFA challenge for user: {} with method: {:?}", user_id, method);
        Ok(challenge_id)
    }

    /// Verify MFA challenge
    pub fn verify_challenge(&self, challenge_id: &str, provided_code: &str) -> Result<bool> {
        let mut challenges = self.active_challenges.lock().unwrap();
        let challenge = challenges.get_mut(challenge_id)
            .ok_or_else(|| anyhow::anyhow!("Challenge not found"))?;
        
        // Check if expired
        if challenge.expires_at < Instant::now() {
            challenges.remove(challenge_id);
            return Ok(false);
        }
        
        // Check attempts
        if challenge.attempts >= challenge.max_attempts {
            challenges.remove(challenge_id);
            return Ok(false);
        }
        
        challenge.attempts += 1;
        
        let is_valid = match challenge.method {
            MfaMethod::Totp => self.verify_totp(&challenge.user_id, provided_code)?,
            MfaMethod::Sms => {
                challenge.code.as_ref().map_or(false, |code| code == provided_code)
            },
            MfaMethod::Email => {
                challenge.code.as_ref().map_or(false, |code| code == provided_code)
            },
            MfaMethod::BackupCode => self.verify_backup_code(&challenge.user_id, provided_code)?,
        };
        
        if is_valid {
            challenges.remove(challenge_id);
            self.update_last_used(&challenge.user_id, &challenge.method);
            info!("MFA challenge verified for user: {}", challenge.user_id);
        } else {
            warn!("MFA challenge failed for user: {}, attempt: {}", 
                  challenge.user_id, challenge.attempts);
        }
        
        Ok(is_valid)
    }

    /// Verify TOTP code
    fn verify_totp(&self, user_id: &str, code: &str) -> Result<bool> {
        use totp_lite::{totp_validate, Sha1};
        
        let settings = self.user_settings.lock().unwrap();
        let user_settings = settings.get(user_id)
            .ok_or_else(|| anyhow::anyhow!("User settings not found"))?;
        
        if let Some(secret) = &user_settings.totp_secret {
            Ok(totp_validate::<Sha1>(secret, self.config.totp_window, code))
        } else {
            Ok(false)
        }
    }

    /// Verify backup code
    fn verify_backup_code(&self, user_id: &str, code: &str) -> Result<bool> {
        let mut settings = self.user_settings.lock().unwrap();
        if let Some(user_settings) = settings.get_mut(user_id) {
            if let Some(pos) = user_settings.backup_codes.iter().position(|c| c == code) {
                user_settings.backup_codes.remove(pos);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Generate SMS code
    fn generate_sms_code(&self) -> Result<String> {
        use rand::{thread_rng, Rng};
        let mut rng = thread_rng();
        Ok((0..6).map(|_| rng.gen_range(0..10).to_string()).collect())
    }

    /// Generate email code
    fn generate_email_code(&self) -> Result<String> {
        use rand::{thread_rng, Rng};
        let mut rng = thread_rng();
        Ok((0..6).map(|_| rng.gen_range(0..10).to_string()).collect())
    }

    /// Send SMS code (placeholder implementation)
    async fn send_sms_code(&self, user_id: &str, code: &str) -> Result<()> {
        let settings = self.user_settings.lock().unwrap();
        let user_settings = settings.get(user_id)
            .ok_or_else(|| anyhow::anyhow!("User settings not found"))?;
        
        if let Some(phone) = &user_settings.phone_number {
            // In production, integrate with SMS provider (Twilio, AWS SNS, etc.)
            info!("Sending SMS code {} to {}", code, phone);
            
            // Log security event
            append_security_event(
                std::path::Path::new("/tmp"),
                crate::graph::common::now_ns(),
                "mfa_sms_sent",
                "mfa",
                "success",
                Some(&format!("user: {}, phone: {}", user_id, phone)),
            )?;
        } else {
            return Err(anyhow::anyhow!("No phone number configured for user"));
        }
        
        Ok(())
    }

    /// Send email code (placeholder implementation)
    async fn send_email_code(&self, user_id: &str, code: &str) -> Result<()> {
        let settings = self.user_settings.lock().unwrap();
        let user_settings = settings.get(user_id)
            .ok_or_else(|| anyhow::anyhow!("User settings not found"))?;
        
        if let Some(email) = &user_settings.email_address {
            // In production, integrate with email provider (SendGrid, AWS SES, etc.)
            info!("Sending email code {} to {}", code, email);
            
            // Log security event
            append_security_event(
                std::path::Path::new("/tmp"),
                crate::graph::common::now_ns(),
                "mfa_email_sent",
                "mfa",
                "success",
                Some(&format!("user: {}, email: {}", user_id, email)),
            )?;
        } else {
            return Err(anyhow::anyhow!("No email address configured for user"));
        }
        
        Ok(())
    }

    /// Update user's last used MFA method
    fn update_last_used(&self, user_id: &str, method: &MfaMethod) {
        let mut settings = self.user_settings.lock().unwrap();
        if let Some(user_settings) = settings.get_mut(user_id) {
            user_settings.last_used = Some(method.clone());
        }
    }

    /// Get user MFA settings
    pub fn get_user_settings(&self, user_id: &str) -> Option<UserMfaSettings> {
        let settings = self.user_settings.lock().unwrap();
        settings.get(user_id).cloned()
    }

    /// Update user MFA settings
    pub fn update_user_settings(&self, settings: UserMfaSettings) -> Result<()> {
        let mut user_settings = self.user_settings.lock().unwrap();
        user_settings.insert(settings.user_id.clone(), settings);
        Ok(())
    }

    /// Get enabled MFA methods
    pub fn get_enabled_methods(&self) -> Vec<MfaMethod> {
        let mut methods = Vec::new();
        if self.config.totp_enabled {
            methods.push(MfaMethod::Totp);
        }
        if self.config.sms_enabled {
            methods.push(MfaMethod::Sms);
        }
        if self.config.email_enabled {
            methods.push(MfaMethod::Email);
        }
        if self.config.backup_codes_enabled {
            methods.push(MfaMethod::BackupCode);
        }
        methods
    }

    /// Check if user has MFA enabled
    pub fn is_mfa_enabled_for_user(&self, user_id: &str) -> bool {
        let settings = self.user_settings.lock().unwrap();
        settings.get(user_id)
            .map(|s| !s.enabled_methods.is_empty())
            .unwrap_or(false)
    }

    /// Cleanup expired challenges
    pub fn cleanup_expired_challenges(&self) {
        let mut challenges = self.active_challenges.lock().unwrap();
        let now = Instant::now();
        challenges.retain(|_, challenge| challenge.expires_at > now);
    }
}

/// Global MFA manager instance
static MFA_MANAGER: std::sync::OnceLock<std::sync::Arc<MfaManager>> = std::sync::OnceLock::new();

/// Initialize global MFA manager
pub async fn initialize_mfa(config: MfaConfig) -> Result<()> {
    let manager = Arc::new(MfaManager::new(config));
    MFA_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("MFA manager already initialized"))?;
    Ok(())
}

/// Get global MFA manager
pub fn get_mfa_manager() -> Option<Arc<MfaManager>> {
    MFA_MANAGER.get().cloned()
}

/// Background task to cleanup expired challenges
pub async fn cleanup_task() {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Some(manager) = get_mfa_manager() {
            manager.cleanup_expired_challenges();
        }
    }
}

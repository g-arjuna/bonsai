//! Threat Intelligence Module
//! Real-time threat intelligence integration and indicator management

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// Threat intelligence configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatIntelConfig {
    pub enabled: bool,
    pub update_interval_minutes: u64,
    pub sources: Vec<ThreatSource>,
    pub auto_block_malicious_ips: bool,
    pub retention_days: u32,
}

impl Default for ThreatIntelConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            update_interval_minutes: 60,
            sources: Vec::new(),
            auto_block_malicious_ips: false,
            retention_days: 30,
        }
    }
}

/// Threat intelligence source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatSource {
    pub name: String,
    pub url: String,
    pub api_key: Option<String>,
    pub format: ThreatFormat,
    pub enabled: bool,
}

/// Threat data format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThreatFormat {
    Json,
    Xml,
    Csv,
    Stix,
    Taxii,
}

/// Threat indicator types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IndicatorType {
    IpAddress,
    Domain,
    Url,
    Hash,
    Email,
    FilePattern,
    MalwareFamily,
}

/// Threat severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ThreatSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Threat indicator
#[derive(Debug, Clone, Serialize)]
pub struct ThreatIndicator {
    pub id: String,
    pub indicator_type: IndicatorType,
    pub value: String,
    pub severity: ThreatSeverity,
    pub confidence: f64,
    pub source: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing, skip_deserializing)]
    pub first_seen: Instant,
    #[serde(skip_serializing, skip_deserializing)]
    pub last_seen: Instant,
    #[serde(skip_serializing, skip_deserializing)]
    pub expires_at: Option<Instant>,
    pub is_active: bool,
}

/// Threat intelligence feed response
#[derive(Debug, Deserialize)]
struct ThreatFeedResponse {
    pub indicators: Vec<ThreatIndicatorData>,
    #[allow(dead_code)]
    pub timestamp: i64,
    #[allow(dead_code)]
    pub source: String,
}

#[derive(Debug, Deserialize)]
struct ThreatIndicatorData {
    pub value: String,
    pub indicator_type: String,
    pub severity: String,
    pub confidence: Option<f64>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub first_seen: Option<i64>,
    pub last_seen: Option<i64>,
    pub expires_at: Option<i64>,
}

/// Threat intelligence manager
pub struct ThreatIntelManager {
    config: ThreatIntelConfig,
    indicators: Arc<Mutex<HashMap<String, ThreatIndicator>>>,
    malicious_ips: Arc<Mutex<HashSet<String>>>,
    last_update: Arc<Mutex<Instant>>,
}

impl ThreatIntelManager {
    pub fn new(config: ThreatIntelConfig) -> Self {
        Self {
            config,
            indicators: Arc::new(Mutex::new(HashMap::new())),
            malicious_ips: Arc::new(Mutex::new(HashSet::new())),
            last_update: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Initialize threat intelligence
    pub async fn initialize(&self) -> Result<()> {
        info!("Initializing threat intelligence");
        
        // Load initial threat data
        self.update_threat_intel().await?;
        
        // Start background update task
        self.start_update_task();
        
        info!("Threat intelligence initialized successfully");
        Ok(())
    }

    /// Update threat intelligence from all sources
    pub async fn update_threat_intel(&self) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        info!("Updating threat intelligence from {} sources", self.config.sources.len());
        
        let mut total_indicators = 0;
        let mut errors = Vec::new();

        for source in &self.config.sources {
            if !source.enabled {
                continue;
            }

            match self.fetch_from_source(source).await {
                Ok(indicators) => {
                    total_indicators += indicators.len();
                    info!("Fetched {} indicators from {}", indicators.len(), source.name);
                    
                    // Store indicators
                    let mut indicator_map = self.indicators.lock().unwrap();
                    for indicator in indicators {
                        indicator_map.insert(indicator.id.clone(), indicator);
                    }
                },
                Err(e) => {
                    error!("Failed to fetch from {}: {}", source.name, e);
                    errors.push(format!("{}: {}", source.name, e));
                }
            }
        }

        // Update malicious IP cache
        self.update_malicious_ip_cache();

        // Update last update time
        let mut last_update = self.last_update.lock().unwrap();
        *last_update = Instant::now();

        // Log security event
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "threat_intel_updated",
            "threat_intel",
            if errors.is_empty() { "success" } else { "partial" },
            Some(&format!("indicators: {}, errors: {}", total_indicators, errors.len())),
        )?;

        if !errors.is_empty() {
            warn!("Threat intelligence update completed with errors: {:?}", errors);
        } else {
            info!("Threat intelligence update completed successfully: {} indicators", total_indicators);
        }

        Ok(())
    }

    /// Fetch threat data from a specific source
    async fn fetch_from_source(&self, source: &ThreatSource) -> Result<Vec<ThreatIndicator>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;

        let mut request = client.get(&source.url);
        
        if let Some(api_key) = &source.api_key {
            request = request.header("Authorization", format!("Bearer {}", api_key));
        }

        let response = request.send()
            .await
            .context("Failed to send request")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        let content = response.text().await
            .context("Failed to read response")?;

        self.parse_threat_data(&content, &source.format, &source.name)
    }

    /// Parse threat data based on format
    fn parse_threat_data(&self, content: &str, format: &ThreatFormat, source: &str) -> Result<Vec<ThreatIndicator>> {
        match format {
            ThreatFormat::Json => self.parse_json_threat_data(content, source),
            ThreatFormat::Csv => self.parse_csv_threat_data(content, source),
            ThreatFormat::Stix => self.parse_stix_threat_data(content, source),
            _ => {
                warn!("Unsupported threat format: {:?}", format);
                Ok(Vec::new())
            }
        }
    }

    /// Parse JSON threat data
    fn parse_json_threat_data(&self, content: &str, source: &str) -> Result<Vec<ThreatIndicator>> {
        let response: ThreatFeedResponse = serde_json::from_str(content)
            .context("Failed to parse JSON threat data")?;

        let mut indicators = Vec::new();
        let now = Instant::now();

        for data in response.indicators {
            let indicator = self.convert_to_indicator(data, source, now)?;
            indicators.push(indicator);
        }

        Ok(indicators)
    }

    /// Parse CSV threat data
    fn parse_csv_threat_data(&self, content: &str, source: &str) -> Result<Vec<ThreatIndicator>> {
            let mut indicators = Vec::new();
        let now = Instant::now();

        for line in content.lines().skip(1) { // Skip header
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 2 {
                continue;
            }

            let indicator_type = match parts[0].to_lowercase().as_str() {
                "ip" | "ip_address" => IndicatorType::IpAddress,
                "domain" => IndicatorType::Domain,
                "url" => IndicatorType::Url,
                "hash" => IndicatorType::Hash,
                _ => continue,
            };

            let severity = match parts.get(2).unwrap_or(&"medium").to_lowercase().as_str() {
                "low" => ThreatSeverity::Low,
                "medium" => ThreatSeverity::Medium,
                "high" => ThreatSeverity::High,
                "critical" => ThreatSeverity::Critical,
                _ => ThreatSeverity::Medium,
            };

            let indicator = ThreatIndicator {
                id: format!("{}-{}", source, uuid::Uuid::new_v4()),
                indicator_type,
                value: parts[1].to_string(),
                severity,
                confidence: 0.8,
                source: source.to_string(),
                description: None,
                tags: Vec::new(),
                first_seen: now,
                last_seen: now,
                expires_at: None,
                is_active: true,
            };

            indicators.push(indicator);
        }

        Ok(indicators)
    }

    /// Parse STIX threat data
    fn parse_stix_threat_data(&self, _content: &str, _source: &str) -> Result<Vec<ThreatIndicator>> {
        // Simplified STIX parsing - in production, use proper STIX library
        let indicators = Vec::new();

        // This is a placeholder for STIX parsing
        // In production, you'd use a proper STIX parser
        warn!("STIX parsing not fully implemented - placeholder");

        Ok(indicators)
    }

    /// Convert threat data to indicator
    fn convert_to_indicator(&self, data: ThreatIndicatorData, source: &str, now: Instant) -> Result<ThreatIndicator> {
        let indicator_type = match data.indicator_type.to_lowercase().as_str() {
            "ip" | "ip_address" => IndicatorType::IpAddress,
            "domain" => IndicatorType::Domain,
            "url" => IndicatorType::Url,
            "hash" => IndicatorType::Hash,
            "email" => IndicatorType::Email,
            _ => IndicatorType::IpAddress, // Default
        };

        let severity = match data.severity.to_lowercase().as_str() {
            "low" => ThreatSeverity::Low,
            "medium" => ThreatSeverity::Medium,
            "high" => ThreatSeverity::High,
            "critical" => ThreatSeverity::Critical,
            _ => ThreatSeverity::Medium,
        };

        let first_seen = data.first_seen
            .map(|ts| Instant::now() - Duration::from_secs(now.elapsed().as_secs() - ts as u64))
            .unwrap_or(now);

        let last_seen = data.last_seen
            .map(|ts| Instant::now() - Duration::from_secs(now.elapsed().as_secs() - ts as u64))
            .unwrap_or(now);

        let expires_at = data.expires_at
            .map(|ts| Instant::now() + Duration::from_secs(ts as u64));

        Ok(ThreatIndicator {
            id: format!("{}-{}", source, uuid::Uuid::new_v4()),
            indicator_type,
            value: data.value,
            severity,
            confidence: data.confidence.unwrap_or(0.8),
            source: source.to_string(),
            description: data.description,
            tags: data.tags.unwrap_or_default(),
            first_seen,
            last_seen,
            expires_at,
            is_active: true,
        })
    }

    /// Update malicious IP cache for fast lookups
    fn update_malicious_ip_cache(&self) {
        let indicators = self.indicators.lock().unwrap();
        let mut malicious_ips = self.malicious_ips.lock().unwrap();
        
        malicious_ips.clear();
        
        for indicator in indicators.values() {
            if indicator.indicator_type == IndicatorType::IpAddress 
                && indicator.is_active 
                && (indicator.severity == ThreatSeverity::High || indicator.severity == ThreatSeverity::Critical) {
                malicious_ips.insert(indicator.value.clone());
            }
        }
    }

    /// Check if IP is malicious
    pub fn is_malicious_ip(&self, ip: &str) -> bool {
        let malicious_ips = self.malicious_ips.lock().unwrap();
        malicious_ips.contains(ip)
    }

    /// Check if any indicator matches
    pub fn check_indicator(&self, value: &str, indicator_type: IndicatorType) -> Option<ThreatIndicator> {
        let indicators = self.indicators.lock().unwrap();
        
        indicators.values()
            .find(|i| i.indicator_type == indicator_type && i.value == value && i.is_active)
            .cloned()
    }

    /// Get indicators by type
    pub fn get_indicators_by_type(&self, indicator_type: IndicatorType) -> Vec<ThreatIndicator> {
        let indicators = self.indicators.lock().unwrap();
        indicators.values()
            .filter(|i| i.indicator_type == indicator_type && i.is_active)
            .cloned()
            .collect()
    }

    /// Get indicators by severity
    pub fn get_indicators_by_severity(&self, severity: ThreatSeverity) -> Vec<ThreatIndicator> {
        let indicators = self.indicators.lock().unwrap();
        indicators.values()
            .filter(|i| i.severity == severity && i.is_active)
            .cloned()
            .collect()
    }

    /// Get indicators count
    pub fn get_indicators_count(&self) -> usize {
        let indicators = self.indicators.lock().unwrap();
        indicators.values().filter(|i| i.is_active).count()
    }

    /// Get threat intelligence statistics
    pub fn get_threat_stats(&self) -> serde_json::Value {
        let indicators = self.indicators.lock().unwrap();
        let last_update = self.last_update.lock().unwrap();
        
        let mut type_counts = HashMap::new();
        let mut severity_counts = HashMap::new();
        
        for indicator in indicators.values().filter(|i| i.is_active) {
            *type_counts.entry(format!("{:?}", indicator.indicator_type)).or_insert(0) += 1;
            *severity_counts.entry(format!("{:?}", indicator.severity)).or_insert(0) += 1;
        }
        
        serde_json::json!({
            "total_indicators": indicators.values().filter(|i| i.is_active).count(),
            "malicious_ips": self.malicious_ips.lock().unwrap().len(),
            "last_update": last_update.elapsed().as_secs(),
            "update_interval_minutes": self.config.update_interval_minutes,
            "sources": self.config.sources.len(),
            "enabled_sources": self.config.sources.iter().filter(|s| s.enabled).count(),
            "indicators_by_type": type_counts,
            "indicators_by_severity": severity_counts
        })
    }

    /// Start background update task
    fn start_update_task(&self) {
        let config = self.config.clone();
        let manager = self.clone(); // This would need to be implemented with proper cloning
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(config.update_interval_minutes * 60));
            
            loop {
                interval.tick().await;
                if let Err(e) = manager.update_threat_intel().await {
                    error!("Background threat intel update failed: {}", e);
                }
            }
        });
    }

    /// Cleanup expired indicators
    pub fn cleanup_expired_indicators(&self) -> Result<()> {
        let now = Instant::now();
        let mut indicators = self.indicators.lock().unwrap();
        
        let mut expired_count = 0;
        for indicator in indicators.values_mut() {
            if let Some(expires_at) = indicator.expires_at {
                if expires_at <= now {
                    indicator.is_active = false;
                    expired_count += 1;
                }
            }
        }
        
        // Update malicious IP cache
        self.update_malicious_ip_cache();
        
        info!("Cleaned up {} expired threat indicators", expired_count);
        Ok(())
    }
}

// Clone implementation for ThreatIntelManager (simplified)
impl Clone for ThreatIntelManager {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            indicators: Arc::clone(&self.indicators),
            malicious_ips: Arc::clone(&self.malicious_ips),
            last_update: Arc::clone(&self.last_update),
        }
    }
}

/// Global threat intelligence manager instance
static THREAT_INTEL_MANAGER: std::sync::OnceLock<std::sync::Arc<ThreatIntelManager>> = std::sync::OnceLock::new();

/// Initialize global threat intelligence manager
pub async fn initialize_threat_intel(config: ThreatIntelConfig) -> Result<()> {
    let manager = Arc::new(ThreatIntelManager::new(config));
    manager.initialize().await?;
    THREAT_INTEL_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("Threat intel manager already initialized"))?;
    Ok(())
}

/// Get global threat intelligence manager
pub fn get_threat_intel_manager() -> Option<Arc<ThreatIntelManager>> {
    THREAT_INTEL_MANAGER.get().cloned()
}

/// Background task to cleanup expired indicators
pub async fn cleanup_task() {
    let mut interval = tokio::time::interval(Duration::from_secs(3600)); // Every hour
    loop {
        interval.tick().await;
        if let Some(manager) = get_threat_intel_manager() {
            if let Err(e) = manager.cleanup_expired_indicators() {
                error!("Threat intel cleanup error: {}", e);
            }
        }
    }
}

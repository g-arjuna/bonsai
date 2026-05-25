//! Security Event Anomaly Detection Module
//! Detects anomalous patterns in security events using statistical analysis and ML

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// Anomaly detection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetectionConfig {
    pub enabled: bool,
    pub analysis_window_minutes: u64,
    pub alert_threshold: f64,
    pub min_samples_for_analysis: usize,
    pub enable_ml_detection: bool,
    pub retention_hours: u64,
}

impl Default for AnomalyDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            analysis_window_minutes: 60,
            alert_threshold: 2.0,
            min_samples_for_analysis: 30,
            enable_ml_detection: false,
            retention_hours: 24,
        }
    }
}

/// Security event types for anomaly detection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Hash, Eq)]
pub enum SecurityEventType {
    AuthenticationFailure,
    AuthenticationSuccess,
    PrivilegeEscalation,
    DataAccess,
    ConfigurationChange,
    NetworkConnection,
    FileAccess,
    ProcessExecution,
    SecurityViolation,
    ThreatDetection,
}

/// Security event data
#[derive(Debug, Clone, Serialize)]
pub struct SecurityEvent {
    pub id: String,
    pub event_type: SecurityEventType,
    pub user_id: Option<String>,
    pub source_ip: Option<String>,
    pub target_device: Option<String>,
    #[serde(skip_serializing, skip_deserializing)]
    pub timestamp: Instant,
    pub severity: f64,
    pub metadata: HashMap<String, String>,
}

/// Anomaly detection result
#[derive(Debug, Clone, Serialize)]
pub struct AnomalyResult {
    pub id: String,
    pub event_type: SecurityEventType,
    pub anomaly_score: f64,
    pub threshold: f64,
    pub is_anomaly: bool,
    pub description: String,
    #[serde(skip_serializing, skip_deserializing)]
    pub detected_at: Instant,
    pub contributing_factors: Vec<String>,
    pub affected_entities: Vec<String>,
}

/// Statistical baseline for event patterns
#[derive(Debug, Clone)]
struct EventBaseline {
    #[allow(dead_code)]
    pub event_type: SecurityEventType,
    pub mean_rate: f64,
    pub std_dev: f64,
    pub sample_count: usize,
    pub last_updated: Instant,
    pub hourly_pattern: [f64; 24], // Hourly distribution
}

/// Anomaly detection manager
pub struct AnomalyDetectionManager {
    config: AnomalyDetectionConfig,
    event_history: Arc<Mutex<HashMap<SecurityEventType, VecDeque<SecurityEvent>>>>,
    baselines: Arc<Mutex<HashMap<SecurityEventType, EventBaseline>>>,
    anomaly_results: Arc<Mutex<Vec<AnomalyResult>>>,
    detection_models: Arc<Mutex<HashMap<SecurityEventType, SimpleDetectionModel>>>,
}

/// Simple statistical detection model
#[derive(Debug, Clone)]
struct SimpleDetectionModel {
    #[allow(dead_code)]
    pub event_type: SecurityEventType,
    pub threshold_multiplier: f64,
    pub min_samples: usize,
    #[allow(dead_code)]
    pub window_size: usize,
}

impl AnomalyDetectionManager {
    pub fn new(config: AnomalyDetectionConfig) -> Self {
        Self {
            config,
            event_history: Arc::new(Mutex::new(HashMap::new())),
            baselines: Arc::new(Mutex::new(HashMap::new())),
            anomaly_results: Arc::new(Mutex::new(Vec::new())),
            detection_models: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Initialize anomaly detection
    pub fn initialize(&self) -> Result<()> {
        info!("Initializing security anomaly detection");
        
        // Initialize detection models
        self.initialize_detection_models();
        
        info!("Anomaly detection initialized successfully");
        Ok(())
    }

    /// Initialize detection models for different event types
    fn initialize_detection_models(&self) {
        let mut models = self.detection_models.lock().unwrap();
        
        let event_types = vec![
            SecurityEventType::AuthenticationFailure,
            SecurityEventType::PrivilegeEscalation,
            SecurityEventType::DataAccess,
            SecurityEventType::ConfigurationChange,
            SecurityEventType::NetworkConnection,
            SecurityEventType::SecurityViolation,
            SecurityEventType::ThreatDetection,
        ];

        for event_type in event_types {
            models.insert(event_type.clone(), SimpleDetectionModel {
                event_type,
                threshold_multiplier: 3.0, // 3-sigma rule
                min_samples: 30,
                window_size: 100,
            });
        }
    }

    /// Process security event for anomaly detection
    pub fn process_event(&self, event: SecurityEvent) -> Result<Vec<AnomalyResult>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        // Store event in history
        self.store_event(&event);

        // Update baselines
        self.update_baselines(&event.event_type);

        // Detect anomalies
        let mut anomalies = Vec::new();
        
        // Rate-based anomaly detection
        if let Some(anomaly) = self.detect_rate_anomaly(&event) {
            anomalies.push(anomaly);
        }

        // Pattern-based anomaly detection
        if let Some(anomaly) = self.detect_pattern_anomaly(&event) {
            anomalies.push(anomaly);
        }

        // User behavior anomaly detection
        if let Some(user_id) = &event.user_id {
            if let Some(anomaly) = self.detect_user_behavior_anomaly(&event, user_id) {
                anomalies.push(anomaly);
            }
        }

        // Store anomaly results
        if !anomalies.is_empty() {
            let mut results = self.anomaly_results.lock().unwrap();
            results.extend(anomalies.clone());
            
            // Log security events for anomalies
            for anomaly in &anomalies {
                self.log_anomaly_detected(anomaly)?;
            }
        }

        Ok(anomalies)
    }

    /// Store event in history
    fn store_event(&self, event: &SecurityEvent) {
        let mut history = self.event_history.lock().unwrap();
        let events = history.entry(event.event_type.clone()).or_insert_with(VecDeque::new);
        
        events.push_back(event.clone());
        
        // Keep only recent events within the analysis window
        let cutoff = Instant::now() - Duration::from_secs(self.config.analysis_window_minutes * 60);
        while let Some(front_event) = events.front() {
            if front_event.timestamp < cutoff {
                events.pop_front();
            } else {
                break;
            }
        }
    }

    /// Update statistical baselines
    fn update_baselines(&self, event_type: &SecurityEventType) {
        let history = self.event_history.lock().unwrap();
        let events = history.get(event_type);
        
        if let Some(events) = events {
            if events.len() < self.config.min_samples_for_analysis {
                return;
            }

            let now = Instant::now();
            let window_start = now - Duration::from_secs(self.config.analysis_window_minutes * 60);
            
            // Calculate event rate (events per minute)
            let events_in_window: Vec<_> = events.iter()
                .filter(|e| e.timestamp >= window_start)
                .collect();
            
            let rate = events_in_window.len() as f64 / self.config.analysis_window_minutes as f64;
            
            // Calculate hourly pattern
            let mut hourly_counts = [0.0; 24];
            for event in &events_in_window {
                let hour = event.timestamp.elapsed().as_secs() / 3600 % 24;
                hourly_counts[hour as usize] += 1.0;
            }
            
            // Normalize to percentages
            let total = hourly_counts.iter().sum::<f64>();
            if total > 0.0 {
                for count in &mut hourly_counts {
                    *count /= total / 100.0;
                }
            }

            let mut baselines = self.baselines.lock().unwrap();
            let baseline = baselines.entry(event_type.clone()).or_insert_with(|| EventBaseline {
                event_type: event_type.clone(),
                mean_rate: rate,
                std_dev: 0.0,
                sample_count: events_in_window.len(),
                last_updated: now,
                hourly_pattern: hourly_counts,
            });

            // Update baseline with exponential smoothing
            let alpha = 0.1; // Smoothing factor
            baseline.mean_rate = alpha * rate + (1.0 - alpha) * baseline.mean_rate;
            baseline.sample_count = events_in_window.len();
            baseline.last_updated = now;
            baseline.hourly_pattern = hourly_counts;
        }
    }

    /// Detect rate-based anomalies
    fn detect_rate_anomaly(&self, event: &SecurityEvent) -> Option<AnomalyResult> {
        let baselines = self.baselines.lock().unwrap();
        let models = self.detection_models.lock().unwrap();
        
        if let Some(baseline) = baselines.get(&event.event_type) {
            if let Some(model) = models.get(&event.event_type) {
                if baseline.sample_count < model.min_samples {
                    return None;
                }

                // Calculate current rate
                let history = self.event_history.lock().unwrap();
                if let Some(events) = history.get(&event.event_type) {
                    let now = Instant::now();
                    let recent_window = Duration::from_secs(300); // 5 minutes
                    let recent_events: Vec<_> = events.iter()
                        .filter(|e| now.duration_since(e.timestamp) <= recent_window)
                        .collect();
                    
                    let current_rate = recent_events.len() as f64 / recent_window.as_secs() as f64 * 60.0; // per minute
                    
                    // Calculate z-score
                    let z_score = if baseline.std_dev > 0.0 {
                        (current_rate - baseline.mean_rate) / baseline.std_dev
                    } else {
                        0.0
                    };

                    let threshold = model.threshold_multiplier;
                    if z_score.abs() > threshold {
                        return Some(AnomalyResult {
                            id: format!("anomaly-{}-{:?}", uuid::Uuid::new_v4(), event.event_type),
                            event_type: event.event_type.clone(),
                            anomaly_score: z_score.abs(),
                            threshold,
                            is_anomaly: true,
                            description: format!("Unusual {} rate detected: {:.2} events/min (baseline: {:.2} ± {:.2})", 
                                format!("{:?}", event.event_type).to_lowercase(), 
                                current_rate, baseline.mean_rate, baseline.std_dev),
                            detected_at: Instant::now(),
                            contributing_factors: vec![
                                format!("z-score: {:.2}", z_score),
                                format!("current_rate: {:.2}", current_rate),
                                format!("baseline_rate: {:.2}", baseline.mean_rate),
                            ],
                            affected_entities: vec![
                                event.source_ip.clone().unwrap_or_default(),
                                event.user_id.clone().unwrap_or_default(),
                            ].into_iter().filter(|s| !s.is_empty()).collect(),
                        });
                    }
                }
            }
        }

        None
    }

    /// Detect pattern-based anomalies
    fn detect_pattern_anomaly(&self, event: &SecurityEvent) -> Option<AnomalyResult> {
        let baselines = self.baselines.lock().unwrap();
        
        if let Some(baseline) = baselines.get(&event.event_type) {
            let current_hour = (event.timestamp.elapsed().as_secs() / 3600) as usize % 24;
            let expected_percentage = baseline.hourly_pattern[current_hour];
            
            // This is a simplified pattern detection
            // In production, you'd use more sophisticated pattern analysis
            if expected_percentage < 5.0 && matches!(event.event_type, SecurityEventType::AuthenticationFailure) {
                return Some(AnomalyResult {
                    id: format!("pattern-anomaly-{}-{:?}", uuid::Uuid::new_v4(), event.event_type),
                    event_type: event.event_type.clone(),
                    anomaly_score: 2.5,
                    threshold: 2.0,
                    is_anomaly: true,
                    description: format!("Unusual timing for {:?}: expected low activity at this hour", event.event_type),
                    detected_at: Instant::now(),
                    contributing_factors: vec![
                        format!("hour: {}", current_hour),
                        format!("expected_percentage: {:.1}%", expected_percentage),
                    ],
                    affected_entities: vec![
                        event.source_ip.clone().unwrap_or_default(),
                        event.user_id.clone().unwrap_or_default(),
                    ].into_iter().filter(|s| !s.is_empty()).collect(),
                });
            }
        }

        None
    }

    /// Detect user behavior anomalies
    fn detect_user_behavior_anomaly(&self, event: &SecurityEvent, user_id: &str) -> Option<AnomalyResult> {
        let history = self.event_history.lock().unwrap();
        
        // Analyze user's recent activity patterns
        let mut user_events = Vec::new();
        for events in history.values() {
            for e in events {
                if let Some(e_user_id) = &e.user_id {
                    if e_user_id == user_id {
                        user_events.push(e);
                    }
                }
            }
        }

        if user_events.len() < 10 {
            return None; // Not enough data
        }

        // Check for unusual patterns
        let now = Instant::now();
        let recent_hour = Duration::from_secs(3600);
        let recent_user_events: Vec<_> = user_events.iter()
            .filter(|e| now.duration_since(e.timestamp) <= recent_hour)
            .collect();

        // Detect rapid authentication failures
        let auth_failures = recent_user_events.iter()
            .filter(|e| matches!(e.event_type, SecurityEventType::AuthenticationFailure))
            .count();

        if auth_failures > 5 {
            return Some(AnomalyResult {
                id: format!("user-anomaly-{}-{}", uuid::Uuid::new_v4(), user_id),
                event_type: SecurityEventType::AuthenticationFailure,
                anomaly_score: auth_failures as f64,
                threshold: 5.0,
                is_anomaly: true,
                description: format!("Multiple authentication failures for user {} in last hour", user_id),
                detected_at: Instant::now(),
                contributing_factors: vec![
                    format!("auth_failures: {}", auth_failures),
                    "possible_brute_force".to_string(),
                ],
                affected_entities: vec![
                    user_id.to_string(),
                    event.source_ip.clone().unwrap_or_default(),
                ].into_iter().filter(|s| !s.is_empty()).collect(),
            });
        }

        None
    }

    /// Log anomaly detection event
    fn log_anomaly_detected(&self, anomaly: &AnomalyResult) -> Result<()> {
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "anomaly_detected",
            "anomaly_detection",
            "success",
            Some(&format!(
                "event_type: {:?}, score: {:.2}, description: {}",
                anomaly.event_type, anomaly.anomaly_score, anomaly.description
            )),
        )?;

        warn!("Security anomaly detected: {:?}", anomaly);
        Ok(())
    }

    /// Get recent anomalies
    pub fn get_recent_anomalies(&self, limit: usize) -> Vec<AnomalyResult> {
        let results = self.anomaly_results.lock().unwrap();
        results.iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get anomalies by event type
    pub fn get_anomalies_by_type(&self, event_type: &SecurityEventType) -> Vec<AnomalyResult> {
        let results = self.anomaly_results.lock().unwrap();
        results.iter()
            .filter(|a| &a.event_type == event_type)
            .cloned()
            .collect()
    }

    /// Get anomaly statistics
    pub fn get_anomaly_stats(&self) -> serde_json::Value {
        let results = self.anomaly_results.lock().unwrap();
        let baselines = self.baselines.lock().unwrap();
        
        let mut type_counts = HashMap::new();
        let mut severity_distribution = HashMap::new();
        
        for anomaly in results.iter() {
            *type_counts.entry(format!("{:?}", anomaly.event_type)).or_insert(0) += 1;
            
            let severity = if anomaly.anomaly_score >= 5.0 {
                "critical"
            } else if anomaly.anomaly_score >= 3.0 {
                "high"
            } else if anomaly.anomaly_score >= 2.0 {
                "medium"
            } else {
                "low"
            };
            *severity_distribution.entry(severity).or_insert(0) += 1;
        }
        
        serde_json::json!({
            "total_anomalies": results.len(),
            "analysis_window_minutes": self.config.analysis_window_minutes,
            "alert_threshold": self.config.alert_threshold,
            "min_samples_for_analysis": self.config.min_samples_for_analysis,
            "enabled_event_types": baselines.len(),
            "anomalies_by_type": type_counts,
            "severity_distribution": severity_distribution,
            "ml_detection_enabled": self.config.enable_ml_detection
        })
    }

    /// Cleanup old anomaly results
    pub fn cleanup_old_results(&self) -> Result<()> {
        let cutoff = Instant::now() - Duration::from_secs(self.config.retention_hours * 3600);
        let mut results = self.anomaly_results.lock().unwrap();
        
        let initial_count = results.len();
        results.retain(|a| a.detected_at >= cutoff);
        let removed_count = initial_count - results.len();
        
        if removed_count > 0 {
            info!("Cleaned up {} old anomaly results", removed_count);
        }
        
        Ok(())
    }
}

/// Global anomaly detection manager instance
static ANOMALY_DETECTION_MANAGER: std::sync::OnceLock<std::sync::Arc<AnomalyDetectionManager>> = std::sync::OnceLock::new();

/// Initialize global anomaly detection manager
pub fn initialize_anomaly_detection(config: AnomalyDetectionConfig) -> Result<()> {
    let manager = Arc::new(AnomalyDetectionManager::new(config));
    manager.initialize()?;
    ANOMALY_DETECTION_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("Anomaly detection manager already initialized"))?;
    Ok(())
}

/// Get global anomaly detection manager
pub fn get_anomaly_detection_manager() -> Option<Arc<AnomalyDetectionManager>> {
    ANOMALY_DETECTION_MANAGER.get().cloned()
}

/// Background task to cleanup old results
pub async fn cleanup_task() {
    let mut interval = tokio::time::interval(Duration::from_secs(3600)); // Every hour
    loop {
        interval.tick().await;
        if let Some(manager) = get_anomaly_detection_manager() {
            if let Err(e) = manager.cleanup_old_results() {
                error!("Anomaly detection cleanup error: {}", e);
            }
        }
    }
}

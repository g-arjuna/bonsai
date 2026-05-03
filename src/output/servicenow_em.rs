//! ServiceNow Event Management push task (T2-4).
//!
//! Polls the graph periodically for recently-fired DetectionEvents, applies the
//! configured severity / age / dedup filter (T2-6), and pushes qualifying events
//! to the ServiceNow `em_event` table.
//!
//! This is a one-off implementation that will be refactored to implement the
//! `OutputAdapter` trait (T6-1 / Sprint 7) when that lands.
//!
//! Severity mapping (ServiceNow):
//!   critical → 1 (Critical)
//!   high     → 2 (Major)
//!   warning  → 3 (Minor)
//!   info     → 5 (Informational / OK)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use lbug::Connection;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::config::ServiceNowConfig;
use crate::credentials::{CredentialVault, ResolvePurpose};
use crate::graph::common::read_str;

// ── Pending detection record (from graph query) ───────────────────────────────

#[derive(Debug)]
struct PendingDetection {
    id: String,
    device_address: String,
    hostname: String,
    rule_id: String,
    severity: String,
    fired_at_ns: i64,
}

// ── EM pusher ─────────────────────────────────────────────────────────────────

pub struct ServiceNowEmPusher {
    config: ServiceNowConfig,
}

impl ServiceNowEmPusher {
    pub fn new(config: ServiceNowConfig) -> Self {
        Self { config }
    }

    /// Spawn the background push loop. Returns immediately; the task runs until
    /// the shutdown signal fires.
    pub fn start(
        self,
        db: Arc<lbug::Database>,
        creds: Arc<CredentialVault>,
        audit_root: PathBuf,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::spawn(async move {
            info!("ServiceNow EM push task started");
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            // In-memory dedup: (device_address, rule_id) → last_pushed_ns
            let mut dedup: HashMap<(String, String), i64> = HashMap::new();
            // Track pushed detection IDs so we don't re-push after restart within the session.
            let mut pushed: HashSet<String> = HashSet::new();

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = self.push_cycle(&db, &creds, &audit_root, &mut dedup, &mut pushed).await {
                            warn!("ServiceNow EM push cycle failed: {e:#}");
                        }
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("ServiceNow EM push task shutting down");
                            break;
                        }
                    }
                }
            }
        });
    }

    async fn push_cycle(
        &self,
        db: &Arc<lbug::Database>,
        creds: &Arc<CredentialVault>,
        audit_root: &Path,
        dedup: &mut HashMap<(String, String), i64>,
        pushed: &mut HashSet<String>,
    ) -> Result<()> {
        let cred = creds
            .resolve(&self.config.credential_alias, ResolvePurpose::AiopsEvent)
            .context("resolve ServiceNow EM credential")?;

        let now_ns = crate::graph::common::now_ns();
        let min_age_ns = self.config.event_filter.min_age_secs as i64 * 1_000_000_000;
        let dedup_window_ns = self.config.event_filter.dedup_window_secs as i64 * 1_000_000_000;
        let min_severity = self.config.event_filter.min_severity.clone();

        // Query the most recent detections; age + dedup filtering happens below in-memory.
        let db2 = Arc::clone(db);
        let detections: Vec<PendingDetection> = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db2).context("open graph connection for EM query")?;
            let rows = conn
                .query(
                    "MATCH (dev:Device)-[:TRIGGERED]->(e:DetectionEvent) \
                 RETURN e.id, e.device_address, dev.hostname, e.rule_id, e.severity, \
                        e.fired_at \
                 ORDER BY e.fired_at DESC LIMIT 200",
                )
                .context("query pending detections")?;
            Ok::<Vec<PendingDetection>, anyhow::Error>(
                rows.map(|row| PendingDetection {
                    id: read_str(&row[0]),
                    device_address: read_str(&row[1]),
                    hostname: read_str(&row[2]),
                    rule_id: read_str(&row[3]),
                    severity: read_str(&row[4]),
                    fired_at_ns: crate::graph::common::read_ts_ns(&row[5]),
                })
                .collect(),
            )
        })
        .await
        .context("spawn_blocking panicked")??;

        let instance_url = self.config.instance_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("build reqwest client")?;

        let mut push_count = 0usize;
        for det in &detections {
            // Already pushed in this session
            if pushed.contains(&det.id) {
                continue;
            }

            // Severity filter
            if !severity_passes(&det.severity, &min_severity) {
                continue;
            }

            // Age filter: detection must be old enough to be "stable"
            if now_ns - det.fired_at_ns < min_age_ns {
                continue;
            }

            // Dedup filter
            let dedup_key = (det.device_address.clone(), det.rule_id.clone());
            if let Some(&last_ns) = dedup.get(&dedup_key)
                && now_ns - last_ns < dedup_window_ns
            {
                continue;
            }

            // Push to ServiceNow EM
            match push_em_event(&http, &instance_url, &cred.username, &cred.password, det).await {
                Ok(()) => {
                    pushed.insert(det.id.clone());
                    dedup.insert(dedup_key, now_ns);
                    push_count += 1;

                    // Audit the push
                    let ts = crate::graph::common::now_ns();
                    if let Err(e) = crate::audit::append_credential_resolve(
                        audit_root,
                        ts,
                        &self.config.credential_alias,
                        "aiops_event",
                        "ok",
                        None,
                    ) {
                        warn!("failed to write EM audit entry: {e}");
                    }
                }
                Err(e) => {
                    warn!(detection_id = %det.id, "failed to push EM event: {e:#}");
                }
            }
        }

        if push_count > 0 {
            debug!(pushed = push_count, "ServiceNow EM push cycle complete");
        }
        Ok(())
    }
}

fn severity_passes(event_severity: &str, min_severity: &str) -> bool {
    let rank = |s: &str| match s {
        "critical" => 3,
        "high" => 2,
        "warning" => 1,
        _ => 0,
    };
    rank(event_severity) >= rank(min_severity)
}

fn severity_to_snow(s: &str) -> &'static str {
    match s {
        "critical" => "1",
        "high" => "2",
        "warning" => "3",
        _ => "5",
    }
}

async fn push_em_event(
    http: &reqwest::Client,
    instance_url: &str,
    username: &str,
    password: &str,
    det: &PendingDetection,
) -> Result<()> {
    let node = if det.hostname.is_empty() {
        &det.device_address
    } else {
        &det.hostname
    };
    let url = format!("{instance_url}/api/now/em/inbound_event");
    let body = json!({
        "records": [{
            "source":      "bonsai",
            "node":        node,
            "type":        det.rule_id,
            "resource":    det.device_address,
            "severity":    severity_to_snow(&det.severity),
            "description": format!("Bonsai detection: {} on {}", det.rule_id, node),
            "message_key": format!("bonsai:{}", det.id),
            "additional_info": json!({
                "detection_id": det.id,
                "device_address": det.device_address,
                "rule_id": det.rule_id,
                "severity": det.severity,
                "fired_at_ns": det.fired_at_ns,
            }).to_string(),
        }]
    });

    let resp = http
        .post(&url)
        .basic_auth(username, Some(password))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("ServiceNow EM returned {status}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── severity_passes ───────────────────────────────────────────────────────

    #[test]
    fn critical_passes_any_min() {
        for min in ["info", "warning", "high", "critical"] {
            assert!(severity_passes("critical", min), "critical should pass min={min}");
        }
    }

    #[test]
    fn info_blocked_by_warning_min() {
        assert!(!severity_passes("info", "warning"));
        assert!(!severity_passes("info", "high"));
        assert!(!severity_passes("info", "critical"));
    }

    #[test]
    fn warning_passes_warning_and_below() {
        assert!(severity_passes("warning", "info"));
        assert!(severity_passes("warning", "warning"));
        assert!(!severity_passes("warning", "high"));
    }

    #[test]
    fn high_passes_high_and_below() {
        assert!(severity_passes("high", "info"));
        assert!(severity_passes("high", "warning"));
        assert!(severity_passes("high", "high"));
        assert!(!severity_passes("high", "critical"));
    }

    #[test]
    fn unknown_severity_treated_as_info() {
        // Unknown maps to rank 0, same as "info"
        assert!(severity_passes("unknown_severity", "info"));
        assert!(!severity_passes("unknown_severity", "warning"));
    }

    // ── severity_to_snow ──────────────────────────────────────────────────────

    #[test]
    fn severity_mapping_matches_snow_codes() {
        assert_eq!(severity_to_snow("critical"), "1");
        assert_eq!(severity_to_snow("high"), "2");
        assert_eq!(severity_to_snow("warning"), "3");
        assert_eq!(severity_to_snow("info"), "5");
        assert_eq!(severity_to_snow("anything_else"), "5");
    }
}

// ── Public constructor used by main.rs ────────────────────────────────────────

/// Start the EM push task if the config has it enabled.
pub fn maybe_start(
    config: &ServiceNowConfig,
    db: Arc<lbug::Database>,
    creds: Arc<CredentialVault>,
    audit_root: PathBuf,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if config.enabled && config.em_push_enabled {
        let pusher = ServiceNowEmPusher::new(config.clone());
        pusher.start(db, creds, audit_root, shutdown);
    }
}

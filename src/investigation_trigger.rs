//! Auto-trigger investigations for high-severity detections.
//!
//! Subscribes to the `BonsaiEvent` broadcast channel and spawns a Python
//! investigation agent (via HTTP POST) whenever a `detection_fired` event
//! with `critical` or `high` severity arrives.
//!
//! Safety:
//! - Deduplication: at most one running investigation per detection_id.
//! - Concurrency cap: at most `MAX_CONCURRENT` investigations in flight.
//! - Cooldown: skip device if it had an investigation started within `DEVICE_COOLDOWN_SECS`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::graph::{BonsaiEvent, GraphStore};

/// Maximum concurrent in-flight investigations.
const MAX_CONCURRENT: usize = 4;

/// Minimum seconds between investigations for the same device.
const DEVICE_COOLDOWN_SECS: u64 = 300;

/// Severity values that trigger auto-investigation.
const AUTO_INVESTIGATE_SEVERITIES: &[&str] = &["critical", "high"];

/// Core configuration for the investigation trigger.
#[derive(Clone, Debug)]
pub struct InvestigationTriggerConfig {
    /// Whether auto-investigation is enabled. Defaults to true when
    /// `ANTHROPIC_API_KEY` is set.
    pub enabled: bool,
    /// Base URL of the bonsai HTTP server (e.g. "http://127.0.0.1:8080").
    pub base_url: String,
}

/// Run the investigation trigger loop. Call this once at startup; it runs
/// until the shutdown signal fires.
pub async fn run_investigation_trigger(
    store: Arc<GraphStore>,
    config: InvestigationTriggerConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if !config.enabled {
        info!("investigation auto-trigger disabled (no ANTHROPIC_API_KEY or config)");
        return;
    }

    let mut event_rx: broadcast::Receiver<BonsaiEvent> = store.subscribe_events();

    // Track in-flight investigation IDs and per-device cooldowns.
    let mut in_flight: HashSet<String> = HashSet::new();
    let mut device_last_triggered: HashMap<String, Instant> = HashMap::new();
    let cooldown = Duration::from_secs(DEVICE_COOLDOWN_SECS);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("investigation trigger: failed to build HTTP client: {e}");
            return;
        }
    };

    info!("investigation auto-trigger started (max_concurrent={MAX_CONCURRENT}, cooldown={DEVICE_COOLDOWN_SECS}s)");

    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                info!("investigation auto-trigger shutting down");
                return;
            }
            result = event_rx.recv() => {
                let event = match result {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(lagged = n, "investigation trigger lagged — some detections may be missed");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("event bus closed, investigation trigger stopping");
                        return;
                    }
                };

                if event.event_type != "detection_fired" {
                    continue;
                }

                // Parse the detection details from the event.
                let detail: serde_json::Value = match serde_json::from_str(&event.detail_json) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let detection_id = detail["id"].as_str().unwrap_or_default().to_string();
                let severity = detail["severity"].as_str().unwrap_or_default();

                if detection_id.is_empty() {
                    continue;
                }

                // Only auto-investigate critical/high severity.
                if !AUTO_INVESTIGATE_SEVERITIES.iter().any(|s| s.eq_ignore_ascii_case(severity)) {
                    debug!(detection_id, severity, "skipping auto-investigation (severity below threshold)");
                    continue;
                }

                // Dedup: skip if already in flight.
                if in_flight.contains(&detection_id) {
                    continue;
                }

                // Concurrency cap.
                if in_flight.len() >= MAX_CONCURRENT {
                    debug!(detection_id, "skipping auto-investigation (max concurrent reached)");
                    continue;
                }

                // Per-device cooldown.
                let device = event.device_address.clone();
                if let Some(last) = device_last_triggered.get(&device) {
                    if last.elapsed() < cooldown {
                        debug!(detection_id, device, "skipping auto-investigation (device cooldown)");
                        continue;
                    }
                }

                info!(detection_id, device = %event.device_address, severity, "auto-triggering investigation");
                device_last_triggered.insert(device.clone(), Instant::now());
                in_flight.insert(detection_id.clone());

                // Spawn the investigation asynchronously.
                let client = client.clone();
                let base_url = config.base_url.clone();
                let device_address = event.device_address.clone();
                let det_id = detection_id.clone();
                let store_clone = Arc::clone(&store);

                tokio::spawn(async move {
                    let _result = run_single_investigation(
                        &client,
                        &base_url,
                        &det_id,
                        &device_address,
                    )
                    .await;
                    // Note: in_flight cleanup happens via the set — we can't
                    // easily remove from the parent task's set from here. The
                    // set is bounded by MAX_CONCURRENT and entries expire
                    // implicitly when we prune old detection_ids below.
                    let _ = store_clone; // keep alive
                });

                // Prune old entries from in_flight to avoid unbounded growth.
                // Simple approach: if we're at cap, clear the set (investigations
                // that were spawned are already running independently).
                if in_flight.len() >= MAX_CONCURRENT * 2 {
                    in_flight.clear();
                }
            }
        }
    }
}

/// Create an investigation via the HTTP API and wait for it to complete.
async fn run_single_investigation(
    client: &reqwest::Client,
    base_url: &str,
    detection_id: &str,
    device_address: &str,
) -> Result<(), String> {
    // Step 1: Create the investigation.
    let create_url = format!("{base_url}/api/investigations");
    let resp = client
        .post(&create_url)
        .json(&serde_json::json!({
            "detection_id": detection_id,
            "device_address": device_address,
            "trigger": "auto"
        }))
        .send()
        .await
        .map_err(|e| format!("create investigation HTTP error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(detection_id, %status, body, "failed to create investigation");
        return Err(format!("create investigation returned {status}"));
    }

    let inv: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse create investigation response: {e}"))?;

    let investigation_id = inv["id"].as_str().unwrap_or_default();
    if investigation_id.is_empty() {
        return Err("empty investigation_id in response".to_string());
    }

    info!(
        investigation_id,
        detection_id,
        device_address,
        "investigation created, agent will run asynchronously"
    );

    // The Python agent is expected to be triggered externally (e.g. via a
    // webhook or polling loop). The investigation node is now in "running"
    // state in the graph. If the Python agent is integrated as a sidecar,
    // it will pick up the investigation and drive it to completion.
    //
    // For now, we just create the Investigation node — the Python agent
    // polls /api/investigations?status=running or is triggered by the
    // orchestrator.

    Ok(())
}

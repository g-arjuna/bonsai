use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::event_bus::InProcessBus;
use crate::graph::{GraphStore, SubscriptionStatusWrite};
use crate::telemetry::{TelemetryEvent, TelemetryUpdate};

pub const VERIFICATION_WINDOW: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct SubscriptionPlan {
    pub target: String,
    pub paths: Vec<SubscriptionPathExpectation>,
}

#[derive(Clone, Debug)]
pub struct SubscriptionPathExpectation {
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
}

#[derive(Clone, Debug)]
struct TrackedPath {
    expectation: SubscriptionPathExpectation,
    status: String,
    first_observed_at_ns: i64,
    last_observed_at_ns: i64,
    deadline: Instant,
}

pub async fn run_subscription_verifier(
    store: Arc<GraphStore>,
    bus: Arc<InProcessBus>,
    mut plan_rx: tokio::sync::mpsc::Receiver<SubscriptionPlan>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut telemetry_rx = bus.subscribe();
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let mut tracked: HashMap<(String, String), TrackedPath> = HashMap::new();

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("subscription verifier received shutdown");
                break;
            }
            maybe_plan = plan_rx.recv() => {
                let Some(plan) = maybe_plan else {
                    info!("subscription verifier plan channel closed");
                    break;
                };
                register_plan(&store, &mut tracked, plan).await;
            }
            event = telemetry_rx.recv() => {
                match event {
                    Ok(update) => observe_update(&store, &mut tracked, update).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "subscription verifier lagged on telemetry bus");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = interval.tick() => {
                mark_silent_paths(&store, &mut tracked).await;
            }
        }
    }
}

async fn register_plan(
    store: &Arc<GraphStore>,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    plan: SubscriptionPlan,
) {
    let now = now_ns();
    let deadline = Instant::now() + VERIFICATION_WINDOW;
    let target = plan.target.clone();

    for expectation in plan.paths {
        let key = tracked_key(&target, &expectation);
        tracked.insert(
            key.clone(),
            TrackedPath {
                expectation: expectation.clone(),
                status: "pending".to_string(),
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                deadline,
            },
        );
        if let Err(error) = store
            .write_subscription_status(SubscriptionStatusWrite {
                device_address: target.clone(),
                path: expectation.path,
                origin: expectation.origin,
                mode: expectation.mode,
                sample_interval_ns: expectation.sample_interval_ns as i64,
                status: "pending".to_string(),
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                updated_at_ns: now,
            })
            .await
        {
            warn!(%error, target = %target, path_key = %key.1, "failed to write pending subscription status");
        }
    }
}

async fn observe_update(
    store: &Arc<GraphStore>,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    update: TelemetryUpdate,
) {
    let event = update.classify();
    let now = update.timestamp_ns.max(now_ns());
    let matching_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|((target, _), tracked_path)| {
            target == &update.target
                && path_matches_update(&tracked_path.expectation, &update, &event)
        })
        .map(|(key, _)| key.clone())
        .collect();

    for key in matching_keys {
        let Some(tracked_path) = tracked.get_mut(&key) else {
            continue;
        };
        if tracked_path.status != "observed" {
            tracked_path.first_observed_at_ns = now;
        }
        tracked_path.status = "observed".to_string();
        tracked_path.last_observed_at_ns = now;

        if let Err(error) = store
            .write_subscription_status(SubscriptionStatusWrite {
                device_address: key.0.clone(),
                path: tracked_path.expectation.path.clone(),
                origin: tracked_path.expectation.origin.clone(),
                mode: tracked_path.expectation.mode.clone(),
                sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
                status: tracked_path.status.clone(),
                first_observed_at_ns: tracked_path.first_observed_at_ns,
                last_observed_at_ns: tracked_path.last_observed_at_ns,
                updated_at_ns: now,
            })
            .await
        {
            warn!(%error, target = %key.0, path_key = %key.1, "failed to write observed subscription status");
        }
    }
}

async fn mark_silent_paths(
    store: &Arc<GraphStore>,
    tracked: &mut HashMap<(String, String), TrackedPath>,
) {
    let now_instant = Instant::now();
    let now = now_ns();
    let silent_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|(_, path)| path.status == "pending" && now_instant >= path.deadline)
        .map(|(key, _)| key.clone())
        .collect();

    for key in silent_keys {
        let Some(tracked_path) = tracked.get_mut(&key) else {
            continue;
        };
        tracked_path.status = "subscribed_but_silent".to_string();

        if let Err(error) = store
            .write_subscription_status(SubscriptionStatusWrite {
                device_address: key.0.clone(),
                path: tracked_path.expectation.path.clone(),
                origin: tracked_path.expectation.origin.clone(),
                mode: tracked_path.expectation.mode.clone(),
                sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
                status: tracked_path.status.clone(),
                first_observed_at_ns: 0,
                last_observed_at_ns: 0,
                updated_at_ns: now,
            })
            .await
        {
            warn!(%error, target = %key.0, path_key = %key.1, "failed to write silent subscription status");
        }
    }
}

fn path_matches_update(
    expectation: &SubscriptionPathExpectation,
    update: &TelemetryUpdate,
    event: &TelemetryEvent,
) -> bool {
    let expected = expectation.path.to_lowercase();
    let actual = update.path.to_lowercase();
    match event {
        TelemetryEvent::InterfaceStats { .. } => {
            expected.contains("statistics")
                || expected.contains("generic-counters")
                || (expected.contains("interfaces")
                    && expectation.mode.eq_ignore_ascii_case("SAMPLE"))
                || actual.contains("statistics")
                || actual.contains("generic-counters")
        }
        TelemetryEvent::InterfaceOperStatus { .. } => {
            expected.contains("oper-state")
                || (expected.contains("interfaces")
                    && expectation.mode.eq_ignore_ascii_case("ON_CHANGE"))
        }
        TelemetryEvent::BgpNeighborState { .. } => {
            expected.contains("bgp") || expected.contains("network-instances")
        }
        TelemetryEvent::BfdSessionState { .. } => expected.contains("bfd"),
        TelemetryEvent::LldpNeighbor { .. } => expected.contains("lldp"),
        TelemetryEvent::Ignored => false,
    }
}

fn tracked_key(target: &str, expectation: &SubscriptionPathExpectation) -> (String, String) {
    (
        target.to_string(),
        format!(
            "{}|{}|{}|{}",
            expectation.origin, expectation.mode, expectation.sample_interval_ns, expectation.path
        ),
    )
}

fn now_ns() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn expectation(path: &str, mode: &str) -> SubscriptionPathExpectation {
        SubscriptionPathExpectation {
            path: path.to_string(),
            origin: String::new(),
            mode: mode.to_string(),
            sample_interval_ns: 0,
        }
    }

    #[test]
    fn interface_counter_update_matches_sample_interface_expectation() {
        let update = TelemetryUpdate {
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            timestamp_ns: 1,
            path: "interfaces/interface[name=ethernet-1/1]/state/counters".to_string(),
            value: json!({"in-pkts": 1}),
        };
        let event = update.classify();

        assert!(path_matches_update(
            &expectation("interfaces", "SAMPLE"),
            &update,
            &event
        ));
        assert!(!path_matches_update(
            &expectation("interfaces", "ON_CHANGE"),
            &update,
            &event
        ));
    }

    #[test]
    fn bgp_update_matches_network_instances_expectation() {
        let update = TelemetryUpdate {
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            timestamp_ns: 1,
            path: "network-instances".to_string(),
            value: json!({
                "network-instance": {
                    "protocols": {
                        "protocol": {
                            "bgp": {
                                "neighbors": {
                                    "neighbor": {
                                        "neighbor-address": "192.0.2.1",
                                        "state": {"session-state": "ESTABLISHED"}
                                    }
                                }
                            }
                        }
                    }
                }
            }),
        };
        let event = update.classify();

        assert!(path_matches_update(
            &expectation("network-instances", "ON_CHANGE"),
            &update,
            &event
        ));
    }
}

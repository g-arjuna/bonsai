use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::event_bus::InProcessBus;
use crate::graph::{BonsaiEvent, SubscriptionStatusWrite};
use crate::store::BonsaiStore;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate};

pub const VERIFICATION_WINDOW: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct SubscriptionPlan {
    pub target: String,
    pub paths: Vec<SubscriptionPathExpectation>,
    /// Deterministic identity of the desired acquisition plan (hash of the
    /// normalized intent + path set). Stable across reconnects for an
    /// unchanged configuration; changes when the effective plan changes.
    pub plan_id: String,
    /// "submitted" — paths were sent in a Subscribe RPC.
    /// "disabled" — acquisition disabled; no RPC emitted.
    /// "rejected" — explicit plan failed validation; no RPC emitted.
    pub state: String,
    /// "automatic" | "explicit" | "disabled" — operator acquisition intent.
    pub acquisition: String,
    /// Device-facing subscription session that owns this plan's coverage.
    /// Minted per Subscribe session by the collector; empty for
    /// non-submitted states. A reconnect under the same plan_id carries a
    /// NEW session_id and must supersede, never merge with, the old one.
    pub session_id: String,
    /// Operator-visible detail (rejection reason, disable note).
    pub detail: String,
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

pub async fn run_subscription_verifier<S: BonsaiStore + 'static>(
    store: S,
    bus: Arc<InProcessBus>,
    mut plan_rx: tokio::sync::mpsc::Receiver<SubscriptionPlan>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let (sub, mut telemetry_rx) = crate::event_bus::MpscSubscriber::new(
        "subscription_verifier",
        1024,
        crate::event_bus::OverflowPolicy::DropNewest,
    );
    bus.add_subscriber(sub).await;

    let mut interval = tokio::time::interval(Duration::from_secs(5));
    let mut tracked: HashMap<(String, String), TrackedPath> = HashMap::new();
    // target -> last registered (plan_id, session_id). A reconnect under
    // an unchanged plan still mints a new session_id, so it is NOT
    // deduplicated: previous-session evidence becomes superseded.
    let mut registered_plans: HashMap<String, (String, String)> = HashMap::new();
    // target -> current authorized (plan_id, session_id) for coverage.
    let mut current_sessions: HashMap<String, (String, String)> = HashMap::new();

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
                let key = (plan.plan_id.clone(), plan.session_id.clone());
                if registered_plans.get(&plan.target) == Some(&key) {
                    continue;
                }
                registered_plans.insert(plan.target.clone(), key);
                if plan.state == "submitted" && !plan.session_id.is_empty() {
                    current_sessions.insert(
                        plan.target.clone(),
                        (plan.plan_id.clone(), plan.session_id.clone()),
                    );
                } else {
                    current_sessions.remove(&plan.target);
                }
                register_plan(&store, &mut tracked, plan).await;
            }
            event = telemetry_rx.recv() => {
                match event {
                    Some(update) => {
                        observe_update(&store, &mut tracked, &current_sessions, update).await;
                        // drain backlog without yielding — avoids queue saturation
                        while let Ok(update) = telemetry_rx.try_recv() {
                            observe_update(&store, &mut tracked, &current_sessions, update)
                                .await;
                        }
                    }
                    None => break,
                }
            }
            _ = interval.tick() => {
                mark_silent_paths(&store, &mut tracked).await;
            }
        }
    }
}

async fn register_plan<S: BonsaiStore>(
    store: &S,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    plan: SubscriptionPlan,
) {
    let now = now_ns();
    let target = plan.target.clone();
    let submitted = plan.state == "submitted";

    // Supersede every previously-tracked path for this target that is not
    // part of the new submitted plan, so an obsolete plan is never left
    // represented as current/active. Disabled and rejected plans supersede
    // the entire previous plan.
    let new_keys: std::collections::HashSet<String> = plan
        .paths
        .iter()
        .map(|e| tracked_key(&target, e).1)
        .collect();
    let stale_keys: Vec<(String, String)> = tracked
        .iter()
        .filter(|((t, k), _)| t == &target && !(submitted && new_keys.contains(k)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in stale_keys {
        if let Some(tracked_path) = tracked.remove(&key) {
            let status = SubscriptionStatusWrite {
                device_address: target.clone(),
                path: tracked_path.expectation.path.clone(),
                origin: tracked_path.expectation.origin.clone(),
                mode: tracked_path.expectation.mode.clone(),
                sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
                status: "superseded".to_string(),
                first_observed_at_ns: tracked_path.first_observed_at_ns,
                last_observed_at_ns: tracked_path.last_observed_at_ns,
                updated_at_ns: now,
            };
            let event = subscription_status_event(&status);
            if let Err(error) = store.write_subscription_status(status).await {
                warn!(%error, target = %target, path_key = %key.1, "failed to write superseded subscription status");
            } else {
                store.publish_event(event);
            }
        }
    }

    // Supersede PERSISTED statuses outside the new plan as well. The
    // in-memory `tracked` sweep above only covers this process's memory;
    // after a restart it is empty while the graph still holds statuses
    // written by the previous plan. Without this, an obsolete plan's rows
    // would remain displayed as current alongside (or instead of) the new
    // plan. Disabled/rejected plans keep nothing — every persisted row for
    // the target is superseded.
    let keep: std::collections::HashSet<(String, String, String, i64)> = if submitted {
        plan.paths
            .iter()
            .map(|e| {
                (
                    e.path.clone(),
                    e.origin.clone(),
                    e.mode.clone(),
                    e.sample_interval_ns as i64,
                )
            })
            .collect()
    } else {
        std::collections::HashSet::new()
    };
    if let Err(error) = store
        .supersede_subscription_statuses_outside_plan(&target, &keep)
        .await
    {
        warn!(%error, target = %target, "failed to supersede persisted subscription statuses");
    }

    if !submitted {
        info!(
            target = %target,
            plan_id = %plan.plan_id,
            state = %plan.state,
            acquisition = %plan.acquisition,
            detail = %plan.detail,
            "acquisition plan not submitted to device"
        );
        publish_plan_state(store, &plan).await;
        return;
    }

    let deadline = Instant::now() + VERIFICATION_WINDOW;

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
        let status = SubscriptionStatusWrite {
            device_address: target.clone(),
            path: expectation.path,
            origin: expectation.origin,
            mode: expectation.mode,
            sample_interval_ns: expectation.sample_interval_ns as i64,
            status: "pending".to_string(),
            first_observed_at_ns: 0,
            last_observed_at_ns: 0,
            updated_at_ns: now,
        };
        let event = subscription_status_event(&status);
        if let Err(error) = store.write_subscription_status(status).await {
            warn!(%error, target = %target, path_key = %key.1, "failed to write pending subscription status");
        } else {
            store.publish_event(event);
        }
    }
}

async fn observe_update<S: BonsaiStore>(
    store: &S,
    tracked: &mut HashMap<(String, String), TrackedPath>,
    current_sessions: &HashMap<String, (String, String)>,
    update: std::sync::Arc<TelemetryUpdate>,
) {
    if !update_has_current_session(current_sessions, &update) {
        return;
    }
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
        let status_changed = tracked_path.status != "observed";
        if status_changed {
            tracked_path.first_observed_at_ns = now;
        }
        tracked_path.status = "observed".to_string();
        tracked_path.last_observed_at_ns = now;

        // Only write to the store on the first transition — avoids a DB write per telemetry update
        if status_changed {
            let status = SubscriptionStatusWrite {
                device_address: key.0.clone(),
                path: tracked_path.expectation.path.clone(),
                origin: tracked_path.expectation.origin.clone(),
                mode: tracked_path.expectation.mode.clone(),
                sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
                status: tracked_path.status.clone(),
                first_observed_at_ns: tracked_path.first_observed_at_ns,
                last_observed_at_ns: tracked_path.last_observed_at_ns,
                updated_at_ns: now,
            };
            let event = subscription_status_event(&status);
            if let Err(error) = store.write_subscription_status(status).await {
                warn!(%error, target = %key.0, path_key = %key.1, "failed to write observed subscription status");
            } else {
                store.publish_event(event);
            }
        }
    }
}

async fn mark_silent_paths<S: BonsaiStore>(
    store: &S,
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

        let status = SubscriptionStatusWrite {
            device_address: key.0.clone(),
            path: tracked_path.expectation.path.clone(),
            origin: tracked_path.expectation.origin.clone(),
            mode: tracked_path.expectation.mode.clone(),
            sample_interval_ns: tracked_path.expectation.sample_interval_ns as i64,
            status: tracked_path.status.clone(),
            first_observed_at_ns: 0,
            last_observed_at_ns: 0,
            updated_at_ns: now,
        };
        let event = subscription_status_event(&status);
        if let Err(error) = store.write_subscription_status(status).await {
            warn!(%error, target = %key.0, path_key = %key.1, "failed to write silent subscription status");
        } else {
            store.publish_event(event);
        }
    }
}

async fn publish_plan_state<S: BonsaiStore>(store: &S, plan: &SubscriptionPlan) {
    let event = BonsaiEvent {
        device_address: plan.target.clone(),
        event_type: "subscription_plan_state".to_string(),
        detail_json: serde_json::json!({
            "plan_id": &plan.plan_id,
            "state": &plan.state,
            "acquisition": &plan.acquisition,
            "detail": &plan.detail,
            "paths": plan.paths.len(),
        })
        .to_string(),
        occurred_at_ns: now_ns(),
        state_change_event_id: String::new(),
        source_type: "gnmi".to_string(),
    };
    store.publish_event(event);
}

fn subscription_status_event(status: &SubscriptionStatusWrite) -> BonsaiEvent {
    BonsaiEvent {
        device_address: status.device_address.clone(),
        event_type: "subscription_status_change".to_string(),
        detail_json: serde_json::json!({
            "path": &status.path,
            "origin": &status.origin,
            "mode": &status.mode,
            "sample_interval_ns": status.sample_interval_ns,
            "status": &status.status,
            "first_observed_at_ns": status.first_observed_at_ns,
            "last_observed_at_ns": status.last_observed_at_ns,
        })
        .to_string(),
        occurred_at_ns: status.updated_at_ns,
        state_change_event_id: String::new(),
        source_type: "gnmi".to_string(),
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
        TelemetryEvent::InterfaceStats { .. }
        | TelemetryEvent::InterfaceSummary { .. }
        | TelemetryEvent::SflowCounters { .. } => {
            expected.contains("statistics")
                || expected.contains("generic-counters")
                || (expected.contains("interfaces")
                    && expectation.mode.eq_ignore_ascii_case("SAMPLE"))
                || actual.contains("statistics")
                || actual.contains("generic-counters")
                || actual.contains("summary")
        }
        TelemetryEvent::InterfaceDescription { .. } => expected.contains("description"),
        TelemetryEvent::ServiceEndpoint { .. }
        | TelemetryEvent::QoSPolicyChange { .. }
        | TelemetryEvent::SyslogEvent { .. }
        | TelemetryEvent::SyslogFact { .. }
        | TelemetryEvent::SnmpTrap { .. }
        | TelemetryEvent::SnmpFact { .. }
        | TelemetryEvent::BmpInitiation
        | TelemetryEvent::BmpStatisticsReport
        | TelemetryEvent::OtlpMetrics { .. }
        | TelemetryEvent::NetflowRecord { .. }
        | TelemetryEvent::SflowRecord { .. }
        | TelemetryEvent::EnvSensor { .. }
        | TelemetryEvent::OpticsDiagnostics { .. }
        | TelemetryEvent::OpticalChannel { .. }
        | TelemetryEvent::PowerUnit { .. } => {
            actual.contains(&expected) || expected.contains(&actual)
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
        TelemetryEvent::IsisAdjacencyState { .. } => expected.contains("isis"),
        TelemetryEvent::LldpNeighbor { .. } => expected.contains("lldp"),
        TelemetryEvent::ConfigChange { .. } => false,
        TelemetryEvent::BmpPeerState => false,
        TelemetryEvent::BmpRouteMonitoring => false,
        TelemetryEvent::BgpLsState => false,
        TelemetryEvent::OtlpSpan { .. } => false,
        TelemetryEvent::Ignored => false,
    }
}

/// Session fencing (M5.2B): only observations carrying the CURRENT
/// authorized (plan_id, session_id) pair may satisfy coverage.
/// Legacy/empty provenance and late data from a superseded session are
/// received telemetry but grant no subscription coverage.
fn update_has_current_session(
    current_sessions: &HashMap<String, (String, String)>,
    update: &TelemetryUpdate,
) -> bool {
    current_sessions
        .get(&update.target)
        .is_some_and(|(plan_id, session_id)| {
            !session_id.is_empty() && update.plan_id == *plan_id && update.session_id == *session_id
        })
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
            plan_id: String::new(),
            session_id: String::new(),
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            role: String::new(),
            site: String::new(),
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
            plan_id: String::new(),
            session_id: String::new(),
            target: "dut:57400".to_string(),
            vendor: "openconfig".to_string(),
            hostname: "dut".to_string(),
            role: String::new(),
            site: String::new(),
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

    fn provenance_update(target: &str, plan_id: &str, session_id: &str) -> TelemetryUpdate {
        TelemetryUpdate {
            plan_id: plan_id.to_string(),
            session_id: session_id.to_string(),
            target: target.to_string(),
            vendor: String::new(),
            hostname: String::new(),
            role: String::new(),
            site: String::new(),
            timestamp_ns: 1,
            path: "interfaces".to_string(),
            value: json!({}),
        }
    }

    #[test]
    fn current_session_provenance_authorizes_coverage() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "dut:57400".to_string(),
            ("plan-1".to_string(), "sess-a".to_string()),
        );
        assert!(update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
    }

    #[test]
    fn superseded_session_cannot_satisfy_current_coverage() {
        let mut sessions = HashMap::new();
        sessions.insert(
            "dut:57400".to_string(),
            ("plan-1".to_string(), "sess-b".to_string()),
        );
        // Same plan_id, previous session — late/reconnect data fenced.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
        // Different plan entirely.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-2", "sess-b")
        ));
        // Legacy/empty provenance never satisfies a session-bound plan.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "", "")
        ));
        // Wrong target.
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("other:57400", "plan-1", "sess-b")
        ));
    }

    #[test]
    fn non_session_plans_grant_no_coverage() {
        // A submitted plan without session provenance (legacy producer)
        // leaves no current session — nothing is authorized.
        let sessions = HashMap::new();
        assert!(!update_has_current_session(
            &sessions,
            &provenance_update("dut:57400", "plan-1", "sess-a")
        ));
    }
}

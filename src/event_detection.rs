use std::sync::Arc;

use serde_json::Value as JsonValue;
use tracing::warn;

use crate::graph::{BonsaiEvent, GraphStore};

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectionCandidate {
    device_address: String,
    rule_id: String,
    severity: String,
    features_json: String,
    fired_at_ns: i64,
    state_change_event_id: String,
}

pub fn start(store: Arc<GraphStore>) {
    let mut rx = store.subscribe_events();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let Some(candidate) = detection_candidate_from_event(&event) else {
                        continue;
                    };
                    if let Err(error) = store
                        .write_detection(
                            candidate.device_address,
                            candidate.rule_id,
                            candidate.severity,
                            candidate.features_json,
                            candidate.fired_at_ns,
                            candidate.state_change_event_id,
                        )
                        .await
                    {
                        warn!(%error, "failed to persist event-driven detection");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "event-detection lagged behind the graph event bus");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn detection_candidate_from_event(event: &BonsaiEvent) -> Option<DetectionCandidate> {
    match event.event_type.as_str() {
        "bgp_session_change" => map_bgp_detection(event),
        "bfd_session_change" => map_bfd_detection(event),
        "interface_oper_status_change" => map_interface_detection(event),
        _ => None,
    }
}

fn map_bgp_detection(event: &BonsaiEvent) -> Option<DetectionCandidate> {
    let detail: JsonValue = serde_json::from_str(&event.detail_json).ok()?;
    let old_state = detail
        .get("old_state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let new_state = detail
        .get("new_state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if old_state != "established" || new_state == "established" || new_state.is_empty() {
        return None;
    }

    Some(DetectionCandidate {
        device_address: event.device_address.clone(),
        rule_id: "bgp_session_down".to_string(),
        severity: "critical".to_string(),
        features_json: event.detail_json.clone(),
        fired_at_ns: event.occurred_at_ns,
        state_change_event_id: event.state_change_event_id.clone(),
    })
}

fn map_bfd_detection(event: &BonsaiEvent) -> Option<DetectionCandidate> {
    let detail: JsonValue = serde_json::from_str(&event.detail_json).ok()?;
    let old_state = detail
        .get("old_state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let new_state = detail
        .get("new_state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if old_state != "up" || new_state == "up" || new_state.is_empty() {
        return None;
    }

    Some(DetectionCandidate {
        device_address: event.device_address.clone(),
        rule_id: "bfd_session_down".to_string(),
        severity: "critical".to_string(),
        features_json: event.detail_json.clone(),
        fired_at_ns: event.occurred_at_ns,
        state_change_event_id: event.state_change_event_id.clone(),
    })
}

fn map_interface_detection(event: &BonsaiEvent) -> Option<DetectionCandidate> {
    let detail: JsonValue = serde_json::from_str(&event.detail_json).ok()?;
    let old_state = detail
        .get("old_state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let new_state = detail
        .get("new_state")
        .or_else(|| detail.get("oper_status"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if old_state.is_empty() || old_state == "down" || new_state != "down" {
        return None;
    }

    Some(DetectionCandidate {
        device_address: event.device_address.clone(),
        rule_id: "interface_down".to_string(),
        severity: "warning".to_string(),
        features_json: event.detail_json.clone(),
        fired_at_ns: event.occurred_at_ns,
        state_change_event_id: event.state_change_event_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(event_type: &str, detail_json: &str) -> BonsaiEvent {
        BonsaiEvent {
            device_address: "leaf1".to_string(),
            event_type: event_type.to_string(),
            detail_json: detail_json.to_string(),
            occurred_at_ns: 123,
            state_change_event_id: "state-1".to_string(),
        }
    }

    #[test]
    fn bgp_down_transition_becomes_detection() {
        let candidate = detection_candidate_from_event(&sample_event(
            "bgp_session_change",
            r#"{"peer":"10.0.0.1","old_state":"established","new_state":"idle"}"#,
        ))
        .expect("candidate");
        assert_eq!(candidate.rule_id, "bgp_session_down");
        assert_eq!(candidate.severity, "critical");
    }

    #[test]
    fn initial_bgp_state_does_not_fire_detection() {
        let candidate = detection_candidate_from_event(&sample_event(
            "bgp_session_change",
            r#"{"peer":"10.0.0.1","old_state":"none","new_state":"idle"}"#,
        ));
        assert!(candidate.is_none());
    }

    #[test]
    fn interface_down_becomes_detection() {
        let candidate = detection_candidate_from_event(&sample_event(
            "interface_oper_status_change",
            r#"{"if_name":"ethernet-1/1","old_state":"up","new_state":"down","oper_status":"down"}"#,
        ))
        .expect("candidate");
        assert_eq!(candidate.rule_id, "interface_down");
        assert_eq!(candidate.severity, "warning");
    }

    #[test]
    fn initial_interface_down_does_not_fire_detection() {
        let candidate = detection_candidate_from_event(&sample_event(
            "interface_oper_status_change",
            r#"{"if_name":"ethernet-1/1","old_state":"","new_state":"down","oper_status":"down"}"#,
        ));
        assert!(candidate.is_none());
    }
}

use std::time::Duration;

use bonsai::config::TargetConfig;
use bonsai::correlation_buffer::{CorrelationBuffer, CorrelationKey};
use bonsai::graph::GraphStore;
use bonsai::streaming::bmp::{BmpEvent, BmpTargetMap};
use bonsai::telemetry::TelemetryUpdate;

fn sample_bmp_event(session_state: &str, message_type: &str, peer_address: &str, rd: u64) -> BmpEvent {
    BmpEvent {
        timestamp_ns: 1_700_000_000_000_000_000,
        collector_peer: "172.20.0.99:41234".to_string(),
        message_type: message_type.to_string(),
        peer_type: if rd == 0 { 0 } else { 1 },
        peer_flags: 0,
        router_distinguisher: rd,
        router_address: "10.0.0.1".to_string(),
        peer_address: peer_address.to_string(),
        peer_as: 65001,
        peer_bgp_id: "10.0.0.1".to_string(),
        session_state: session_state.to_string(),
        route_entries: Vec::new(),
        raw_len: 48,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name: None,
        sys_descr: None,
        termination_reason: None,
        termination_reason_name: None,
        rib_type: None,
        peer_up_info: None,
        init_admin_string: None,
        termination_message: None,
    }
}

#[tokio::test]
async fn b09_bmp_peer_state_transition_emits_state_change_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store =
        GraphStore::open(db_path.to_str().unwrap(), 256 * 1024 * 1024).expect("open graph");

    let up_event = sample_bmp_event("Established", "peer_up", "10.0.0.2", 0);
    store
        .write(TelemetryUpdate {
            timestamp_ns: 1_700_000_000_000_000_000,
            target: "172.20.0.11:57400".to_string(),
            vendor: "frr".to_string(),
            hostname: "edge-a".to_string(),
            role: "pe".to_string(),
            site: "dc1".to_string(),
            path: "streaming/bmp/peer-up".to_string(),
            value: serde_json::to_value(&up_event).unwrap(),
        })
        .await
        .expect("write peer up");

    let down_event = sample_bmp_event("Down", "peer_down", "10.0.0.2", 0);
    store
        .write(TelemetryUpdate {
            timestamp_ns: 1_700_000_005_000_000_000,
            target: "172.20.0.11:57400".to_string(),
            vendor: "frr".to_string(),
            hostname: "edge-a".to_string(),
            role: "pe".to_string(),
            site: "dc1".to_string(),
            path: "streaming/bmp/peer-down".to_string(),
            value: serde_json::to_value(&down_event).unwrap(),
        })
        .await
        .expect("write peer down");

    // Repeated identical Down state must NOT emit a duplicate transition.
    store
        .write(TelemetryUpdate {
            timestamp_ns: 1_700_000_006_000_000_000,
            target: "172.20.0.11:57400".to_string(),
            vendor: "frr".to_string(),
            hostname: "edge-a".to_string(),
            role: "pe".to_string(),
            site: "dc1".to_string(),
            path: "streaming/bmp/peer-down".to_string(),
            value: serde_json::to_value(&down_event).unwrap(),
        })
        .await
        .expect("write repeated peer down");

    // Same peer IP in a different VRF (RD=200) must be tracked independently.
    let vrf200_down = sample_bmp_event("Down", "peer_down", "10.0.0.2", 200);
    store
        .write(TelemetryUpdate {
            timestamp_ns: 1_700_000_007_000_000_000,
            target: "172.20.0.11:57400".to_string(),
            vendor: "frr".to_string(),
            hostname: "edge-a".to_string(),
            role: "pe".to_string(),
            site: "dc1".to_string(),
            path: "streaming/bmp/peer-down".to_string(),
            value: serde_json::to_value(&vrf200_down).unwrap(),
        })
        .await
        .expect("write vrf 200 peer down");

    let events = store
        .read_events_history(Some("bmp".to_string()), None, None, 20)
        .await
        .expect("read bmp events");
    assert_eq!(
        events.len(),
        3,
        "initial PeerUp (RD=0), PeerDown (RD=0), and independent VRF PeerDown (RD=200) must emit 3 bmp_session_change events"
    );
}

#[test]
fn b09_bmp_target_map_does_not_attribute_unknown_exporter_to_remote_peer() {
    let remote_peer_target = TargetConfig {
        address: "10.0.0.1:57400".to_string(),
        hostname: Some("remote-peer-device".to_string()),
        vendor: Some("cisco_xrd".to_string()),
        ..Default::default()
    };
    let map = BmpTargetMap::new(&[remote_peer_target]);
    let event = sample_bmp_event("Established", "peer_up", "10.0.0.1", 0);
    let resolved = map.resolve(&event);
    assert_eq!(
        resolved.address, "172.20.0.99",
        "unknown BMP exporter 172.20.0.99 must not be misattributed to onboarded remote peer 10.0.0.1"
    );
}

#[test]
fn b09_correlation_buffer_preserves_expired_slot_and_avoids_ambiguous_wildcard_merge() {
    // 1. Expired slot must not be dropped when a new arrival hits the same key before drain_expired().
    let buf = CorrelationBuffer::new(0); // 0s window expires immediately
    let key = CorrelationKey::new("172.20.0.11", "bgp_neighbor_down", "10.0.0.2");
    buf.record(
        key.clone(),
        "evt-1".to_string(),
        "bmp".to_string(),
        1_000,
        "{}".to_string(),
    );
    std::thread::sleep(Duration::from_millis(5));
    buf.record(
        key.clone(),
        "evt-2".to_string(),
        "syslog".to_string(),
        2_000,
        "{}".to_string(),
    );
    let drained = buf.drain_all();
    let all_ids: Vec<String> = drained
        .into_iter()
        .flat_map(|s| s.state_change_event_ids)
        .collect();
    assert!(
        all_ids.contains(&"evt-1".to_string()) && all_ids.contains(&"evt-2".to_string()),
        "expired slot evt-1 must be preserved for draining rather than overwritten: got {all_ids:?}"
    );

    // 2. Two open peers on the same device must not arbitrarily absorb an ambiguous empty-sub_key event.
    let buf2 = CorrelationBuffer::new(60);
    buf2.record(
        CorrelationKey::new("172.20.0.11", "bgp_neighbor_down", "10.0.0.2"),
        "peer-2-down".to_string(),
        "bmp".to_string(),
        1_000,
        "{}".to_string(),
    );
    buf2.record(
        CorrelationKey::new("172.20.0.11", "bgp_neighbor_down", "10.0.0.3"),
        "peer-3-down".to_string(),
        "bmp".to_string(),
        1_001,
        "{}".to_string(),
    );
    buf2.record(
        CorrelationKey::new("172.20.0.11", "bgp_neighbor_down", ""),
        "ambiguous-snmp".to_string(),
        "snmp".to_string(),
        1_002,
        "{}".to_string(),
    );
    assert_eq!(
        buf2.len(),
        3,
        "when two distinct peer slots are open, an unkeyed SNMP event is ambiguous and must remain in its own slot"
    );
}

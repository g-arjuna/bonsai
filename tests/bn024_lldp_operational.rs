//! BN-024 operational LLDP projection regressions.

use bonsai::graph::GraphStore;
use bonsai::telemetry::TelemetryUpdate;

fn xrd_multi_update(is_delete: bool, timestamp_ns: i64) -> TelemetryUpdate {
    TelemetryUpdate {
        target: "172.20.0.11:57400".into(),
        vendor: "cisco".into(),
        hostname: "xrd1".into(),
        role: "spine".into(),
        site: "lab".into(),
        timestamp_ns,
        path: "lldp/nodes/node[node-name=0/RP0/CPU0]/neighbors/details/detail[interface-name=GigabitEthernet0/0/0/0][device-id=leaf1]".into(),
        value: if is_delete {
            serde_json::json!({})
        } else {
            serde_json::json!({
                "lldp-neighbor": [
                    {"chassis-id":"aa:aa:aa:aa:aa:01","port-id-detail":"GigabitEthernet0/0/0/1","detail":{"system-name":"leaf1"}},
                    {"chassis-id":"aa:aa:aa:aa:aa:02","port-id-detail":"GigabitEthernet0/0/0/2","detail":{"system-name":"leaf2"}}
                ]
            })
        },
        plan_id: String::new(),
        session_id: String::new(),
        source_protocol: String::new(),
        gnmi_path: vec![],
        is_delete,
        collector_id: String::new(),
    }
}

#[tokio::test]
async fn xrd_multi_neighbor_event_persists_every_identity_keyed_entry() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(xrd_multi_update(false, 1_700_000_000_000_000_000))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.11")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.lifecycle == "active"));
    assert!(rows.iter().all(|row| row.neighbor_id == "leaf1"));
    assert!(
        rows.iter()
            .any(|row| row.chassis_id == "aa:aa:aa:aa:aa:01" && row.system_name == "leaf1")
    );
    assert!(
        rows.iter()
            .any(|row| row.chassis_id == "aa:aa:aa:aa:aa:02" && row.system_name == "leaf2")
    );
    assert_ne!(rows[0].id, rows[1].id);
    assert!(
        rows.iter()
            .all(|row| row.neighbor_key.contains("aa:aa:aa:aa:aa:0"))
    );
}

#[tokio::test]
async fn xrd_delete_withdraws_path_evidence_without_erasing_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(xrd_multi_update(false, 1_700_000_000_000_000_000))
        .await
        .unwrap();
    store
        .write(xrd_multi_update(true, 1_700_000_010_000_000_000))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.11")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.lifecycle == "withdrawn"));
    assert!(rows.iter().all(|row| !row.chassis_id.is_empty()));
}

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
            .all(|row| row.neighbor_key.starts_with("leaf1:proto:"))
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

fn ceos_update(is_delete: bool, timestamp_ns: i64, chassis_id: &str) -> TelemetryUpdate {
    TelemetryUpdate {
        target: "172.20.0.21:6030".into(),
        vendor: "arista".into(),
        hostname: "ceos1".into(),
        role: "leaf".into(),
        site: "lab".into(),
        timestamp_ns,
        path: "lldp/interfaces/interface[name=Ethernet1]/neighbors/neighbor[id=1]/state".into(),
        value: if is_delete {
            serde_json::json!({})
        } else {
            serde_json::json!({
                "chassis-id": chassis_id,
                "system-name": "spine1",
                "port-id": "Ethernet1"
            })
        },
        plan_id: "plan-1".into(),
        session_id: "session-1".into(),
        source_protocol: "gnmi".into(),
        gnmi_path: vec![],
        is_delete,
        collector_id: "collector-a".into(),
    }
}

#[tokio::test]
async fn delayed_older_observation_does_not_resurrect_withdrawn_xrd_entries() {
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
    store
        .write(xrd_multi_update(false, 1_700_000_000_000_000_000))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.11")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.lifecycle == "withdrawn"));
}

#[tokio::test]
async fn delayed_older_withdrawal_does_not_withdraw_newer_observation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(xrd_multi_update(false, 1_700_000_010_000_000_000))
        .await
        .unwrap();
    store
        .write(xrd_multi_update(true, 1_700_000_000_000_000_000))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.11")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.lifecycle == "active"));
}

#[tokio::test]
async fn equal_time_conflicting_observation_is_marked_uncertain_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(ceos_update(false, 1_700_000_000_000_000_000, "aa:01"))
        .await
        .unwrap();
    store
        .write(ceos_update(false, 1_700_000_000_000_000_000, "bb:02"))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.21")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].chassis_id, "aa:01");
    assert_eq!(rows[0].ordering_state, "conflicting");
    assert!(!rows[0].ordering_detail.is_empty());
}

#[tokio::test]
async fn state_container_delete_is_not_positive_neighbor_occurrence_withdrawal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(ceos_update(false, 1_700_000_000_000_000_000, "aa:01"))
        .await
        .unwrap();
    store
        .write(ceos_update(true, 1_700_000_010_000_000_000, "aa:01"))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.21")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, "active");
}

fn interface_update(
    target: &str,
    hostname: &str,
    ifname: &str,
    timestamp_ns: i64,
) -> TelemetryUpdate {
    TelemetryUpdate {
        target: target.into(),
        vendor: "arista".into(),
        hostname: hostname.into(),
        role: "leaf".into(),
        site: "lab".into(),
        timestamp_ns,
        path: format!("interfaces/interface[name={ifname}]/state/counters"),
        value: serde_json::json!({"in-pkts": 1, "out-pkts": 1}),
        plan_id: String::new(),
        session_id: String::new(),
        source_protocol: "gnmi".into(),
        gnmi_path: vec![],
        is_delete: false,
        collector_id: String::new(),
    }
}

fn ceos_projection_update(neighbor_id: &str, timestamp_ns: i64) -> TelemetryUpdate {
    TelemetryUpdate {
        target: "172.20.0.21:6030".into(),
        vendor: "arista".into(),
        hostname: "ceos1".into(),
        role: "leaf".into(),
        site: "lab".into(),
        timestamp_ns,
        path: format!(
            "lldp/interfaces/interface[name=Ethernet1]/neighbors/neighbor[id={neighbor_id}]/state"
        ),
        value: serde_json::json!({
            "chassis-id": "aa:leaf:01",
            "chassis-id-subtype": "mac-address",
            "system-name": "leaf1",
            "port-id": "Ethernet1",
            "port-id-subtype": "interface-name"
        }),
        plan_id: "plan-1".into(),
        session_id: "session-1".into(),
        source_protocol: "gnmi".into(),
        gnmi_path: vec![],
        is_delete: false,
        collector_id: "collector-a".into(),
    }
}

fn ceos_exact_delete(neighbor_id: &str, timestamp_ns: i64) -> TelemetryUpdate {
    let mut update = ceos_projection_update(neighbor_id, timestamp_ns);
    update.path =
        format!("lldp/interfaces/interface[name=Ethernet1]/neighbors/neighbor[id={neighbor_id}]");
    update.value = serde_json::json!({});
    update.is_delete = true;
    update
}

fn projection_support_rows(db_path: &std::path::Path) -> Vec<(String, String, String)> {
    let sysconfig = lbug::SystemConfig::default().buffer_pool_size(128 * 1024 * 1024);
    let db = lbug::Database::new(db_path.to_str().unwrap(), sysconfig).expect("open raw graph");
    let conn = lbug::Connection::new(&db).expect("raw connection");
    let mut stmt = conn
        .prepare(
            "MATCH (li:Interface {id: '172.20.0.21:Ethernet1'})-[r:CONNECTED_TO]->(ri:Interface) \
             RETURN ri.id, r.projection_owner, r.lldp_supporting_ids",
        )
        .expect("prepare projection query");
    conn.execute(&mut stmt, vec![])
        .expect("query projection")
        .map(|row| {
            let text = |idx: usize| match row.get(idx) {
                Some(lbug::Value::String(value)) => value.clone(),
                _ => String::new(),
            };
            (text(0), text(1), text(2))
        })
        .collect()
}

#[tokio::test]
async fn collector_uses_same_evidence_lifecycle_without_core_topology_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("collector-graph.db");
    let collector = bonsai::collector::graph::CollectorGraphStore::open(
        db_path.to_str().unwrap(),
        128 * 1024 * 1024,
    )
    .expect("open collector graph");
    collector
        .write(ceos_projection_update("1", 1_700_000_000_000_000_000))
        .await
        .unwrap();
    collector
        .write(ceos_exact_delete("1", 1_700_000_010_000_000_000))
        .await
        .unwrap();
    collector
        .write(ceos_projection_update("1", 1_700_000_000_000_000_000))
        .await
        .unwrap();

    let sysconfig = lbug::SystemConfig::default().buffer_pool_size(128 * 1024 * 1024);
    let db =
        lbug::Database::new(db_path.to_str().unwrap(), sysconfig).expect("raw collector graph");
    let conn = lbug::Connection::new(&db).expect("raw collector connection");
    let mut stmt = conn
        .prepare(
            "MATCH (n:LldpNeighbor) RETURN n.lifecycle, n.population_entries_encountered, n.ordering_state",
        )
        .expect("prepare collector lifecycle query");
    let rows: Vec<(String, i64, String)> = conn
        .execute(&mut stmt, vec![])
        .expect("query collector lifecycle")
        .map(|row| {
            let text = |idx: usize| match row.get(idx) {
                Some(lbug::Value::String(value)) => value.clone(),
                _ => String::new(),
            };
            let integer = match row.get(1) {
                Some(lbug::Value::Int64(value)) => *value,
                _ => -1,
            };
            (text(0), integer, text(2))
        })
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "withdrawn");
    assert_eq!(rows[0].1, 1);
    assert_eq!(rows[0].2, "ordered");

    let mut edge_stmt = conn
        .prepare("MATCH ()-[r:CONNECTED_TO]->() RETURN count(r)")
        .expect("prepare collector edge query");
    let count = conn
        .execute(&mut edge_stmt, vec![])
        .expect("query collector edges")
        .next()
        .and_then(|row| match row.get(0) {
            Some(lbug::Value::Int64(value)) => Some(*value),
            _ => None,
        })
        .unwrap_or(-1);
    assert_eq!(
        count, 0,
        "Collector retains compatible evidence lifecycle but must not duplicate Core topology projection"
    );
}

#[tokio::test]
async fn withdrawal_revokes_only_the_withdrawn_evidence_support() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");

    {
        let store =
            GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");
        store
            .write(interface_update(
                "172.20.0.21:6030",
                "ceos1",
                "Ethernet1",
                99,
            ))
            .await
            .unwrap();
        store
            .write(interface_update(
                "172.20.0.22:6030",
                "leaf1",
                "Ethernet1",
                99,
            ))
            .await
            .unwrap();
        store
            .write(ceos_projection_update("1", 1_700_000_000_000_000_000))
            .await
            .unwrap();
        store
            .write(ceos_projection_update("2", 1_700_000_001_000_000_000))
            .await
            .unwrap();
    }

    let rows = projection_support_rows(&db_path);
    assert_eq!(rows.len(), 1, "one derived adjacency with shared support");
    assert_eq!(rows[0].0, "172.20.0.22:Ethernet1");
    assert_eq!(rows[0].1, "lldp_operational");
    let support: Vec<String> = serde_json::from_str(&rows[0].2).expect("support json");
    assert_eq!(
        support.len(),
        2,
        "two independent observations support the adjacency"
    );

    {
        let store =
            GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("reopen graph");
        store
            .write(ceos_exact_delete("1", 1_700_000_010_000_000_000))
            .await
            .unwrap();
    }
    let rows = projection_support_rows(&db_path);
    assert_eq!(
        rows.len(),
        1,
        "remaining independent support preserves the adjacency"
    );
    let support: Vec<String> = serde_json::from_str(&rows[0].2).expect("support json");
    assert_eq!(support.len(), 1);

    {
        let store =
            GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("reopen graph");
        store
            .write(ceos_exact_delete("2", 1_700_000_011_000_000_000))
            .await
            .unwrap();
    }
    let rows = projection_support_rows(&db_path);
    assert!(
        rows.is_empty(),
        "final applicable support removes the LLDP-owned projection"
    );
}

#[tokio::test]
async fn newer_observation_rejects_delayed_older_observation_and_exact_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");

    store
        .write(ceos_update(false, 1_700_000_010_000_000_000, "aa:01"))
        .await
        .unwrap();
    store
        .write(ceos_update(false, 1_700_000_000_000_000_000, "aa:01"))
        .await
        .unwrap();
    store
        .write(ceos_update(false, 1_700_000_010_000_000_000, "aa:01"))
        .await
        .unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.21")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, "active");
    assert_eq!(rows[0].chassis_id, "aa:01");
    assert_eq!(rows[0].ordering_state, "ordered");
}

#[tokio::test]
async fn restart_replay_does_not_duplicate_or_resurrect_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph.db");
    let store = GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open graph");
    let first = ceos_projection_update("restart-neighbor", 1_700_000_000_000_000_000);
    let delete = ceos_exact_delete("restart-neighbor", 1_700_000_010_000_000_000);
    let mut restarted_replay = first.clone();
    restarted_replay.session_id = "session-2".into();
    restarted_replay.plan_id = "plan-2".into();

    store.write(first).await.unwrap();
    store.write(delete).await.unwrap();
    store.write(restarted_replay.clone()).await.unwrap();
    store.write(restarted_replay).await.unwrap();

    let rows = store
        .lldp_neighbors_for_device("172.20.0.21")
        .await
        .expect("read lldp rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lifecycle, "withdrawn");
}

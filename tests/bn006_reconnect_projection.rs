use bonsai::graph::GraphStore;
use bonsai::routing_service::{
    ROUTING_CHECKPOINT_CONFIG_CLASS, ROUTING_CHECKPOINT_ID, RoutingServiceConfig,
    RoutingServiceState, project_routing_events_to_graph,
};
use rbmp_core::bmp::parser::DEFAULT_MAX_FRAME;
use rbmp_rib::RibManager;
use rbmp_store::{BmpJournal, RibEventJournal};

#[tokio::test]
async fn legacy_journal_and_checkpoint_rebuild_without_changing_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("bmp.jsonl");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bn006_eos_4_36/reconnect.journal.jsonl"
        ),
        &bmp,
    )
    .unwrap();
    let events = dir.path().join("events.jsonl");
    let (mut rib, _) = RibManager::new(128);
    let replay = BmpJournal::replay_into_with_state(&bmp, &mut rib).unwrap();
    let mut legacy = Vec::new();
    for event in &replay.emitted_events {
        let mut value = serde_json::to_value(event).unwrap();
        value["payload"]
            .as_object_mut()
            .unwrap()
            .remove("retired_routes");
        serde_json::to_writer(&mut legacy, &value).unwrap();
        legacy.push(b'\n');
    }
    std::fs::write(&events, &legacy).unwrap();
    let graph_path = dir.path().join("graph.db");
    let graph = GraphStore::open(graph_path.to_str().unwrap(), 128 * 1024 * 1024).unwrap();
    let old_journal = RibEventJournal::open(Some(&events)).await.unwrap();
    let old = project_routing_events_to_graph(&old_journal, &graph, 1000)
        .await
        .unwrap();
    assert_eq!(
        old.checkpoint
            .peer_summaries
            .values()
            .map(|p| p.active_prefixes.len())
            .sum::<usize>(),
        5
    );
    let mut record = graph
        .list_config_items(Some(ROUTING_CHECKPOINT_CONFIG_CLASS.into()))
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.id == ROUTING_CHECKPOINT_ID)
        .unwrap();
    let mut checkpoint: serde_json::Value = serde_json::from_str(&record.content_json).unwrap();
    checkpoint
        .as_object_mut()
        .unwrap()
        .remove("projection_version");
    record.content_json = checkpoint.to_string();
    graph.upsert_config_item(record).await.unwrap();
    drop(old_journal);

    let cfg = RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(bmp),
        event_journal_path: Some(events.clone()),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: None,
        identity_db_path: None,
        identity_scope: None,
    };
    let mut missing_raw = cfg.clone();
    missing_raw.bmp_journal_path = None;
    let incomplete = RoutingServiceState::new(missing_raw).await.unwrap();
    let response =
        bonsai::routing_service::build_events_json(&incomplete, &Default::default()).await;
    assert_eq!(response["projection_semantics_version"], 0);
    let incomplete_comparison = bonsai::routing_service::diagnostics::build_compare_snapshots_json(
        &incomplete,
        &Default::default(),
    )
    .await;
    assert_eq!(incomplete_comparison["history_coverage"], "partial");
    let (_, incomplete_history) = bonsai::routing_service::build_prefix_history_json(
        &incomplete,
        &bonsai::routing_service::PrefixHistoryQuery {
            prefix: Some("10.77.1.0/24".into()),
            speaker: None,
            vrf_rd: None,
            exporter_instance: None,
            limit: None,
        },
    )
    .await;
    assert_eq!(incomplete_history["history_coverage"], "partial");
    drop(incomplete);
    let first = RoutingServiceState::new(cfg.clone()).await.unwrap();
    let response = bonsai::routing_service::build_events_json(&first, &Default::default()).await;
    assert_eq!(response["projection_semantics_version"], 1);
    let retirement_seq = replay
        .emitted_events
        .iter()
        .find_map(|event| match &event.payload {
            rbmp_rib::event::RibEventPayload::EndOfRib { retired_routes, .. }
                if retired_routes
                    .iter()
                    .any(|route| route.route_key.path_id == Some(3)) =>
            {
                Some(event.event_key.sequence)
            }
            _ => None,
        })
        .unwrap();
    let comparison = bonsai::routing_service::diagnostics::build_compare_snapshots_json(
        &first,
        &bonsai::routing_service::diagnostics::CompareSnapshotsQuery {
            before_seq: Some(retirement_seq - 1),
            after_seq: Some(retirement_seq),
            ..Default::default()
        },
    )
    .await;
    assert!(
        comparison["removed_routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|route| route["prefix"] == "10.77.1.0/24" && route["path_id"] == 3)
    );
    let partial = project_routing_events_to_graph(&first.event_journal, &graph, 7)
        .await
        .unwrap();
    assert_eq!(partial.newly_applied_events, 7);
    assert!(partial.sequence_lag > 0);
    drop(first);
    for _ in 0..2 {
        let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
        assert_eq!(state.rib.read().await.total_routes(), 4);
        let projected = loop {
            let step = project_routing_events_to_graph(&state.event_journal, &graph, 7)
                .await
                .unwrap();
            if step.sequence_lag == 0 {
                break step;
            }
            assert!(step.newly_applied_events > 0);
        };
        assert_eq!(
            projected
                .checkpoint
                .peer_summaries
                .values()
                .map(|p| p.active_prefixes.len())
                .sum::<usize>(),
            4
        );
        assert_eq!(
            projected.checkpoint.last_applied_sequence,
            replay.emitted_events.last().unwrap().event_key.sequence
        );
        assert_eq!(std::fs::read(&events).unwrap(), legacy);
    }
}

#[tokio::test]
async fn remote_projector_rejects_service_without_retirement_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let graph_path = dir.path().join("old-service-graph.db");
    let graph = GraphStore::open(graph_path.to_str().unwrap(), 128 * 1024 * 1024).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/internal/routing/v1/events",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({"durable_sequence":0,"events":[]}))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    graph
        .upsert_config_item(bonsai::graph::ConfigItemRecord {
            id: "test-routing-url".into(),
            config_class: "app_config".into(),
            vendor: String::new(),
            name: "routing_service_url".into(),
            version: "1".into(),
            content_json: format!("http://{address}"),
            enabled: true,
            created_by: "test".into(),
        })
        .await
        .unwrap();
    let error = bonsai::routing_service::project_remote_routing_events_to_graph(&graph, 100)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("compatible snapshot retirement semantics")
    );
    server.abort();
}

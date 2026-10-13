use bonsai::routing_service::{
    RoutingServiceConfig, RoutingServiceState, build_routes_json, sample_bmp_session_frames,
};
use rbmp_core::bgp::types::{BgpUpdate, PathAttributes, Prefix};
use rbmp_core::bmp::parser::{DEFAULT_MAX_FRAME, parse_bmp_message, parse_peer_header};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload, StatEntry};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};

fn config(dir: &std::path::Path) -> RoutingServiceConfig {
    RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.join("bmp.jsonl")),
        event_journal_path: Some(dir.join("events.jsonl")),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: None,
    }
}

fn frames() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 2), 65001, [10, 10, 1], 100)
}

fn speaker() -> IpAddr {
    "192.0.2.1".parse().unwrap()
}

#[tokio::test]
async fn accepted_raw_record_must_recover_event_history() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    let (init, _, _) = frames();
    let msg = BmpMessage {
        id: uuid::Uuid::new_v4(),
        received_at: chrono::Utc::now(),
        speaker_addr: speaker(),
        payload: parse_bmp_message(&init, speaker(), DEFAULT_MAX_FRAME).unwrap(),
    };
    state.bmp_journal.append(&msg).await.unwrap();
    drop(state);

    let recovered = RoutingServiceState::new(cfg).await.unwrap();
    assert_eq!(recovered.rib.read().await.speakers().len(), 1);
    assert!(
        recovered.event_journal.total_events().await > 0,
        "Accepted raw observation rebuilt current state but lost event history"
    );
}

#[tokio::test]
async fn recovered_truncated_event_tail_must_allow_future_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (init, _, _) = frames();
    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    state
        .ingest_bmp_bytes(speaker(), Some(1), &init)
        .await
        .unwrap();
    drop(state);

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("events.jsonl"))
        .unwrap();
    f.write_all(b"{\"torn\":").unwrap();
    drop(f);

    let resumed = RoutingServiceState::new(cfg.clone()).await.unwrap();
    resumed
        .ingest_bmp_bytes(speaker(), Some(2), &init)
        .await
        .unwrap();
    drop(resumed);

    assert!(
        RoutingServiceState::new(cfg).await.is_ok(),
        "Skipped torn tail was left on disk and merged with the next record"
    );
}

#[tokio::test]
async fn transport_gap_must_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (init, peer, route) = frames();
    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    for frame in [&init, &peer, &route] {
        state
            .ingest_bmp_bytes(speaker(), Some(1), frame)
            .await
            .unwrap();
    }
    state
        .record_transport_disconnect(speaker(), "review disconnect")
        .await;
    drop(state);

    let recovered = RoutingServiceState::new(cfg).await.unwrap();
    assert!(
        recovered
            .transport_gaps
            .read()
            .await
            .contains_key(&speaker()),
        "Monitoring gap disappeared across restart"
    );
}

#[tokio::test]
async fn large_valid_update_must_preserve_all_route_events() {
    let dir = tempfile::tempdir().unwrap();
    let state = RoutingServiceState::new(config(dir.path())).await.unwrap();
    let (init, peer, route) = frames();
    for frame in [&init, &peer] {
        state
            .ingest_bmp_bytes(speaker(), Some(1), frame)
            .await
            .unwrap();
    }
    let mut payload = parse_bmp_message(&route, speaker(), DEFAULT_MAX_FRAME).unwrap();
    if let BmpPayload::RouteMonitoring { update, .. } = &mut payload {
        update.announced = (0..5000u32)
            .map(|i| {
                Prefix::V4(
                    ipnet::Ipv4Net::new(Ipv4Addr::new(10, (i >> 8) as u8, i as u8, 0), 24).unwrap(),
                )
            })
            .collect();
        update.announced_path_ids = vec![None; 5000];
    } else {
        panic!("Expected update");
    }
    let before = state.event_journal.total_events().await;
    state
        .ingest_bmp_message(
            BmpMessage {
                id: uuid::Uuid::new_v4(),
                received_at: chrono::Utc::now(),
                speaker_addr: speaker(),
                payload,
            },
            Some(1),
        )
        .await
        .unwrap();
    let (_, routes) = build_routes_json(&state, &Default::default()).await;
    assert_eq!(routes["total_matching"], 5000);
    assert_eq!(
        state.event_journal.total_events().await - before,
        5000,
        "Broadcast lag silently dropped emitted history events"
    );
}

#[tokio::test]
async fn r02_second_restart_after_raw_recovery_is_idempotent_and_preserves_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (init, peer, route) = frames();

    // Ingest init + peer normally at epoch 7, then simulate crash after writing route to bmp_journal
    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    state
        .ingest_bmp_bytes(speaker(), Some(7), &init)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker(), Some(7), &peer)
        .await
        .unwrap();
    assert_eq!(state.event_journal.total_events().await, 2);

    let route_msg = BmpMessage {
        id: uuid::Uuid::new_v4(),
        received_at: chrono::Utc::now(),
        speaker_addr: speaker(),
        payload: parse_bmp_message(&route, speaker(), DEFAULT_MAX_FRAME).unwrap(),
    };
    state
        .bmp_journal
        .append_observation(&route_msg, Some(7), Some(&route))
        .await
        .unwrap();
    drop(state);

    // First restart: recovers missing 3rd event (RouteChange) and preserves epoch 7
    let recovered_1 = RoutingServiceState::new(cfg.clone()).await.unwrap();
    assert_eq!(recovered_1.event_journal.total_events().await, 3);
    let events_1 = recovered_1.event_journal.events_after_seq(0, 10).await;
    assert_eq!(events_1.len(), 3);
    for ev in &events_1 {
        assert_eq!(ev.event_key.source.session_epoch, 7);
    }
    drop(recovered_1);

    // Second restart: idempotent (still 3 events, identical event UUIDs and sequences)
    let recovered_2 = RoutingServiceState::new(cfg).await.unwrap();
    assert_eq!(recovered_2.event_journal.total_events().await, 3);
    let events_2 = recovered_2.event_journal.events_after_seq(0, 10).await;
    assert_eq!(events_2.len(), 3);
    for (a, b) in events_1.iter().zip(events_2.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.event_key, b.event_key);
    }
}

#[tokio::test]
async fn r03_truncated_bmp_journal_tail_is_repaired_on_disk_for_future_appends() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (init, peer, route) = frames();

    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    state
        .ingest_bmp_bytes(speaker(), Some(1), &init)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker(), Some(1), &peer)
        .await
        .unwrap();
    drop(state);

    // Inject torn tail on bmp.jsonl
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("bmp.jsonl"))
        .unwrap();
    f.write_all(b"{\"id\":\"torn-bmp-tail").unwrap();
    drop(f);

    // Restart, append another frame, and restart again
    let resumed = RoutingServiceState::new(cfg.clone()).await.unwrap();
    resumed
        .ingest_bmp_bytes(speaker(), Some(1), &route)
        .await
        .unwrap();
    drop(resumed);

    let final_state = RoutingServiceState::new(cfg)
        .await
        .expect("Torn bmp.jsonl tail must be truncated before appending next record");
    assert_eq!(final_state.rib.read().await.total_routes(), 1);
    assert_eq!(final_state.event_journal.total_events().await, 3);
}

#[tokio::test]
async fn r04_epoch_fenced_disconnect_and_resync_completeness_rules() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (init, peer, route) = frames();
    let (peer_hdr, _) = parse_peer_header(&peer[6..]).unwrap();

    let state = RoutingServiceState::new(cfg.clone()).await.unwrap();
    for frame in [&init, &peer, &route] {
        state
            .ingest_bmp_bytes(speaker(), Some(2), frame)
            .await
            .unwrap();
    }
    // Send EndOfRib in epoch 2 so view becomes complete_for_observed_view
    let eor_msg = BmpMessage {
        id: uuid::Uuid::new_v4(),
        received_at: chrono::Utc::now(),
        speaker_addr: speaker(),
        payload: BmpPayload::RouteMonitoring {
            peer_header: peer_hdr.clone(),
            update: BgpUpdate {
                withdrawn: vec![],
                withdrawn_path_ids: vec![],
                attributes: PathAttributes::default(),
                announced: vec![],
                announced_path_ids: vec![],
            },
        },
    };
    state
        .ingest_bmp_message(eor_msg.clone(), Some(2))
        .await
        .unwrap();
    assert_eq!(
        state.overall_completeness().await,
        "complete_for_observed_view"
    );

    // 1. Stale disconnect from epoch 1 (< active epoch 2) must be fenced and ignored
    state
        .record_transport_disconnect_for_epoch(speaker(), Some(1), "stale_epoch_1_eof")
        .await;
    assert!(!state.transport_gaps.read().await.contains_key(&speaker()));
    assert_eq!(
        state.overall_completeness().await,
        "complete_for_observed_view"
    );

    // 2. Active disconnect for epoch 2 marks speaker as gap
    state
        .record_transport_disconnect_for_epoch(speaker(), Some(2), "active_epoch_2_eof")
        .await;
    assert_eq!(state.overall_completeness().await, "gap");

    // 3. Stray StatsReport in epoch 2 does NOT clear the transport gap
    let stats_msg = BmpMessage {
        id: uuid::Uuid::new_v4(),
        received_at: chrono::Utc::now(),
        speaker_addr: speaker(),
        payload: BmpPayload::StatsReport {
            peer_header: peer_hdr,
            stats: vec![StatEntry {
                stat_type: 0,
                name: "rejected".into(),
                value: 1,
                afi_safi: None,
            }],
        },
    };
    state.ingest_bmp_message(stats_msg, Some(2)).await.unwrap();
    assert_eq!(
        state.overall_completeness().await,
        "gap",
        "Stray non-synchronization message in same epoch must not clear transport gap"
    );

    // 4. Reconnecting with Initiation in epoch 3 clears the transport gap and enters "syncing" (not complete until EoR)
    state
        .ingest_bmp_bytes(speaker(), Some(3), &init)
        .await
        .unwrap();
    assert!(!state.transport_gaps.read().await.contains_key(&speaker()));
    assert_eq!(
        state.overall_completeness().await,
        "syncing",
        "New session epoch must remain syncing until EndOfRib is received"
    );

    // 5. Receiving EndOfRib in epoch 3 restores complete_for_observed_view
    state.ingest_bmp_message(eor_msg, Some(3)).await.unwrap();
    assert_eq!(
        state.overall_completeness().await,
        "complete_for_observed_view"
    );
}

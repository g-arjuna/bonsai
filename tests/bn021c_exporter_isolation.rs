//! BN-021c regression: two independent BMP exporters behind one NAT
//! address must never merge route, peer, session, transport-gap, journal
//! or replay state.
//!
//! All data is synthetic; no live devices, no credential material.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bonsai::routing_service::{
    build_internal_routing_router, resolved_exporter_instance, run_bmp_listener,
    RoutingServiceConfig, RoutingServiceState, TransportDisconnectOutcome,
};
use bonsai::sqlite_store::{SqliteStore, CONFIG_DB_FILE_NAME};
use bonsai::telemetry_identity::{
    BindingProvenance, BindingStatus, CanonicalDevice, TrustedBinding,
};
use chrono::Utc;
use rbmp_core::bgp::types::{BgpUpdate, PathAttributes, Prefix};
use rbmp_core::bmp::parser::DEFAULT_MAX_FRAME;
use rbmp_core::bmp::types::{
    BgpOpenInfo, BmpMessage, BmpPayload, PeerDownReason, PeerFlags, PeerHeader, PeerKey, PeerType,
    PeerUpMessage, RibType, SpeakerKey,
};
use rbmp_rib::RibManager;
use rbmp_store::BmpJournal;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

/// Shared NAT source address observed by the BMP listener for BOTH
/// independent exporters.
fn nat_addr() -> IpAddr {
    "198.51.100.1".parse().unwrap()
}

/// Identical BGP peer identity advertised by both exporters.
fn peer_addr() -> IpAddr {
    "192.0.2.1".parse().unwrap()
}

/// Identical non-zero VRF RD advertised by both exporters.
fn peer_rd() -> [u8; 8] {
    [0, 0, 0, 1, 0, 0, 0, 100]
}

fn prefix() -> Prefix {
    Prefix::V4("203.0.113.0/24".parse().unwrap())
}

fn peer_hdr() -> PeerHeader {
    PeerHeader {
        peer_type: PeerType::RdInstance,
        peer_flags: PeerFlags(0),
        peer_distinguisher: peer_rd(),
        peer_address: peer_addr(),
        peer_as: 65002,
        peer_bgp_id: Ipv4Addr::new(192, 0, 2, 1),
        timestamp: Utc::now(),
        rib_type: RibType::AdjRibInPostPolicy,
    }
}

fn bmp_msg(scope: &str, payload: BmpPayload) -> BmpMessage {
    BmpMessage {
        id: Uuid::new_v4(),
        received_at: Utc::now(),
        speaker_addr: nat_addr(),
        exporter_instance: scope.to_string(),
        exporter_id: None,
        device_id: None,
        payload,
    }
}

fn initiation_msg(scope: &str, sys_name: &str) -> BmpMessage {
    bmp_msg(
        scope,
        BmpPayload::Initiation {
            sys_name: Some(sys_name.to_string()),
            sys_descr: None,
            labels: vec![],
        },
    )
}

fn peer_up_msg(scope: &str) -> BmpMessage {
    let speaker = nat_addr();
    bmp_msg(
        scope,
        BmpPayload::PeerUp(PeerUpMessage {
            peer_header: peer_hdr(),
            local_addr: speaker,
            local_port: 179,
            remote_port: 50000,
            sent_open: BgpOpenInfo {
                version: 4,
                asn: 65000,
                hold_time: 90,
                bgp_id: Ipv4Addr::new(10, 0, 0, 1),
                capabilities: vec![],
            },
            recv_open: BgpOpenInfo {
                version: 4,
                asn: 65002,
                hold_time: 90,
                bgp_id: Ipv4Addr::new(192, 0, 2, 1),
                capabilities: vec![],
            },
        }),
    )
}

fn announce_msg(scope: &str, local_pref: u32) -> BmpMessage {
    let mut attrs = PathAttributes::default();
    attrs.local_pref = Some(local_pref);
    bmp_msg(
        scope,
        BmpPayload::RouteMonitoring {
            peer_header: peer_hdr(),
            update: BgpUpdate {
                withdrawn: vec![],
                withdrawn_path_ids: vec![],
                attributes: attrs,
                announced: vec![prefix()],
                announced_path_ids: vec![None],
            },
        },
    )
}

fn withdraw_msg(scope: &str) -> BmpMessage {
    bmp_msg(
        scope,
        BmpPayload::RouteMonitoring {
            peer_header: peer_hdr(),
            update: BgpUpdate {
                withdrawn: vec![prefix()],
                withdrawn_path_ids: vec![None],
                attributes: PathAttributes::default(),
                announced: vec![],
                announced_path_ids: vec![],
            },
        },
    )
}

fn peer_down_msg(scope: &str) -> BmpMessage {
    bmp_msg(
        scope,
        BmpPayload::PeerDown {
            peer_header: peer_hdr(),
            reason: PeerDownReason::RemoteSystemClosed2,
        },
    )
}

fn route_count(rib: &rbmp_rib::RibManager, key: SpeakerKey) -> usize {
    rib.ribs_for_speaker(key)
        .map(|(_, table)| table.iter_rib(RibType::AdjRibInPostPolicy).count())
        .sum()
}

async fn test_state(dir: &TempDir, identity_db: Option<PathBuf>) -> Arc<RoutingServiceState> {
    RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.path().join("bmp.jsonl")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn021c-test-token".to_string()),
        identity_db_path: identity_db,
        identity_scope: None,
    })
    .await
    .expect("routing state")
}

// ── A. Live RIB/session/peer isolation behind one NAT address ────────────────

#[tokio::test]
async fn nat_peers_never_merge_live_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir, None).await;

    // Two independent exporters, same transport IP, same peer IP, same
    // VRF RD, same prefix — only the logical-exporter scope differs.
    let scope_a = "conn:exporter-a";
    let scope_b = "conn:exporter-b";
    let key_a = SpeakerKey::new(nat_addr(), scope_a);
    let key_b = SpeakerKey::new(nat_addr(), scope_b);
    assert_ne!(key_a, key_b);

    for (scope, epoch, lp) in [(scope_a, 1u64, 100u32), (scope_b, 2u64, 200u32)] {
        state
            .ingest_bmp_message(initiation_msg(scope, "router"), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(peer_up_msg(scope), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(announce_msg(scope, lp), Some(epoch))
            .await
            .unwrap();
    }

    {
        let rib = state.rib.read().await;
        assert_eq!(rib.speakers().len(), 2, "one speaker session per exporter");
        assert!(rib.speaker(key_a).is_some());
        assert!(rib.speaker(key_b).is_some());
        // ObservationSource carries the scope, not just the transport IP.
        assert_eq!(
            rib.speaker(key_a).unwrap().source.exporter_instance,
            scope_a
        );
        assert_eq!(
            rib.speaker(key_b).unwrap().source.exporter_instance,
            scope_b
        );

        // Identical peer header still yields distinct PeerKeys per exporter.
        let peers_a: Vec<PeerKey> = rib.ribs_for_speaker(key_a).map(|(k, _)| *k).collect();
        let peers_b: Vec<PeerKey> = rib.ribs_for_speaker(key_b).map(|(k, _)| *k).collect();
        assert_eq!(peers_a.len(), 1);
        assert_eq!(peers_b.len(), 1);
        assert_ne!(peers_a[0], peers_b[0]);
        assert_eq!(peers_a[0].peer_address, peer_addr());
        assert_eq!(peers_a[0].peer_distinguisher, peers_b[0].peer_distinguisher);

        assert_eq!(route_count(&rib, key_a), 1);
        assert_eq!(route_count(&rib, key_b), 1);
    }

    // Withdrawal on exporter A leaves exporter B's identical route intact.
    state
        .ingest_bmp_message(withdraw_msg(scope_a), Some(1))
        .await
        .unwrap();
    {
        let rib = state.rib.read().await;
        assert_eq!(route_count(&rib, key_a), 0, "A withdrawal must apply");
        assert_eq!(route_count(&rib, key_b), 1, "B must be untouched");
    }

    // PeerDown on A cannot clear B's peer/RIB state.
    state
        .ingest_bmp_message(peer_down_msg(scope_a), Some(1))
        .await
        .unwrap();
    {
        let rib = state.rib.read().await;
        assert_eq!(route_count(&rib, key_b), 1, "B survives A PeerDown");
        assert!(rib.speaker(key_b).is_some());
    }

    // Transport disconnect fenced to A's SpeakerKey only.
    state
        .record_transport_disconnect_for_key(key_a, scope_a, Some(1), "test eof")
        .await;
    {
        let gaps = state.transport_gaps.read().await;
        assert!(gaps.contains_key(&key_a));
        assert!(!gaps.contains_key(&key_b), "B must have no gap");
        drop(gaps);
        let rib = state.rib.read().await;
        assert!(rib.speaker(key_b).is_some());
    }
}

// ── B. Trusted-binding resolution scopes state per logical exporter ──────────

fn canonical_device(address: &str) -> CanonicalDevice {
    CanonicalDevice {
        device_id: String::new(),
        address: address.to_string(),
        site: Some("dc1".to_string()),
        hostname: None,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: String::new(),
        updated_by: String::new(),
    }
}

fn bmp_binding(exporter_id: &str, device_id: &str, sys_name: &str) -> TrustedBinding {
    TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "default".to_string(),
        protocol: "bmp".to_string(),
        exporter_id: exporter_id.to_string(),
        device_id: device_id.to_string(),
        match_transport: Some(nat_addr().to_string()),
        match_reported_identity: Some(sys_name.to_string()),
        match_exporter_claim: Some(format!("bmp:{sys_name}")),
        match_observation_domain: None,
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: "test".to_string(),
        updated_by: "test".to_string(),
    }
}

#[tokio::test]
async fn trusted_bindings_resolve_exporters_behind_nat() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    let (dev_a, dev_b) = {
        let store = SqliteStore::open(dir.path()).unwrap();
        let a = store
            .upsert_canonical_device(&canonical_device("10.0.0.11"), "test")
            .unwrap();
        let b = store
            .upsert_canonical_device(&canonical_device("10.0.0.12"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&bmp_binding("exp-rtr-a", &a.device_id, "frr-a"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&bmp_binding("exp-rtr-b", &b.device_id, "frr-b"), "test")
            .unwrap();
        (a.device_id, b.device_id)
    };
    assert!(db_path.exists());

    let state = test_state(&dir, Some(db_path)).await;

    // Same NAT IP, different sysName → different canonical exporters.
    let ra = state.resolve_bmp_exporter(nat_addr(), "conn-1", Some("frr-a"), None);
    assert_eq!(ra.status, "resolved");
    assert_eq!(ra.scope.exporter_instance, "exp:exp-rtr-a");
    assert_eq!(ra.scope.device_id.as_deref(), Some(dev_a.as_str()));

    let rb = state.resolve_bmp_exporter(nat_addr(), "conn-2", Some("frr-b"), None);
    assert_eq!(rb.status, "resolved");
    assert_eq!(rb.scope.exporter_instance, "exp:exp-rtr-b");
    assert_eq!(rb.scope.device_id.as_deref(), Some(dev_b.as_str()));

    assert_ne!(
        ra.scope.speaker_key(nat_addr()),
        rb.scope.speaker_key(nat_addr())
    );

    // Positive association: a second connection reporting the SAME
    // sysName resolves to the SAME exporter scope (FRR multi-instance
    // behavior preserved) — they share one speaker session.
    let ra2 = state.resolve_bmp_exporter(nat_addr(), "conn-3", Some("frr-a"), None);
    assert_eq!(ra2.scope.exporter_instance, ra.scope.exporter_instance);
    assert_eq!(
        ra2.scope.speaker_key(nat_addr()),
        ra.scope.speaker_key(nat_addr())
    );

    // Unbound sysName from the same NAT IP fails closed into an
    // isolated connection scope — never merged with a resolved exporter.
    let rc = state.resolve_bmp_exporter(nat_addr(), "conn-4", Some("frr-c"), None);
    assert_ne!(rc.status, "resolved");
    assert!(rc.scope.device_id.is_none());
    assert!(rc.scope.exporter_instance.starts_with("conn:"));
    assert_ne!(
        rc.scope.speaker_key(nat_addr()),
        ra.scope.speaker_key(nat_addr())
    );

    // Resolved exporters ingest under their stable scope; identical
    // peer/prefix state stays partitioned per canonical exporter.
    for (scope, epoch, dev) in [
        (&ra.scope, 11u64, Some(dev_a.as_str())),
        (&rb.scope, 12u64, Some(dev_b.as_str())),
    ] {
        let mut m = peer_up_msg(&scope.exporter_instance);
        m.device_id = dev.map(str::to_string);
        state
            .ingest_bmp_message(initiation_msg(&scope.exporter_instance, "frr"), Some(epoch))
            .await
            .unwrap();
        state.ingest_bmp_message(m, Some(epoch)).await.unwrap();
        state
            .ingest_bmp_message(announce_msg(&scope.exporter_instance, 100), Some(epoch))
            .await
            .unwrap();
    }

    let key_a = ra.scope.speaker_key(nat_addr());
    let key_b = rb.scope.speaker_key(nat_addr());
    let rib = state.rib.read().await;
    assert_eq!(rib.speakers().len(), 2);
    assert_eq!(
        rib.speaker(key_a).unwrap().source.device_id.as_deref(),
        Some(dev_a.as_str())
    );
    assert_eq!(
        rib.speaker(key_b).unwrap().source.device_id.as_deref(),
        Some(dev_b.as_str())
    );
    assert_eq!(route_count(&rib, key_a), 1);
    assert_eq!(route_count(&rib, key_b), 1);
}

// ── C. Durable journal + replay preserve exporter partitioning ───────────────

#[tokio::test]
async fn journal_replay_preserves_exporter_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let state = test_state(&dir, None).await;

    // exp:* scopes carry trusted-binding identity and keep last-known
    // state across replay; conn:* retirement is covered separately.
    for (scope, epoch, lp) in [
        ("exp:exp-rtr-a", 21u64, 100u32),
        ("exp:exp-rtr-b", 22u64, 300u32),
    ] {
        state
            .ingest_bmp_message(initiation_msg(scope, "router"), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(peer_up_msg(scope), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(announce_msg(scope, lp), Some(epoch))
            .await
            .unwrap();
    }
    drop(state);

    // Replay reconstructs independently keyed speaker/peer/RIB state.
    let mut rib = RibManager::new(128).0;
    let report = BmpJournal::replay_into(&bmp_path, &mut rib).unwrap();
    assert!(report.applied_messages > 0);

    let key_a = SpeakerKey::new(nat_addr(), "exp:exp-rtr-a");
    let key_b = SpeakerKey::new(nat_addr(), "exp:exp-rtr-b");
    assert_eq!(rib.speakers().len(), 2);
    let peers_a: Vec<PeerKey> = rib.ribs_for_speaker(key_a).map(|(k, _)| *k).collect();
    let peers_b: Vec<PeerKey> = rib.ribs_for_speaker(key_b).map(|(k, _)| *k).collect();
    assert_eq!(peers_a.len(), 1);
    assert_eq!(peers_b.len(), 1);
    assert_ne!(peers_a[0], peers_b[0]);
    assert_eq!(route_count(&rib, key_a), 1);
    assert_eq!(route_count(&rib, key_b), 1);

    // Duplicate replay must not duplicate route transitions.
    let before = rib.total_routes();
    let mut rib2 = RibManager::new(128).0;
    BmpJournal::replay_into(&bmp_path, &mut rib2).unwrap();
    assert_eq!(rib2.total_routes(), before);
    assert_eq!(rib2.speakers().len(), 2);

    // A legacy journal record without exporter identity lands in the
    // legacy speaker scope (instance 0) — separate from both conn: scopes.
    let legacy = serde_json::json!({
        "id": Uuid::new_v4(),
        "received_at": Utc::now(),
        "speaker_addr": nat_addr(),
        "payload": {
            "type": "initiation",
            "sys_name": "legacy-router",
            "sys_descr": null,
            "labels": []
        }
    });
    let legacy_path = dir.path().join("legacy.jsonl");
    std::fs::write(&legacy_path, format!("{legacy}\n")).unwrap();
    let mut rib3 = RibManager::new(128).0;
    BmpJournal::replay_into(&legacy_path, &mut rib3).unwrap();
    let legacy_key = SpeakerKey::new(nat_addr(), "");
    assert!(rib3.speaker(legacy_key).is_some());
    assert_ne!(legacy_key, key_a);
    assert_ne!(legacy_key, key_b);

    // conn:* scopes with no disconnect record replay as live state —
    // ungraceful restarts cannot distinguish dead connections (documented
    // BN-021c limitation). Explicit disconnects DO retire them (test F).
    let conn_path = dir.path().join("conn.jsonl");
    {
        let journal = BmpJournal::open(Some(&conn_path)).await.unwrap();
        for (scope, epoch, lp) in [
            ("conn:dead-1", 41u64, 100u32),
            ("conn:dead-2", 42u64, 200u32),
        ] {
            for m in [
                initiation_msg(scope, "router"),
                peer_up_msg(scope),
                announce_msg(scope, lp),
            ] {
                journal
                    .append_observation(&m, Some(epoch), None)
                    .await
                    .unwrap();
            }
        }
    }
    let mut rib4 = RibManager::new(128).0;
    BmpJournal::replay_into(&conn_path, &mut rib4).unwrap();
    assert_eq!(rib4.speakers().len(), 2);
    assert_eq!(rib4.total_routes(), 2);
}

// ── D. Cross-scope canonical-id collision regression (BN-021c hardening) ────
//
// BN-021b canonical exporter ids are unique per (scope, protocol), NOT
// globally: two sites may both name an exporter "router-01". The BMP
// speaker scope must therefore encode the identity namespace, not merely
// the short canonical id, or identical scopes would merge authoritative
// state behind one NAT address.

#[tokio::test]
async fn cross_scope_duplicate_exporter_ids_cannot_merge() {
    // The scope string itself must carry the namespace.
    assert_eq!(
        resolved_exporter_instance("default", "router-01"),
        "exp:router-01"
    );
    assert_eq!(
        resolved_exporter_instance("site-a", "router-01"),
        "exp:site-a:router-01"
    );
    assert_eq!(
        resolved_exporter_instance("site-b", "router-01"),
        "exp:site-b:router-01"
    );
    assert_ne!(
        resolved_exporter_instance("site-a", "router-01"),
        resolved_exporter_instance("site-b", "router-01")
    );
    assert_ne!(
        resolved_exporter_instance("site-a", "router-01"),
        resolved_exporter_instance("default", "router-01")
    );
    // Distinct namespaces hash to distinct speaker instances.
    assert_ne!(
        SpeakerKey::new(nat_addr(), "exp:site-a:router-01"),
        SpeakerKey::new(nat_addr(), "exp:site-b:router-01")
    );

    // End-to-end: site-scoped bindings reusing "router-01" for different
    // devices. BMP observations resolve only under the default identity
    // scope today, so both connections fail closed into isolated conn:*
    // scopes — never merged, never assigned a wrong device id.
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    {
        let store = SqliteStore::open(dir.path()).unwrap();
        let mut dev_a = canonical_device("10.1.0.11");
        dev_a.site = Some("site-a".to_string());
        let mut dev_b = canonical_device("10.2.0.11");
        dev_b.site = Some("site-b".to_string());
        let a = store.upsert_canonical_device(&dev_a, "test").unwrap();
        let b = store.upsert_canonical_device(&dev_b, "test").unwrap();

        let mut ba = bmp_binding("router-01", &a.device_id, "site-a-rtr");
        ba.scope = "site-a".to_string();
        let mut bb = bmp_binding("router-01", &b.device_id, "site-b-rtr");
        bb.scope = "site-b".to_string();
        store.upsert_exporter_binding(&ba, "test").unwrap();
        store.upsert_exporter_binding(&bb, "test").unwrap();
    }

    let state = test_state(&dir, Some(db_path)).await;
    let ra = state.resolve_bmp_exporter(nat_addr(), "conn-a", Some("site-a-rtr"), None);
    let rb = state.resolve_bmp_exporter(nat_addr(), "conn-b", Some("site-b-rtr"), None);

    for r in [&ra, &rb] {
        assert_ne!(
            r.status, "resolved",
            "site-scoped claims must not resolve via default scope"
        );
        assert!(r.scope.device_id.is_none());
        assert!(r.scope.exporter_instance.starts_with("conn:"));
    }
    assert_ne!(ra.scope.exporter_instance, rb.scope.exporter_instance);
    assert_ne!(
        ra.scope.speaker_key(nat_addr()),
        rb.scope.speaker_key(nat_addr())
    );
}

// ── E. Stale session epochs are fenced for every payload kind ────────────────

#[test]
fn stale_epoch_messages_cannot_mutate_newer_session() {
    let (mut mgr, _rx) = RibManager::new(128);
    let exporter: IpAddr = nat_addr();
    let key = SpeakerKey::new(exporter, "");

    let old_epoch = mgr.open_speaker_session(exporter, "", None, None, Utc::now());
    mgr.process_for_epoch(peer_up_msg(""), Some(old_epoch));
    mgr.process_for_epoch(announce_msg("", 100), Some(old_epoch));
    assert_eq!(route_count(&mgr, key), 1);

    // Reconnect allocates a newer epoch; new session re-establishes state.
    let new_epoch = mgr.open_speaker_session(exporter, "", None, None, Utc::now());
    assert!(new_epoch > old_epoch);
    mgr.process_for_epoch(peer_up_msg(""), Some(new_epoch));
    mgr.process_for_epoch(announce_msg("", 300), Some(new_epoch));

    // A delayed PeerDown from the OLD epoch must not clear the new session.
    mgr.process_for_epoch(peer_down_msg(""), Some(old_epoch));
    assert_eq!(
        route_count(&mgr, key),
        1,
        "stale-epoch PeerDown must be fenced"
    );

    // A delayed withdrawal from the OLD epoch must not remove the route.
    mgr.process_for_epoch(withdraw_msg(""), Some(old_epoch));
    assert_eq!(
        route_count(&mgr, key),
        1,
        "stale-epoch withdrawal must be fenced"
    );

    // Current-epoch messages still apply normally.
    mgr.process_for_epoch(withdraw_msg(""), Some(new_epoch));
    assert_eq!(route_count(&mgr, key), 0);
}

// ── F. Live vs replay equivalence for connection-scoped retirement ────────────

#[tokio::test]
async fn replay_retires_connection_scoped_speakers_like_live() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let state = test_state(&dir, None).await;

    // conn:a connects, advertises, then disconnects -> live path retires it.
    let scope_a = "conn:exporter-a";
    let scope_b = "conn:exporter-b";
    let scope_e = "exp:exp-rtr-a";
    let key_a = SpeakerKey::new(nat_addr(), scope_a);
    let key_b = SpeakerKey::new(nat_addr(), scope_b);
    let key_e = SpeakerKey::new(nat_addr(), scope_e);

    for (scope, epoch, lp) in [
        (scope_a, 31u64, 100u32),
        (scope_b, 32u64, 200u32),
        (scope_e, 33u64, 300u32),
    ] {
        state
            .ingest_bmp_message(initiation_msg(scope, "router"), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(peer_up_msg(scope), Some(epoch))
            .await
            .unwrap();
        state
            .ingest_bmp_message(announce_msg(scope, lp), Some(epoch))
            .await
            .unwrap();
    }

    // Simulate the listener teardown path for conn:a and exp:e.
    assert_eq!(
        state
            .record_transport_disconnect_for_key(key_a, scope_a, Some(31), "tcp eof")
            .await,
        TransportDisconnectOutcome::Recorded
    );
    state.retire_connection_scoped_speaker(key_a).await;
    assert_eq!(
        state
            .record_transport_disconnect_for_key(key_e, scope_e, Some(33), "tcp eof")
            .await,
        TransportDisconnectOutcome::Recorded
    );
    // exp:* speakers are NEVER retired — last-known routes persist under gap.

    // Live snapshot: conn:a retired, conn:b and exp:e alive.
    {
        let rib = state.rib.read().await;
        assert!(rib.speaker(key_a).is_none(), "live conn:a must be retired");
        assert!(rib.speaker(key_b).is_some());
        assert!(rib.speaker(key_e).is_some());
        assert_eq!(route_count(&rib, key_b), 1);
        assert_eq!(route_count(&rib, key_e), 1);
    }
    let live_gaps: Vec<String> = {
        let gaps = state.transport_gaps.read().await;
        let mut v: Vec<String> = gaps
            .keys()
            .map(|k| format!("{}|{}", k.speaker_addr, k.speaker_instance))
            .collect();
        v.sort();
        v
    };
    drop(state);

    // Replay into a fresh RIB must reconstruct the SAME effective state.
    // conn:a was retired by its disconnect record; conn:b sent none, so
    // it replays as live state — conn ids cannot be reused, which makes
    // this a documented BN-021c limitation for ungraceful restarts.
    let mut rib = RibManager::new(128).0;
    let outcome = BmpJournal::replay_into_with_state(&bmp_path, &mut rib).unwrap();

    assert!(
        rib.speaker(key_a).is_none(),
        "replay must not resurrect retired conn:a"
    );
    assert!(rib.speaker(key_b).is_some());
    assert!(
        rib.speaker(key_e).is_some(),
        "exp:* keeps last-known state under gap"
    );
    assert_eq!(route_count(&rib, key_b), 1);
    assert_eq!(route_count(&rib, key_e), 1);

    let mut replay_gaps: Vec<String> = outcome
        .transport_gaps
        .keys()
        .map(|k| format!("{}|{}", k.speaker_addr, k.speaker_instance))
        .collect();
    replay_gaps.sort();
    assert_eq!(
        live_gaps, replay_gaps,
        "live and replay gap sets must match"
    );

    // Decode contexts for the retired conn:a peer must not survive replay.
    let a_instance = rbmp_core::bmp::types::exporter_instance_id(scope_a);
    assert!(outcome
        .decode_contexts
        .keys()
        .all(|k| !(k.speaker_addr == nat_addr() && k.speaker_instance == a_instance)));
}

// ── G. Real-TCP NAT acceptance: two exporters, one visible source IP ─────────
//
// Both BMP clients connect from 127.0.0.1 — the routing service observes
// the same transport address for both, exactly as a NAT would present.
// This exercises the real listener, identity resolution, HTTP API,
// lifecycle and journal replay — not hand-built RoutingServiceState.

fn bmp_tlv(t: u16, v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&t.to_be_bytes());
    out.extend_from_slice(&(v.len() as u16).to_be_bytes());
    out.extend_from_slice(v);
    out
}

fn bmp_frame(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + body.len());
    out.push(3); // BMP version 3
    out.extend_from_slice(&((6 + body.len()) as u32).to_be_bytes());
    out.push(msg_type);
    out.extend_from_slice(body);
    out
}

fn initiation_frame(sys_name: &str) -> Vec<u8> {
    let mut body = bmp_tlv(1, b"FRR 10.0");
    body.extend(bmp_tlv(2, sys_name.as_bytes()));
    bmp_frame(4, &body)
}

fn peer_header_wire(peer_ip: Ipv4Addr, peer_as: u32) -> Vec<u8> {
    // GlobalInstance peer header, IPv4 peer address (16-octet field).
    let mut h = Vec::with_capacity(42);
    h.push(0); // PeerType::GlobalInstance
    h.push(0); // flags: IPv4, 4-byte ASN
    h.extend_from_slice(&[0u8; 8]); // RD
    h.extend_from_slice(&[0u8; 12]); // peer address padding
    h.extend_from_slice(&peer_ip.octets());
    h.extend_from_slice(&peer_as.to_be_bytes());
    h.extend_from_slice(&peer_ip.octets()); // BGP identifier
    h.extend_from_slice(&1_700_000_000u32.to_be_bytes()); // timestamp
    h.extend_from_slice(&0u32.to_be_bytes());
    h
}

fn peer_down_frame(peer_ip: Ipv4Addr, peer_as: u32) -> Vec<u8> {
    let mut body = peer_header_wire(peer_ip, peer_as);
    body.push(4); // RemoteSystemClosed2
    bmp_frame(2, &body) // BmpMsgType::PeerDown = 2
}

async fn write_frame(stream: &mut TcpStream, frame: &[u8]) {
    stream.write_all(frame).await.unwrap();
    stream.flush().await.unwrap();
}

async fn get_json(client: &reqwest::Client, url: &str) -> serde_json::Value {
    let resp = client
        .get(url)
        .header("x-bonsai-routing-token", "bn021c-test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET {url}");
    resp.json().await.unwrap()
}

async fn wait_for<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn live_tcp_nat_two_exporters_stay_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let events_path = dir.path().join("events.jsonl");
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);

    // Trusted bindings: two canonical devices, distinct sysNames, both
    // observed from the same transport address (the NAT source IP).
    // bmp_binding() pins the synthetic NAT address; here the observed
    // transport is loopback, so build bindings explicitly.
    let nat_transport = "127.0.0.1".to_string();
    let loop_binding = |sys_name: &str, exporter_id: &str, device_id: &str| TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "default".to_string(),
        protocol: "bmp".to_string(),
        exporter_id: exporter_id.to_string(),
        device_id: device_id.to_string(),
        match_transport: Some(nat_transport.clone()),
        match_reported_identity: Some(sys_name.to_string()),
        match_exporter_claim: Some(format!("bmp:{sys_name}")),
        match_observation_domain: None,
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: "test".to_string(),
        updated_by: "test".to_string(),
    };
    let (dev_a, dev_b) = {
        let store = SqliteStore::open(dir.path()).unwrap();
        let a = store
            .upsert_canonical_device(&canonical_device("10.9.0.11"), "test")
            .unwrap();
        let b = store
            .upsert_canonical_device(&canonical_device("10.9.0.12"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&loop_binding("frr-a", "exp-rtr-a", &a.device_id), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&loop_binding("frr-b", "exp-rtr-b", &b.device_id), "test")
            .unwrap();
        (a.device_id, b.device_id)
    };

    let state = RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(bmp_path.clone()),
        event_journal_path: Some(events_path),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: Some("bn021c-test-token".to_string()),
        identity_db_path: Some(db_path),
        identity_scope: None,
    })
    .await
    .unwrap();

    // Real BMP TCP listener + real HTTP API.
    let bmp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bmp_addr: SocketAddr = bmp_listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let listener_state = Arc::clone(&state);
    tokio::spawn(async move { run_bmp_listener(bmp_listener, listener_state, shutdown_rx).await });

    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr: SocketAddr = http_listener.local_addr().unwrap();
    let http_state = Arc::clone(&state);
    tokio::spawn(async move {
        axum::serve(http_listener, build_internal_routing_router(http_state))
            .await
            .unwrap();
    });

    let base = format!("http://{http_addr}/internal/routing/v1");
    let client = reqwest::Client::new();

    // Two trusted exporters + one unknown exporter, all from 127.0.0.1.
    let peer_ip = Ipv4Addr::new(192, 0, 2, 1);
    let peer_as = 65002u32;
    let pfx_a = [203, 0, 113];
    let pfx_b = [198, 18, 0];

    let mut conn_a = TcpStream::connect(bmp_addr).await.unwrap();
    let mut conn_b = TcpStream::connect(bmp_addr).await.unwrap();
    let mut conn_x = TcpStream::connect(bmp_addr).await.unwrap();

    // (init sysName, peer_up+announce frames)
    let (_i1, up1, ann1) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, pfx_a, 100);
    let (_i2, up2, ann2) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, pfx_b, 200);
    let (_i3, up3, ann3) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, pfx_b, 200);

    for (stream, sys_name) in [
        (&mut conn_a, "frr-a"),
        (&mut conn_b, "frr-b"),
        (&mut conn_x, "frr-unknown"),
    ] {
        write_frame(stream, &initiation_frame(sys_name)).await;
    }
    for (stream, up, ann) in [
        (&mut conn_a, &up1, &ann1),
        (&mut conn_b, &up2, &ann2),
        (&mut conn_x, &up3, &ann3),
    ] {
        write_frame(stream, up).await;
        write_frame(stream, ann).await;
    }

    // Wait until all three speakers are visible via the real HTTP API.
    wait_for("three speakers", || async {
        let v = get_json(&client, &format!("{base}/speakers")).await;
        v["speakers"].as_array().map(|a| a.len()) == Some(3)
    })
    .await;

    // 1+2: exporters appear separately with correct identities.
    let speakers = get_json(&client, &format!("{base}/speakers")).await;
    let list = speakers["speakers"].as_array().unwrap();
    let find = |inst: &str| {
        list.iter()
            .find(|s| s["exporter_instance"] == inst)
            .unwrap_or_else(|| panic!("missing speaker {inst}"))
    };
    let spk_a = find("exp:exp-rtr-a");
    let spk_b = find("exp:exp-rtr-b");
    assert_eq!(spk_a["device_id"], dev_a);
    assert_eq!(spk_b["device_id"], dev_b);
    // Both connections share the SAME observed transport address.
    assert_eq!(spk_a["speaker_addr"], "127.0.0.1");
    assert_eq!(spk_b["speaker_addr"], "127.0.0.1");
    let conn_scopes: Vec<_> = list
        .iter()
        .filter(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap()
                .starts_with("conn:")
        })
        .collect();
    assert_eq!(
        conn_scopes.len(),
        1,
        "unknown exporter must be conn:*-isolated"
    );
    assert!(conn_scopes[0]["device_id"].is_null());

    // 3: route advertisements stay isolated per exporter scope. Wait for
    // the RouteMonitoring frames to be processed first.
    wait_for("exporter A route visible", || async {
        let v = get_json(
            &client,
            &format!("{base}/routes?exporter_instance=exp:exp-rtr-a"),
        )
        .await;
        v["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["prefix"] == "203.0.113.0/24")
    })
    .await;
    wait_for("exporter B route visible", || async {
        let v = get_json(
            &client,
            &format!("{base}/routes?exporter_instance=exp:exp-rtr-b"),
        )
        .await;
        v["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["prefix"] == "198.18.0.0/24")
    })
    .await;
    let routes_a = get_json(
        &client,
        &format!("{base}/routes?exporter_instance=exp:exp-rtr-a"),
    )
    .await;
    let routes_b = get_json(
        &client,
        &format!("{base}/routes?exporter_instance=exp:exp-rtr-b"),
    )
    .await;
    let pfx_a_str = "203.0.113.0/24";
    let pfx_b_str = "198.18.0.0/24";
    let routes_a_list = routes_a["routes"].as_array().unwrap();
    let routes_b_list = routes_b["routes"].as_array().unwrap();
    assert!(routes_a_list.iter().any(|r| r["prefix"] == pfx_a_str));
    assert!(!routes_a_list.iter().any(|r| r["prefix"] == pfx_b_str));
    assert!(routes_b_list.iter().any(|r| r["prefix"] == pfx_b_str));
    assert!(!routes_b_list.iter().any(|r| r["prefix"] == pfx_a_str));

    // speaker=<addr>|<instance> selector isolates one exporter.
    let key_a = format!("127.0.0.1|exp:exp-rtr-a");
    let routes_key = get_json(&client, &format!("{base}/routes?speaker={key_a}")).await;
    let rk = routes_key["routes"].as_array().unwrap();
    assert!(rk.iter().all(|r| r["prefix"] == pfx_a_str));

    // device_id selector on peers isolates exporter A.
    let peers_a = get_json(&client, &format!("{base}/peers?device_id={dev_a}")).await;
    let pa = peers_a["peers"].as_array().unwrap();
    assert_eq!(pa.len(), 1);
    assert_eq!(pa[0]["speaker_device_id"], dev_a);
    assert_eq!(pa[0]["peer_address"], "192.0.2.1");

    // conn:* exporter also has isolated routes (same prefix as B).
    let conn_inst = conn_scopes[0]["exporter_instance"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for("conn exporter route visible", || async {
        let v = get_json(
            &client,
            &format!("{base}/routes?exporter_instance={conn_inst}"),
        )
        .await;
        v["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["prefix"] == "198.18.0.0/24")
    })
    .await;
    let routes_x = get_json(
        &client,
        &format!("{base}/routes?exporter_instance={conn_inst}"),
    )
    .await;
    assert!(routes_x["routes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["prefix"] == pfx_b_str));

    // 4: withdraw A's route — B unaffected.
    let withdraw_a = bonsai::routing_service::sample_bmp_withdraw_frame(peer_ip, peer_as, pfx_a);
    write_frame(&mut conn_a, &withdraw_a).await;
    wait_for("A route withdrawn", || async {
        let v = get_json(
            &client,
            &format!("{base}/routes?exporter_instance=exp:exp-rtr-a"),
        )
        .await;
        !v["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["prefix"] == pfx_a_str)
    })
    .await;
    let routes_b2 = get_json(
        &client,
        &format!("{base}/routes?exporter_instance=exp:exp-rtr-b"),
    )
    .await;
    assert!(routes_b2["routes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["prefix"] == pfx_b_str));

    // 5: PeerDown on A does not affect B's peer/RIB.
    write_frame(&mut conn_a, &peer_down_frame(peer_ip, peer_as)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let peers_b = get_json(&client, &format!("{base}/peers?device_id={dev_b}")).await;
    let pb = peers_b["peers"].as_array().unwrap();
    assert_eq!(pb.len(), 1);
    assert_eq!(pb[0]["state"], "up");
    let routes_b3 = get_json(
        &client,
        &format!("{base}/routes?exporter_instance=exp:exp-rtr-b"),
    )
    .await;
    assert!(routes_b3["routes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["prefix"] == pfx_b_str));

    // 6: disconnecting A does not affect B's active session.
    drop(conn_a);
    // exp:* keeps last-known state under a transport gap once recorded.
    wait_for("A transport gap recorded", || async {
        let v = get_json(&client, &format!("{base}/speakers")).await;
        v["speakers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["exporter_instance"] == "exp:exp-rtr-a" && s["completeness_state"] == "gap")
    })
    .await;
    let speakers2 = get_json(&client, &format!("{base}/speakers")).await;
    let list2 = speakers2["speakers"].as_array().unwrap();
    assert!(list2
        .iter()
        .any(|s| s["exporter_instance"] == "exp:exp-rtr-b" && s["peer_count"].as_u64() == Some(1)));
    let spk_a2 = list2
        .iter()
        .find(|s| s["exporter_instance"] == "exp:exp-rtr-a")
        .expect("exp speaker retained under gap");
    assert_eq!(spk_a2["completeness_state"], "gap");

    // 10 (partial): the unknown conn:* exporter fails closed — no device id.
    assert!(list2
        .iter()
        .filter(|s| s["exporter_instance"]
            .as_str()
            .unwrap()
            .starts_with("conn:"))
        .all(|s| s["device_id"].is_null()));

    // Disconnect the unknown exporter: its conn:* speaker must retire.
    drop(conn_x);
    wait_for("conn speaker retired", || async {
        let v = get_json(&client, &format!("{base}/speakers")).await;
        !v["speakers"].as_array().unwrap().iter().any(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap()
                .starts_with("conn:")
        })
    })
    .await;
    // B is still fully alive.
    let speakers3 = get_json(&client, &format!("{base}/speakers")).await;
    assert!(speakers3["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["exporter_instance"] == "exp:exp-rtr-b"));

    // 8: journal replay reconstructs the same effective state.
    drop(conn_b);
    // Wait until both disconnect gaps are durably recorded before replay.
    let key_a = SpeakerKey::new("127.0.0.1".parse().unwrap(), "exp:exp-rtr-a");
    let key_b = SpeakerKey::new("127.0.0.1".parse().unwrap(), "exp:exp-rtr-b");
    wait_for("B disconnect gap recorded", || async {
        state.transport_gaps.read().await.contains_key(&key_b)
    })
    .await;
    wait_for("A disconnect gap recorded", || async {
        state.transport_gaps.read().await.contains_key(&key_a)
    })
    .await;
    drop(state);
    let mut rib = RibManager::new(128).0;
    let outcome = BmpJournal::replay_into_with_state(&bmp_path, &mut rib).unwrap();

    let key_b = SpeakerKey::new("127.0.0.1".parse().unwrap(), "exp:exp-rtr-b");
    assert!(rib.speaker(key_b).is_some(), "replay keeps exp:* speaker");
    // Wire peer headers carry flags=0 → AdjRibInPrePolicy; count all views.
    let replay_routes_b: usize = rib
        .ribs_for_speaker(key_b)
        .map(|(_, t)| t.all_prefixes().len())
        .sum();
    assert_eq!(replay_routes_b, 1, "replay keeps B's route");
    // No conn:* speaker may survive replay.
    assert!(rib
        .speakers()
        .iter()
        .all(|s| !s.source.exporter_instance.starts_with("conn:")));
    // Both disconnects are fenced to their own speaker keys.
    assert!(outcome.transport_gaps.contains_key(&key_b));
    assert!(outcome.transport_gaps.contains_key(&key_a));

    let _ = shutdown_tx.send(true);
}

// ── H. P0-A: configured trusted ingress scope reaches site bindings ──────────
//
// Canonical exporter ids are unique per (scope, protocol). Site-scoped
// bindings must become reachable ONLY through the operator-configured
// `identity_scope` — never inferred from self-reported sysName.

#[tokio::test]
async fn configured_identity_scope_reaches_site_scoped_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    {
        let store = SqliteStore::open(dir.path()).unwrap();
        let mut dev_a = canonical_device("10.20.0.11");
        dev_a.site = Some("site-a".to_string());
        let mut dev_d = canonical_device("10.20.0.12");
        dev_d.site = Some("default".to_string());
        let a = store.upsert_canonical_device(&dev_a, "test").unwrap();
        let d = store.upsert_canonical_device(&dev_d, "test").unwrap();

        // Same canonical exporter id "router-01" in two namespaces.
        let mut ba = bmp_binding("router-01", &a.device_id, "site-a-rtr");
        ba.scope = "site-a".to_string();
        let bd = bmp_binding("router-01", &d.device_id, "def-rtr");
        store.upsert_exporter_binding(&ba, "test").unwrap();
        store.upsert_exporter_binding(&bd, "test").unwrap();
    }

    // Listener configured for the site-a ingress scope.
    let state = RoutingServiceState::new(RoutingServiceConfig {
        bmp_listen_addr: "127.0.0.1:0".into(),
        http_listen_addr: "127.0.0.1:0".into(),
        bmp_journal_path: Some(dir.path().join("bmp.jsonl")),
        event_journal_path: Some(dir.path().join("events.jsonl")),
        max_frame_bytes: DEFAULT_MAX_FRAME,
        internal_service_token: None,
        identity_db_path: Some(db_path),
        identity_scope: Some("site-a".to_string()),
    })
    .await
    .unwrap();

    // Site-scoped claim resolves into the namespaced partition.
    let ra = state.resolve_bmp_exporter(nat_addr(), "c1", Some("site-a-rtr"), None);
    assert_eq!(ra.status, "resolved");
    assert_eq!(ra.scope.exporter_instance, "exp:site-a:router-01");
    assert!(ra.scope.device_id.is_some());

    // A default-scope binding is invisible under the site-a ingress
    // scope — scope is part of the trusted namespace, not a hint.
    let rd = state.resolve_bmp_exporter(nat_addr(), "c2", Some("def-rtr"), None);
    assert_ne!(rd.status, "resolved");
    assert!(rd.scope.exporter_instance.starts_with("conn:"));
    assert!(rd.scope.device_id.is_none());

    // And under the DEFAULT scope the inverse holds (covered by
    // cross_scope_duplicate_exporter_ids_cannot_merge for the
    // unconfigured-listener case).
}

// ── I. P1: routing service opens the identity DB read-only ───────────────────

#[tokio::test]
async fn identity_store_is_opened_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let db_file = dir.path().join(CONFIG_DB_FILE_NAME);
    {
        let store = SqliteStore::open(dir.path()).unwrap();
        store
            .upsert_canonical_device(&canonical_device("10.30.0.1"), "test")
            .unwrap();
    }

    let ro = SqliteStore::open_file_readonly(&db_file).unwrap();
    assert_eq!(ro.schema_version().unwrap(), 3);
    // Reads still work.
    assert!(ro
        .list_canonical_devices()
        .unwrap()
        .iter()
        .any(|d| d.address == "10.30.0.1"));
    // Writes must fail — the routing service can never mutate the
    // shared management database.
    assert!(ro
        .upsert_canonical_device(&canonical_device("10.30.0.2"), "test")
        .is_err());

    // A missing database must NOT be created (previous open_file would
    // have created an empty schema-bearing file).
    let missing = dir.path().join("missing.db");
    assert!(SqliteStore::open_file_readonly(&missing).is_err());
    assert!(!missing.exists());
}

// ── J. P0-D: untrusted claims — ambiguous evidence fails closed ──────────────
//
// BMP sysName is a self-reported claim, not authenticated identity.
// Identical claims from unrelated exporters behind one NAT resolve
// identically BY DESIGN (FRR multi-instance coalescing relies on it);
// the trust anchor is the operator-provisioned binding. Ambiguous or
// unmatched evidence must never acquire authoritative identity.

#[tokio::test]
async fn ambiguous_claims_fail_closed_into_connection_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    {
        let store = SqliteStore::open(dir.path()).unwrap();
        let a = store
            .upsert_canonical_device(&canonical_device("10.40.0.11"), "test")
            .unwrap();
        let b = store
            .upsert_canonical_device(&canonical_device("10.40.0.12"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&bmp_binding("exp-a", &a.device_id, "frr-a"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(&bmp_binding("exp-b", &b.device_id, "frr-b"), "test")
            .unwrap();
    }
    let state = test_state(&dir, Some(db_path.clone())).await;

    // Distinct trusted claims resolve to their own exporters.
    let ra = state.resolve_bmp_exporter(nat_addr(), "c1", Some("frr-a"), None);
    let rb = state.resolve_bmp_exporter(nat_addr(), "c2", Some("frr-b"), None);
    assert_eq!(ra.scope.exporter_instance, "exp:exp-a");
    assert_eq!(rb.scope.exporter_instance, "exp:exp-b");

    // An unbound claim fails closed.
    let rx = state.resolve_bmp_exporter(nat_addr(), "c3", Some("frr-evil"), None);
    assert!(rx.scope.exporter_instance.starts_with("conn:"));
    assert!(rx.scope.device_id.is_none());

    // THREAT SCENARIO (documented limitation): a second, unrelated
    // exporter presenting the SAME "frr-a" claim from the same NAT
    // resolves identically and coalesces into exp:exp-a. BMP sysName is
    // self-reported; only an operator binding + future ingress
    // authentication can distinguish them. Assert the actual behavior
    // so the trust assumption stays explicit.
    let ra2 = state.resolve_bmp_exporter(nat_addr(), "c4", Some("frr-a"), None);
    assert_eq!(ra2.scope.exporter_instance, "exp:exp-a");
    assert_eq!(ra2.scope.device_id, ra.scope.device_id);

    // Ambiguous provisioning is rejected at write time: a catch-all
    // binding overlaps the pinned ones and upsert refuses it — the
    // store itself fails closed against ambiguous registrations.
    {
        let store = SqliteStore::open(dir.path()).unwrap();
        let c = store
            .upsert_canonical_device(&canonical_device("10.40.0.13"), "test")
            .unwrap();
        let mut catch_all = bmp_binding("exp-catchall", &c.device_id, "unused");
        catch_all.match_transport = None;
        catch_all.match_reported_identity = None;
        catch_all.match_exporter_claim = None;
        assert!(
            store.upsert_exporter_binding(&catch_all, "test").is_err(),
            "overlapping catch-all binding must be rejected at provisioning"
        );
    }

    // If ambiguous bindings ever DO coexist (e.g. imported data), the
    // resolver itself must still fail closed. Evaluate the resolver
    // directly with a synthetic overlapping set.
    let mut catch_all = bmp_binding("exp-catchall", "dev-catchall", "unused");
    catch_all.match_transport = None;
    catch_all.match_reported_identity = None;
    catch_all.match_exporter_claim = None;
    let probe = rbmp_core::bmp::types::ObservationSource::with_ids(
        "default",
        "bmp:frr-a".to_string(),
        nat_addr(),
        "pending",
        0,
    );
    let observed =
        bonsai::telemetry_identity::identity_from_bmp_observation(&probe, Some("frr-a"), None);
    let bindings = vec![
        bmp_binding("exp-a", "dev-a", "frr-a"),
        bmp_binding("exp-b", "dev-b", "frr-b"),
        catch_all,
    ];
    let res = bonsai::telemetry_identity::IdentityResolver::resolve(&observed, &bindings);
    assert_eq!(
        res.status,
        bonsai::telemetry_identity::IdentityStatus::Ambiguous,
        "overlapping claimants must resolve Ambiguous, never first-match"
    );
    assert!(res.device_id.is_none());
    assert!(res.canonical_exporter_id.is_none());
}

// ── K/L. TCP-level helpers shared by the lifecycle tests ─────────────────────

struct TcpServices {
    bmp_addr: SocketAddr,
    base: String,
    shutdown: tokio::sync::watch::Sender<bool>,
}

async fn start_tcp_services(state: &Arc<RoutingServiceState>) -> TcpServices {
    let bmp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bmp_addr = bmp_listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let listener_state = Arc::clone(state);
    tokio::spawn(async move { run_bmp_listener(bmp_listener, listener_state, shutdown_rx).await });

    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let http_state = Arc::clone(state);
    tokio::spawn(async move {
        axum::serve(http_listener, build_internal_routing_router(http_state))
            .await
            .unwrap();
    });
    TcpServices {
        bmp_addr,
        base: format!("http://{http_addr}/internal/routing/v1"),
        shutdown: shutdown_tx,
    }
}

fn termination_frame() -> Vec<u8> {
    // BMPv3 Termination (type 5), empty TLV body: clean session end.
    bmp_frame(5, &[])
}

fn invalid_frame() -> Vec<u8> {
    // Valid BMP type (PeerUp) with a malformed body: passes header
    // validation in `read_bmp_frame` but is rejected by the payload
    // parser inside ingestion — the path that previously bypassed
    // disconnect lifecycle entirely.
    bmp_frame(3, &[0xaa; 60])
}

fn loopback_binding(sys_name: &str, exporter_id: &str, device_id: &str) -> TrustedBinding {
    TrustedBinding {
        binding_id: SqliteStore::new_binding_id(),
        scope: "default".to_string(),
        protocol: "bmp".to_string(),
        exporter_id: exporter_id.to_string(),
        device_id: device_id.to_string(),
        match_transport: Some("127.0.0.1".to_string()),
        match_reported_identity: Some(sys_name.to_string()),
        match_exporter_claim: Some(format!("bmp:{sys_name}")),
        match_observation_domain: None,
        provenance: BindingProvenance::OperatorProvisioned,
        status: BindingStatus::Active,
        created_at_ns: 0,
        updated_at_ns: 0,
        created_by: "test".to_string(),
        updated_by: "test".to_string(),
    }
}

async fn speaker_instances(client: &reqwest::Client, base: &str) -> Vec<String> {
    let v = get_json(client, &format!("{base}/speakers")).await;
    v["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["exporter_instance"].as_str().map(str::to_string))
        .collect()
}

// ── L. P0-B: an invalid BMP frame must run disconnect lifecycle ──────────────

#[tokio::test]
async fn invalid_frame_records_disconnect_and_retires_conn_scope() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let state = test_state(&dir, None).await;
    let svc = start_tcp_services(&state).await;
    let client = reqwest::Client::new();

    let peer_ip = Ipv4Addr::new(192, 0, 2, 55);
    let peer_as = 65055u32;
    let (_i, up, ann) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, [203, 0, 113], 100);

    let mut conn = TcpStream::connect(svc.bmp_addr).await.unwrap();
    write_frame(&mut conn, &initiation_frame("frr-bad")).await;
    write_frame(&mut conn, &up).await;
    write_frame(&mut conn, &ann).await;

    // Wait for the conn:* speaker and its route to appear.
    wait_for("conn speaker", || async {
        speaker_instances(&client, &svc.base)
            .await
            .iter()
            .any(|i| i.starts_with("conn:"))
    })
    .await;
    let conn_inst = speaker_instances(&client, &svc.base)
        .await
        .into_iter()
        .find(|i| i.starts_with("conn:"))
        .expect("conn speaker instance");
    wait_for("conn route visible", || async {
        let v = get_json(
            &client,
            &format!("{}/routes?exporter_instance={conn_inst}", svc.base),
        )
        .await;
        v["routes"].as_array().unwrap().len() == 1
    })
    .await;

    // Send a malformed frame: the listener rejects it AND closes the
    // connection — the disconnect lifecycle must still run with a
    // reason distinct from TCP EOF.
    write_frame(&mut conn, &invalid_frame()).await;
    // Let the listener consume the frame and close the connection
    // itself; an immediate drop can RST and discard the pending frame.
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(conn);

    // conn:* scope is retired — no last-known routes leak. The durable
    // disconnect record (with the distinct rejection reason) remains
    // in the journal even though the in-memory gap is pruned on
    // retirement.
    wait_for("conn speaker retired", || async {
        speaker_instances(&client, &svc.base)
            .await
            .iter()
            .all(|i| !i.starts_with("conn:"))
    })
    .await;
    let journal_text = std::fs::read_to_string(&bmp_path).unwrap();
    assert!(
        journal_text.contains("bmp_frame_rejected"),
        "journal must record the protocol rejection distinctly from EOF; journal tail: {}",
        &journal_text[journal_text.len().saturating_sub(2000)..]
    );

    // Replay must not resurrect the retired scope either.
    drop(state);
    let mut rib = RibManager::new(128).0;
    let outcome = BmpJournal::replay_into_with_state(&bmp_path, &mut rib).unwrap();
    assert!(
        rib.speakers()
            .iter()
            .all(|s| !s.source.exporter_instance.starts_with("conn:")),
        "replay must not resurrect retired conn:* speaker"
    );
    assert!(outcome
        .transport_gaps
        .values()
        .all(|g| !g.exporter_instance.starts_with("conn:")));

    let _ = svc.shutdown.send(true);
}

// ── M. P0-1/P0-C: same-claim concurrent connections cannot merge ────────────
//
// BMP sysName is a self-reported claim, not authenticated identity. A
// second SIMULTANEOUS connection presenting an identical claim cannot be
// proven to share exporter ownership, so the listener admits only ONE
// live connection per resolved `exp:*` scope; extras are isolated into
// `conn:*` speaker state (resolved attribution fields preserved).
// Sequential same-claim reconnects still rejoin the persistent `exp:*`
// scope — indistinguishable from a legitimate reconnect — which remains
// a documented limitation pending authenticated ingress (BN-021f).

#[tokio::test]
async fn same_claim_concurrent_connections_stay_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    let dev_id = {
        let store = SqliteStore::open(dir.path()).unwrap();
        let d = store
            .upsert_canonical_device(&canonical_device("10.50.0.9"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(
                &loopback_binding("frr-shared", "exp-shared", &d.device_id),
                "test",
            )
            .unwrap();
        d.device_id
    };
    let state = test_state(&dir, Some(db_path)).await;
    let svc = start_tcp_services(&state).await;
    let client = reqwest::Client::new();

    let peer_ip1 = Ipv4Addr::new(192, 0, 2, 1);
    let peer_ip2 = Ipv4Addr::new(192, 0, 2, 2);
    let (_i1, up1, ann1) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip1, 65001, [203, 0, 113], 100);
    let (_i2, up2, ann2) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip2, 65002, [198, 18, 0], 200);

    // conn1 establishes the exp:* speaker first.
    let mut conn1 = TcpStream::connect(svc.bmp_addr).await.unwrap();
    write_frame(&mut conn1, &initiation_frame("frr-shared")).await;
    write_frame(&mut conn1, &up1).await;
    write_frame(&mut conn1, &ann1).await;
    wait_for("exp speaker with 1 peer", || async {
        let v = get_json(&client, &format!("{}/speakers", svc.base)).await;
        v["speakers"].as_array().unwrap().iter().any(|s| {
            s["exporter_instance"] == "exp:exp-shared" && s["peer_count"].as_u64() == Some(1)
        })
    })
    .await;

    // conn2 presents the IDENTICAL claim while conn1 is live. It must
    // NOT merge into exp:exp-shared — it is admitted as conn:* with
    // resolved attribution preserved.
    let mut conn2 = TcpStream::connect(svc.bmp_addr).await.unwrap();
    write_frame(&mut conn2, &initiation_frame("frr-shared")).await;
    write_frame(&mut conn2, &up2).await;
    write_frame(&mut conn2, &ann2).await;
    wait_for("conn-isolated sibling speaker", || async {
        let v = get_json(&client, &format!("{}/speakers", svc.base)).await;
        v["speakers"].as_array().unwrap().iter().any(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap()
                .starts_with("conn:")
                && s["peer_count"].as_u64() == Some(1)
        })
    })
    .await;

    let speakers = get_json(&client, &format!("{}/speakers", svc.base)).await;
    let list = speakers["speakers"].as_array().unwrap();
    let exp_spk = list
        .iter()
        .find(|s| s["exporter_instance"] == "exp:exp-shared")
        .expect("exp speaker");
    assert_eq!(exp_spk["peer_count"].as_u64(), Some(1));
    assert_eq!(exp_spk["total_routes"].as_u64(), Some(1));
    let conn_spk = list
        .iter()
        .find(|s| {
            s["exporter_instance"]
                .as_str()
                .unwrap()
                .starts_with("conn:")
        })
        .expect("conn speaker");
    assert_eq!(conn_spk["peer_count"].as_u64(), Some(1));
    assert_eq!(conn_spk["total_routes"].as_u64(), Some(1));
    // Attribution is preserved even though speaker state is isolated.
    assert_eq!(conn_spk["device_id"], dev_id);
    let conn_inst = conn_spk["exporter_instance"].as_str().unwrap().to_string();

    // conn2's prefix must be visible ONLY under its conn scope.
    wait_for("conn route visible", || async {
        let v = get_json(
            &client,
            &format!("{}/routes?exporter_instance={conn_inst}", svc.base),
        )
        .await;
        v["routes"].as_array().unwrap().len() == 1
    })
    .await;
    let exp_routes = get_json(
        &client,
        &format!("{}/routes?exporter_instance=exp:exp-shared", svc.base),
    )
    .await;
    let pfxs: Vec<&str> = exp_routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["prefix"].as_str())
        .collect();
    assert_eq!(
        pfxs,
        vec!["203.0.113.0/24"],
        "exp scope must not contain sibling routes: {pfxs:?}"
    );

    // conn1 Termination tears down ONLY the exp speaker; conn2 is a
    // different SpeakerKey and is completely unaffected.
    write_frame(&mut conn1, &termination_frame()).await;
    drop(conn1);
    wait_for("exp speaker torn down", || async {
        let insts = speaker_instances(&client, &svc.base).await;
        !insts.iter().any(|i| i == "exp:exp-shared")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let conn_routes = get_json(
        &client,
        &format!("{}/routes?exporter_instance={conn_inst}", svc.base),
    )
    .await;
    assert_eq!(
        conn_routes["routes"].as_array().unwrap().len(),
        1,
        "conn:* sibling state must survive exp:* Termination"
    );

    // conn2 terminates as the last connection of ITS scope.
    write_frame(&mut conn2, &termination_frame()).await;
    drop(conn2);
    wait_for("conn speaker torn down", || async {
        let insts = speaker_instances(&client, &svc.base).await;
        insts.iter().all(|i| !i.starts_with("conn:"))
    })
    .await;

    // Replay reconstructs the same effective lifecycle.
    drop(state);
    let mut rib = RibManager::new(128).0;
    BmpJournal::replay_into_with_state(&bmp_path, &mut rib).unwrap();
    assert!(
        rib.speakers().iter().all(|s| {
            !(s.source.exporter_instance == "exp:exp-shared"
                || s.source.exporter_instance.starts_with("conn:"))
        }),
        "replay must not resurrect either terminated scope"
    );

    let _ = svc.shutdown.send(true);
}

// ── N. P0-3 Case A: frames after BMP Termination must not mutate state ───────

#[tokio::test]
async fn post_termination_frames_cannot_mutate_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir, None).await;
    let svc = start_tcp_services(&state).await;
    let client = reqwest::Client::new();

    let peer_ip = Ipv4Addr::new(192, 0, 2, 77);
    let peer_as = 65077u32;
    let (_i, up, ann) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, [203, 0, 113], 100);
    let (_i2, up2, ann2) =
        bonsai::routing_service::sample_bmp_session_frames(peer_ip, peer_as, [198, 18, 0], 200);

    let mut conn = TcpStream::connect(svc.bmp_addr).await.unwrap();
    write_frame(&mut conn, &initiation_frame("frr-term")).await;
    write_frame(&mut conn, &up).await;
    write_frame(&mut conn, &ann).await;
    wait_for("pre-termination route", || async {
        speaker_instances(&client, &svc.base)
            .await
            .iter()
            .any(|i| i.starts_with("conn:"))
    })
    .await;

    // Terminate, then keep writing on the same TCP connection. A
    // terminated BMP session accepts no further observations.
    write_frame(&mut conn, &termination_frame()).await;
    write_frame(&mut conn, &up2).await;
    write_frame(&mut conn, &ann2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let insts = speaker_instances(&client, &svc.base).await;
    assert!(
        insts.iter().all(|i| !i.starts_with("conn:")),
        "speaker must stay terminated; post-Termination frames resurrected state: {insts:?}"
    );
    let speakers = get_json(&client, &format!("{}/speakers", svc.base)).await;
    assert_eq!(speakers["speakers"].as_array().unwrap().len(), 0);

    let _ = svc.shutdown.send(true);
}

// ── O. P0-3 Case D: clean Termination records no transport gap ───────────────

#[tokio::test]
async fn clean_termination_records_no_transport_gap() {
    let dir = tempfile::tempdir().unwrap();
    let bmp_path = dir.path().join("bmp.jsonl");
    let db_path = dir.path().join(CONFIG_DB_FILE_NAME);
    let dev_id = {
        let store = SqliteStore::open(dir.path()).unwrap();
        let d = store
            .upsert_canonical_device(&canonical_device("10.60.0.9"), "test")
            .unwrap();
        store
            .upsert_exporter_binding(
                &loopback_binding("frr-clean", "exp-clean", &d.device_id),
                "test",
            )
            .unwrap();
        d.device_id
    };
    let _ = dev_id;
    let state = test_state(&dir, Some(db_path)).await;
    let svc = start_tcp_services(&state).await;
    let client = reqwest::Client::new();

    let (_i, up, ann) = bonsai::routing_service::sample_bmp_session_frames(
        Ipv4Addr::new(192, 0, 2, 1),
        65001,
        [203, 0, 113],
        100,
    );
    let mut conn = TcpStream::connect(svc.bmp_addr).await.unwrap();
    write_frame(&mut conn, &initiation_frame("frr-clean")).await;
    write_frame(&mut conn, &up).await;
    write_frame(&mut conn, &ann).await;
    wait_for("exp speaker live", || async {
        speaker_instances(&client, &svc.base)
            .await
            .iter()
            .any(|i| i == "exp:exp-clean")
    })
    .await;

    // Clean BMP Termination followed by TCP close must NOT leave a
    // transport-gap record: the session ended deliberately. Previously
    // the trailing EOF was journaled as bmp_tcp_eof and pinned global
    // completeness to "gap" for a dead speaker.
    write_frame(&mut conn, &termination_frame()).await;
    drop(conn);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let key = SpeakerKey::new("127.0.0.1".parse().unwrap(), "exp:exp-clean");
    assert!(
        !state.transport_gaps.read().await.contains_key(&key),
        "clean Termination must not record a transport gap"
    );
    let health = get_json(&client, &format!("{}/health", svc.base)).await;
    assert_ne!(
        health["completeness_state"].as_str().unwrap_or(""),
        "gap",
        "dead speaker must not pin completeness to gap: {health}"
    );

    // Replay agrees: no gap record exists for the terminated speaker.
    drop(state);
    let mut rib = RibManager::new(128).0;
    let outcome = BmpJournal::replay_into_with_state(&bmp_path, &mut rib).unwrap();
    assert!(
        outcome.transport_gaps.values().all(|g| {
            !(g.speaker_addr == IpAddr::from([127, 0, 0, 1])
                && g.exporter_instance == "exp:exp-clean")
        }),
        "replay must not surface a gap for the cleanly terminated speaker"
    );

    let _ = svc.shutdown.send(true);
}

// ── P. P0-2: journal write failure is observable and blocks retirement ───────

#[tokio::test]
async fn disconnect_journal_failure_is_observable_and_blocks_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir, None).await;
    let scope = "conn:exporter-fail".to_string();
    let speaker = nat_addr();

    // Establish a conn-scoped speaker with a route.
    state
        .ingest_bmp_message(initiation_msg(&scope, "frr-fail"), Some(7))
        .await
        .unwrap();
    state
        .ingest_bmp_message(peer_up_msg(&scope), Some(7))
        .await
        .unwrap();
    state
        .ingest_bmp_message(announce_msg(&scope, 100), Some(7))
        .await
        .unwrap();
    let key = SpeakerKey::new(speaker, &scope);
    assert!(state.rib.read().await.speaker(key).is_some());

    // Inject a durable-write failure: the recorder must report it and
    // mutate NOTHING (no in-memory gap, EOR state untouched).
    state
        .bmp_journal
        .fail_writes
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let outcome = state
        .record_transport_disconnect_for_key(key, &scope, Some(7), "tcp eof")
        .await;
    assert_eq!(outcome, TransportDisconnectOutcome::PersistenceFailed);
    assert!(
        !state.transport_gaps.read().await.contains_key(&key),
        "failed persistence must not record an in-memory gap"
    );
    assert!(
        state.rib.read().await.speaker(key).is_some(),
        "live state must be left untouched on persistence failure"
    );

    // Recovery: with writes restored the same disconnect records durably.
    state
        .bmp_journal
        .fail_writes
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let outcome = state
        .record_transport_disconnect_for_key(key, &scope, Some(7), "tcp eof")
        .await;
    assert_eq!(outcome, TransportDisconnectOutcome::Recorded);
    assert!(state.transport_gaps.read().await.contains_key(&key));
}

// ── Q. P0-2/P0-3: disconnect recorder outcome matrix ─────────────────────────

#[tokio::test]
async fn disconnect_recorder_distinguishes_stale_recorded_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(&dir, None).await;
    let scope = "exp:exp-matrix".to_string();
    let speaker = nat_addr();
    let key = SpeakerKey::new(speaker, &scope);

    state
        .ingest_bmp_message(initiation_msg(&scope, "frr-m"), Some(11))
        .await
        .unwrap();
    state
        .ingest_bmp_message(peer_up_msg(&scope), Some(11))
        .await
        .unwrap();

    // Stale epoch: an older connection's disconnect cannot touch the
    // newer session (Case C fencing primitive).
    let out = state
        .record_transport_disconnect_for_key(key, &scope, Some(10), "old eof")
        .await;
    assert_eq!(out, TransportDisconnectOutcome::Stale);
    assert!(!state.transport_gaps.read().await.contains_key(&key));

    // Active epoch: durably recorded.
    let out = state
        .record_transport_disconnect_for_key(key, &scope, Some(11), "tcp eof")
        .await;
    assert_eq!(out, TransportDisconnectOutcome::Recorded);
    assert!(state.transport_gaps.read().await.contains_key(&key));

    // Clean Termination removes the speaker AND clears the gap; a
    // subsequent disconnect for the dead session is NoActiveSession —
    // no meaningless gap is re-recorded (Case D).
    state
        .ingest_bmp_message(
            bmp_msg(
                &scope,
                BmpPayload::Termination {
                    reason_code: 0,
                    reason_text: None,
                },
            ),
            Some(11),
        )
        .await
        .unwrap();
    assert!(state.rib.read().await.speaker(key).is_none());
    assert!(!state.transport_gaps.read().await.contains_key(&key));
    let out = state
        .record_transport_disconnect_for_key(key, &scope, Some(11), "tcp eof")
        .await;
    assert_eq!(out, TransportDisconnectOutcome::NoActiveSession);
    assert!(!state.transport_gaps.read().await.contains_key(&key));
}

// ── R. P1-2: exporter-instance encoding cannot produce ambiguous ids ────────

#[test]
fn exporter_instance_encoding_is_unambiguous() {
    // Typical values keep the historical readable form.
    assert_eq!(
        resolved_exporter_instance("default", "exp-frr-a"),
        "exp:exp-frr-a"
    );
    assert_eq!(
        resolved_exporter_instance("site-a", "router-01"),
        "exp:site-a:router-01"
    );

    // Separator-bearing components must never serialize two distinct
    // (scope, canonical) pairs to the same composite id.
    let a = resolved_exporter_instance("a:b", "c");
    let b = resolved_exporter_instance("a", "b:c");
    assert_ne!(a, b, "ambiguous composite ids: {a} vs {b}");
    let c = resolved_exporter_instance("a%3Ab", "c");
    assert_ne!(a, c, "percent must itself be encoded: {a} vs {c}");

    // And the composite ids must hash to distinct SpeakerKeys.
    let addr: IpAddr = "10.0.0.1".parse().unwrap();
    assert_ne!(
        SpeakerKey::new(addr, &a),
        SpeakerKey::new(addr, &b),
        "distinct logical exporters must not share a SpeakerKey"
    );

    // A default-scope canonical id containing ':' cannot collide with a
    // namespaced (scope, canonical) pair either.
    let d = resolved_exporter_instance("default", "x:y");
    let e = resolved_exporter_instance("x", "y");
    assert_ne!(d, e);
    assert_ne!(SpeakerKey::new(addr, &d), SpeakerKey::new(addr, &e));
}

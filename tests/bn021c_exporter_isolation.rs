//! BN-021c regression: two independent BMP exporters behind one NAT
//! address must never merge route, peer, session, transport-gap, journal
//! or replay state.
//!
//! All data is synthetic; no live devices, no credential material.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use bonsai::routing_service::{RoutingServiceConfig, RoutingServiceState};
use bonsai::sqlite_store::{CONFIG_DB_FILE_NAME, SqliteStore};
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
use std::sync::Arc;
use tempfile::TempDir;
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
        internal_service_token: None,
        identity_db_path: identity_db,
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

    for (scope, epoch, lp) in [
        ("conn:exporter-a", 21u64, 100u32),
        ("conn:exporter-b", 22u64, 300u32),
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

    let key_a = SpeakerKey::new(nat_addr(), "conn:exporter-a");
    let key_b = SpeakerKey::new(nat_addr(), "conn:exporter-b");
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
}

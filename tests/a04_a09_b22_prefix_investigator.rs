//! Comprehensive verification suite for Slice P1 (A04–A09 + B22):
//! - A04: Bounded scoped diagnostic tools (`get_prefix_journey`, `get_peer_timeline`,
//!        `get_evidence_coverage`, `get_changes`, `compare_snapshots`, `explain_missing_prefix`)
//!        and `MAX_TOOL_OUTPUT_BYTES` / `MAX_GRAPH_ROWS` caps.
//! - A05: Versioned canonical `ObservationEnvelope` and lineage from durable `RibEvent`s.
//! - A06: Incident-specific evidence coverage (healthy unrelated speaker never masks missing
//!        BMP/view/EOR evidence on the target speaker/VRF).
//! - A07: Prefix journey across views (`AdjRibInPrePolicy`, `LocRib`, `AdjRibOutPostPolicy`)
//!        and deterministic snapshot comparison (`compare_snapshots`).
//! - A08: Pre-call `BudgetReservationLedger` & `ToolCallDeduplicator`.
//! - A09 & B22: Grounded read-only "Explain a Missing Prefix" investigator across
//!        supported diagnosis, rejected hypothesis, and insufficient-evidence cases.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use bonsai::investigation_runtime::{
    resolve_investigation_outcome, BudgetReservationLedger, InvestigationTerminalState,
    LoopExitReason, ToolCallDeduplicator,
};
use bonsai::mcp_server::{
     enforce_tool_output_bounds, tool_schemas, MAX_TOOL_OUTPUT_BYTES,
};
use bonsai::routing_service::{
    build_compare_snapshots_json, build_coverage_json, build_evidence_coverage_json,
    build_explain_prefix_json, build_peer_timeline_json, build_prefix_journey_json,
    sample_bmp_session_frames, sample_bmp_withdraw_frame, CompareSnapshotsQuery,
    EvidenceCoverageQuery, ExplainPrefixQuery, ObservationEnvelope, PeerTimelineQuery,
    PrefixJourneyQuery, RoutingServiceConfig, RoutingServiceState, OBSERVATION_SCHEMA_VERSION,
};
use serde_json::json;

fn build_eor_frame(peer_type: u8, peer_flags: u8, rd: [u8; 8], peer_ip: Ipv4Addr, peer_as: u32) -> Vec<u8> {
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(peer_type);
    peer_hdr.push(peer_flags);
    peer_hdr.extend_from_slice(&rd);
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&1700000005u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let mut bgp_eor = Vec::new();
    bgp_eor.extend_from_slice(&[0xff; 16]);
    bgp_eor.extend_from_slice(&23u16.to_be_bytes());
    bgp_eor.push(2); // UPDATE
    bgp_eor.extend_from_slice(&0u16.to_be_bytes());
    bgp_eor.extend_from_slice(&0u16.to_be_bytes());

    let rm_len = (6 + 42 + bgp_eor.len()) as u32;
    let mut frame = Vec::with_capacity(rm_len as usize);
    frame.push(3);
    frame.extend_from_slice(&rm_len.to_be_bytes());
    frame.push(0); // RouteMonitoring
    frame.extend_from_slice(&peer_hdr);
    frame.extend_from_slice(&bgp_eor);
    frame
}

fn build_loc_rib_peer_up_and_route(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix_octets: [u8; 3],
    local_pref: u32,
) -> (Vec<u8>, Vec<u8>) {
    // peer_type = 3 (RFC 9069 Loc-RIB), flags = 0
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(3); // LocRibInstance
    peer_hdr.push(0);
    peer_hdr.extend_from_slice(&[0u8; 8]);
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&1700000002u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let mut open = Vec::with_capacity(29);
    open.extend_from_slice(&[0xff; 16]);
    open.extend_from_slice(&29u16.to_be_bytes());
    open.push(1);
    open.push(4);
    open.extend_from_slice(&(peer_as as u16).to_be_bytes());
    open.extend_from_slice(&90u16.to_be_bytes());
    open.extend_from_slice(&peer_ip.octets());
    open.push(0);

    let mut peer_up = Vec::with_capacity(126);
    peer_up.push(3);
    peer_up.extend_from_slice(&126u32.to_be_bytes());
    peer_up.push(3);
    peer_up.extend_from_slice(&peer_hdr);
    peer_up.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 254]);
    peer_up.extend_from_slice(&179u16.to_be_bytes());
    peer_up.extend_from_slice(&50000u16.to_be_bytes());
    peer_up.extend_from_slice(&open);
    peer_up.extend_from_slice(&open);

    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]);
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1]);
    attrs.extend_from_slice(&peer_as.to_be_bytes());
    attrs.extend_from_slice(&[0x40, 3, 4]);
    attrs.extend_from_slice(&peer_ip.octets());
    attrs.extend_from_slice(&[0x40, 5, 4]);
    attrs.extend_from_slice(&local_pref.to_be_bytes());

    let nlri = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + 2 + attrs.len() + nlri.len()) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2);
    bgp_update.extend_from_slice(&0u16.to_be_bytes());
    bgp_update.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&attrs);
    bgp_update.extend_from_slice(&nlri);

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0);
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);

    (peer_up, rm)
}

#[tokio::test]
async fn a04_scoped_mcp_tools_and_output_byte_caps() {
    let schemas = tool_schemas();
    let tool_names: Vec<String> = schemas["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();

    for expected in [
        "get_incident",
        "query_devices",
        "get_device_blast_radius",
        "list_active_detections",
        "query_graph",
        "get_prefix_journey",
        "get_peer_timeline",
        "get_evidence_coverage",
        "get_changes",
        "compare_snapshots",
        "explain_missing_prefix",
    ] {
        assert!(
            tool_names.contains(&expected.to_string()),
            "missing expected MCP tool {expected} in {tool_names:?}"
        );
    }

    // Verify hard byte ceiling (MAX_TOOL_OUTPUT_BYTES = 65_536)
    let oversized_str = "x".repeat(MAX_TOOL_OUTPUT_BYTES + 10_000);
    let bounded = enforce_tool_output_bounds(json!({ "huge_field": oversized_str }));
    assert_eq!(bounded["truncated"], true);
    assert_eq!(bounded["byte_cap"], MAX_TOOL_OUTPUT_BYTES);
    assert!(bounded.to_string().len() <= MAX_TOOL_OUTPUT_BYTES);
}

#[tokio::test]
async fn a05_canonical_observation_envelope_preserves_identity_and_lineage() {
    let state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();
    let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
    let (init, peer_up, rm) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113], 150);
    state.ingest_bmp_bytes(speaker, Some(7), &init).await.unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(7), &peer_up)
        .await
        .unwrap();
    state.ingest_bmp_bytes(speaker, Some(7), &rm).await.unwrap();

    let events = state.event_journal.events_after_seq(0, 10).await;
    let route_ev = events
        .iter()
        .find(|e| matches!(e.payload, rbmp_rib::event::RibEventPayload::RouteChange(_)))
        .expect("RouteChange event present");

    let env = ObservationEnvelope::from_rib_event(route_ev, "tenant-alpha", false);
    assert_eq!(env.schema_version, OBSERVATION_SCHEMA_VERSION);
    assert_eq!(env.tenant_id, "tenant-alpha");
    assert_eq!(env.source.kind, "bmp");
    assert_eq!(env.source.speaker_addr, "192.0.2.10");
    assert_eq!(env.source.session_epoch, 7);
    assert_eq!(env.source_semantics.view, "AdjRibInPrePolicy");
    assert_eq!(env.source_semantics.direction, "inbound");
    assert_eq!(env.source_semantics.vrf_rd, "0:0");
    assert_eq!(env.quality.completeness, "complete_for_observed_view");
    assert!(env.entity_refs.contains(&"device:192.0.2.10".to_string()));
    assert!(env.entity_refs.contains(&"vrf:0:0".to_string()));
    assert!(env
        .entity_refs
        .contains(&"prefix:203.0.113.0/24".to_string()));
}

#[tokio::test]
async fn a06_incident_specific_evidence_coverage_never_masks_target_speaker_gap() {
    let state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();

    // Unrelated speaker 192.0.2.10 is healthy with PeerUp + Route + End-of-RIB
    let healthy_speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
    let (init, peer_up, rm) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113], 100);
    state
        .ingest_bmp_bytes(healthy_speaker, Some(1), &init)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(healthy_speaker, Some(1), &peer_up)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(healthy_speaker, Some(1), &rm)
        .await
        .unwrap();
    let eor = build_eor_frame(0, 0, [0u8; 8], Ipv4Addr::new(10, 0, 0, 1), 65001);
    state
        .ingest_bmp_bytes(healthy_speaker, Some(1), &eor)
        .await
        .unwrap();

    // Target incident speaker 192.0.2.20 has ONLY sent BMP Initiation (0 peers, no EOR)
    let target_speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20));
    state
        .ingest_bmp_bytes(target_speaker, Some(1), &init)
        .await
        .unwrap();

    // 1. Global coverage must now report "syncing" because 192.0.2.20 has 0 peers / no EOR
    let global_cov = build_coverage_json(&state).await;
    assert_eq!(global_cov["completeness_state"], "syncing");

    // 2. Scoped coverage for target speaker 192.0.2.20 reports "syncing"
    let target_cov = build_evidence_coverage_json(
        &state,
        &EvidenceCoverageQuery {
            speaker: Some("192.0.2.20".to_string()),
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(target_cov["completeness_state"], "syncing");
    assert_eq!(target_cov["fib_forwarding_evidence_available"], false);

    // 3. Scoped coverage for healthy speaker 192.0.2.10 reports "complete_for_observed_view"
    //    but explicitly lists missing views (LocRib, AdjRibInPostPolicy, AdjRibOutPostPolicy)
    let healthy_cov = build_evidence_coverage_json(
        &state,
        &EvidenceCoverageQuery {
            speaker: Some("192.0.2.10".to_string()),
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(
        healthy_cov["completeness_state"],
        "complete_for_observed_view"
    );
    let missing_views: Vec<String> = healthy_cov["missing_views"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    assert!(missing_views.contains(&"LocRib".to_string()));
    assert!(missing_views.contains(&"AdjRibInPostPolicy".to_string()));
}

#[tokio::test]
async fn a07_a09_b22_prefix_investigator_three_held_out_diagnostic_cases() {
    let state = Arc::new(
        RoutingServiceState::new(RoutingServiceConfig::default())
            .await
            .unwrap(),
    );
    let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));

    // Ingest Initiation + AdjRibInPrePolicy PeerUp + Announce 203.0.113.0/24 + EOR
    let (init, peer_up, rm_203) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113], 100);
    state.ingest_bmp_bytes(speaker, Some(1), &init).await.unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &peer_up)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &rm_203)
        .await
        .unwrap();
    let eor_pre = build_eor_frame(0, 0, [0u8; 8], Ipv4Addr::new(10, 0, 0, 1), 65001);
    state
        .ingest_bmp_bytes(speaker, Some(1), &eor_pre)
        .await
        .unwrap();

    // ── Case C: Only AdjRibInPrePolicy observed -> must abstain on Loc-RIB/FIB reachability! ──
    let (_, case_c) = build_explain_prefix_json(
        &state,
        &ExplainPrefixQuery {
            question: Some(
                "The BGP session is up. Why can this service not reach prefix 203.0.113.0/24 in VRF 0:0?"
                    .to_string(),
            ),
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            speaker: Some("192.0.2.10".to_string()),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(case_c["status"], "partial");
    assert_eq!(case_c["epistemic_state"], "insufficient_evidence");
    assert_eq!(case_c["hypotheses"][0]["category"], "insufficient_evidence");
    // Peer session down must be explicitly contradicted with counterevidence IDs
    let peer_down_hyp = case_c["hypotheses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["category"] == "peer_session_down")
        .expect("peer_session_down hypothesis evaluated");
    assert_eq!(peer_down_hyp["status"], "contradicted");
    assert!(!peer_down_hyp["counterevidence_ids"]
        .as_array()
        .unwrap()
        .is_empty());

    // ── Case A: Now synchronize Loc-RIB (RFC 9069 peer_type=3) on 192.0.2.10 with 198.51.100.0/24
    //            and EOR, leaving 203.0.113.0/24 absent from Loc-RIB! ──
    let (loc_peer_up, loc_rm_198) =
        build_loc_rib_peer_up_and_route(Ipv4Addr::new(192, 0, 2, 10), 65000, [198, 51, 100], 200);
    let eor_loc = build_eor_frame(3, 0, [0u8; 8], Ipv4Addr::new(192, 0, 2, 10), 65000);
    state
        .ingest_bmp_bytes(speaker, Some(1), &loc_peer_up)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &loc_rm_198)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &eor_loc)
        .await
        .unwrap();

    let (_, journey_a) = build_prefix_journey_json(
        &state,
        &PrefixJourneyQuery {
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            speaker: Some("192.0.2.10".to_string()),
            view: None,
            limit: Some(50),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(
        journey_a["views_with_active_prefix"],
        json!(["AdjRibInPrePolicy"])
    );
    assert!(journey_a["observed_views_without_prefix"]
        .as_array()
        .unwrap()
        .contains(&json!("LocRib")));

    let (_, case_a) = build_explain_prefix_json(
        &state,
        &ExplainPrefixQuery {
            question: None,
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            speaker: Some("192.0.2.10".to_string()),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(case_a["status"], "completed");
    assert_eq!(case_a["epistemic_state"], "supported");
    assert_eq!(
        case_a["root_cause_type"],
        "import_policy_rejected_before_loc_rib"
    );
    assert_eq!(
        case_a["hypotheses"][0]["category"],
        "import_policy_rejected_before_loc_rib"
    );
    assert_eq!(case_a["hypotheses"][0]["status"], "supported");
    assert!(!case_a["supporting_evidence_ids"]
        .as_array()
        .unwrap()
        .is_empty());

    // ── Case B: Collector BMP transport disconnect -> must report collector_transport_gap,
    //            never fabricate a BGP withdrawal! ──
    state
        .record_transport_disconnect(speaker, "bmp_tcp_eof")
        .await;
    let (_, case_b) = build_explain_prefix_json(
        &state,
        &ExplainPrefixQuery {
            question: None,
            prefix: Some("203.0.113.0/24".to_string()),
            vrf_rd: Some("0:0".to_string()),
            speaker: Some("192.0.2.10".to_string()),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(case_b["status"], "partial");
    assert_eq!(case_b["epistemic_state"], "insufficient_evidence");
    assert_eq!(case_b["root_cause_type"], "collector_transport_gap");
    assert_eq!(
        case_b["hypotheses"][0]["category"],
        "collector_transport_gap"
    );

    let timeline = build_peer_timeline_json(
        &state,
        &PeerTimelineQuery {
            speaker: Some("192.0.2.10".to_string()),
            peer: None,
            vrf_rd: Some("0:0".to_string()),
            limit: Some(50),
            tenant_id: Some("lab".to_string()),
        },
    )
    .await;
    assert_eq!(timeline["completeness_state"], "gap");
    assert_eq!(timeline["transport_gaps"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a07_compare_snapshots_and_a08_budget_reservation_and_deduplicator() {
    let state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();
    let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));

    let (init, peer_up, rm_lp100) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113], 100);
    state.ingest_bmp_bytes(speaker, Some(1), &init).await.unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &peer_up)
        .await
        .unwrap();
    state
        .ingest_bmp_bytes(speaker, Some(1), &rm_lp100)
        .await
        .unwrap();
    let seq_after_announce = state.event_journal.latest_seq().await;

    // Modify attribute: local_pref 100 -> 250
    let (_, _, rm_lp250) =
        sample_bmp_session_frames(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113], 250);
    state
        .ingest_bmp_bytes(speaker, Some(1), &rm_lp250)
        .await
        .unwrap();
    let seq_after_modify = state.event_journal.latest_seq().await;

    // Withdraw prefix
    let withdraw = sample_bmp_withdraw_frame(Ipv4Addr::new(10, 0, 0, 1), 65001, [203, 0, 113]);
    state
        .ingest_bmp_bytes(speaker, Some(1), &withdraw)
        .await
        .unwrap();
    let seq_after_withdraw = state.event_journal.latest_seq().await;

    // 1. Compare seq 0 -> seq_after_announce: 1 added route
    let diff_add = build_compare_snapshots_json(
        &state,
        &CompareSnapshotsQuery {
            before_seq: Some(0),
            after_seq: Some(seq_after_announce),
            speaker: Some("192.0.2.10".to_string()),
            vrf_rd: Some("0:0".to_string()),
            prefix: Some("203.0.113.0/24".to_string()),
            limit: Some(50),
        },
    )
    .await;
    assert_eq!(diff_add["added_routes"].as_array().unwrap().len(), 1);

    // 2. Compare seq_after_announce -> seq_after_modify: 1 modified route (local_pref 100 -> 250)
    let diff_mod = build_compare_snapshots_json(
        &state,
        &CompareSnapshotsQuery {
            before_seq: Some(seq_after_announce),
            after_seq: Some(seq_after_modify),
            speaker: Some("192.0.2.10".to_string()),
            vrf_rd: Some("0:0".to_string()),
            prefix: Some("203.0.113.0/24".to_string()),
            limit: Some(50),
        },
    )
    .await;
    assert_eq!(diff_mod["modified_routes"].as_array().unwrap().len(), 1);
    assert_eq!(diff_mod["modified_routes"][0]["before"]["local_pref"], 100);
    assert_eq!(diff_mod["modified_routes"][0]["after"]["local_pref"], 250);

    // 3. Compare seq_after_modify -> seq_after_withdraw: 1 removed route
    let diff_rem = build_compare_snapshots_json(
        &state,
        &CompareSnapshotsQuery {
            before_seq: Some(seq_after_modify),
            after_seq: Some(seq_after_withdraw),
            speaker: Some("192.0.2.10".to_string()),
            vrf_rd: Some("0:0".to_string()),
            prefix: Some("203.0.113.0/24".to_string()),
            limit: Some(50),
        },
    )
    .await;
    assert_eq!(diff_rem["removed_routes"].as_array().unwrap().len(), 1);

    // 4. Verify A08 pre-call BudgetReservationLedger
    let mut ledger = BudgetReservationLedger::new(0.80);
    // Reserve $0.15 for inv-1 (per-inv limit $0.50, daily limit $1.00) -> succeeds ($0.95 total)
    assert!(ledger
        .reserve("res-1", "inv-1", 0.15, 0.50, 1.00)
        .is_ok());
    // Concurrent reservation of $0.10 for inv-2 would push daily total to $1.05 > $1.00 -> blocked!
    let err = ledger
        .reserve("res-2", "inv-2", 0.10, 0.50, 1.00)
        .unwrap_err();
    assert!(matches!(err, LoopExitReason::DailyBudgetExceeded { .. }));
    // Commit res-1 at actual $0.12 -> daily committed is now $0.92, reserved is $0.00
    ledger.commit("res-1", 0.12).unwrap();
    assert!((ledger.daily_committed_usd() - 0.92).abs() < 1e-9);

    // 5. Verify A08 ToolCallDeduplicator and RepeatedToolCallDetected outcome
    let mut dedup = ToolCallDeduplicator::new();
    let args = json!({ "prefix": "203.0.113.0/24", "vrf_rd": "0:0" });
    assert!(!dedup.record_or_detect_repeat("get_prefix_journey", &args));
    assert!(dedup.record_or_detect_repeat("get_prefix_journey", &args));

    let rep_outcome = resolve_investigation_outcome(
        LoopExitReason::RepeatedToolCallDetected {
            tool_name: "get_prefix_journey".to_string(),
        },
        "",
    );
    assert_eq!(rep_outcome.state, InvestigationTerminalState::Partial);
    assert_eq!(rep_outcome.stop_reason, "repeated_tool_call_detected");
}

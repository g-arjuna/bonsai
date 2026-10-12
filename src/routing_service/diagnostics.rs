//! Slice P1 (A04–A09 + B22): Canonical observation envelope, incident-specific
//! evidence coverage, prefix journey, peer timeline, deterministic snapshot comparison,
//! and grounded read-only "Explain a Missing Prefix" investigator.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::{Arc, OnceLock, RwLock as StdRwLock};

use axum::http::StatusCode;
use chrono::Utc;
use rbmp_core::bgp::types::LlgrState;
use rbmp_core::bmp::types::{PeerKey, RibType};
use rbmp_rib::event::{RibEvent, RibEventPayload, RouteAction};
use rbmp_rib::session::PeerState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use super::{RoutingServiceState, TransportGapRecord, ROUTING_API_VERSION};

pub const OBSERVATION_SCHEMA_VERSION: u32 = 1;

static DEFAULT_ROUTING_SERVICE: OnceLock<StdRwLock<Option<Arc<RoutingServiceState>>>> =
    OnceLock::new();

/// Register an in-process `RoutingServiceState` so MCP tools (`call_tool`) can query
/// durable routing state directly without requiring a separate loopback socket.
pub fn register_default_routing_service(state: Arc<RoutingServiceState>) {
    let lock = DEFAULT_ROUTING_SERVICE.get_or_init(|| StdRwLock::new(None));
    if let Ok(mut guard) = lock.write() {
        *guard = Some(state);
    }
}

/// Retrieve the currently registered in-process `RoutingServiceState`, if any.
pub fn default_routing_service() -> Option<Arc<RoutingServiceState>> {
    let lock = DEFAULT_ROUTING_SERVICE.get_or_init(|| StdRwLock::new(None));
    lock.read().ok().and_then(|g| g.clone())
}

// ── A05: Canonical Observation Envelope & Lineage ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationSourceRef {
    pub kind: String,
    pub collector_id: String,
    pub session_id: String,
    pub speaker_addr: String,
    pub session_epoch: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationSourceSemantics {
    pub view: String,
    pub direction: String,
    pub vrf_rd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationParserMeta {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationQuality {
    pub completeness: String,
    pub missing_fields: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "payload_type", rename_all = "snake_case")]
pub enum ObservationPayload {
    BmpRouteChange {
        action: String,
        prefix: String,
        path_id: Option<u32>,
        peer_addr: String,
        peer_as: u32,
        as_path: Option<String>,
        next_hop: Option<String>,
        local_pref: Option<u32>,
        med: Option<u32>,
        origin: Option<String>,
        communities: Vec<String>,
    },
    BmpPeerTransition {
        state: String,
        peer_addr: String,
        peer_as: u32,
        vrf_rd: String,
        reason: Option<String>,
    },
    BmpEndOfRib {
        peer_addr: String,
        peer_as: u32,
        vrf_rd: String,
        afi_safi: String,
    },
    BmpSpeakerTransition {
        state: String,
        sys_name: Option<String>,
        sys_descr: Option<String>,
        reason: Option<String>,
    },
    TransportDisconnectGap {
        speaker_addr: String,
        reason: String,
    },
}

/// Versioned canonical observation envelope shared across BMP, gNMI, syslog, and config snapshots (Strategy §4, A05).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationEnvelope {
    pub schema_version: u32,
    pub observation_id: String,
    pub tenant_id: String,
    pub entity_refs: Vec<String>,
    pub event_time: Option<String>,
    pub received_time: String,
    pub clock_quality: String,
    pub source: ObservationSourceRef,
    pub source_semantics: ObservationSourceSemantics,
    pub parser: ObservationParserMeta,
    pub raw_ref: String,
    pub payload: ObservationPayload,
    pub quality: ObservationQuality,
}

fn rib_type_direction(rt: RibType) -> &'static str {
    match rt {
        RibType::AdjRibInPrePolicy | RibType::AdjRibInPostPolicy => "inbound",
        RibType::LocRib | RibType::LocRibFiltered => "local",
        RibType::AdjRibOutPrePolicy | RibType::AdjRibOutPostPolicy => "outbound",
    }
}

impl ObservationEnvelope {
    /// Convert a durable `RibEvent` into a versioned `ObservationEnvelope` preserving
    /// exporter (`ObservationSource`), instance/VRF (`PeerKey`), view (`ViewKey`),
    /// path (`path_id`), and session epoch identity.
    pub fn from_rib_event(ev: &RibEvent, tenant_id: &str, has_transport_gap: bool) -> Self {
        let seq = ev.event_key.sequence;
        let epoch = ev.event_key.source.session_epoch;
        let collector_id = ev.event_key.source.collector_id.clone();
        let speaker_str = ev.speaker.to_string();
        let obs_id = format!("obs-bmp-seq-{seq}-{}", ev.id);

        let (entity_refs, event_time, clock_quality, semantics, payload, missing_fields) =
            match &ev.payload {
                RibEventPayload::RouteChange(rc) => {
                    let vrf_rd = rc.peer_key.distinguisher_string();
                    let peer_str = rc.peer_header.peer_address.to_string();
                    let view_str = format!("{:?}", rc.rib_type);
                    let refs = vec![
                        format!("device:{speaker_str}"),
                        format!("vrf:{vrf_rd}"),
                        format!("peer:{speaker_str}|{vrf_rd}|{peer_str}"),
                        format!("prefix:{}", rc.prefix),
                    ];
                    let attrs = rc.attributes.as_ref();
                    let mut missing = Vec::new();
                    if rc.action == RouteAction::Announce {
                        if attrs.and_then(|a| a.next_hop).is_none() {
                            missing.push("next_hop".to_string());
                        }
                        if attrs.and_then(|a| a.as_path.as_ref()).is_none() {
                            missing.push("as_path".to_string());
                        }
                    }
                    let src_ts = rc.peer_header.timestamp;
                    let (ev_time, clk) = if src_ts.timestamp() > 0 {
                        (Some(src_ts.to_rfc3339()), "synchronized".to_string())
                    } else {
                        (None, "unknown".to_string())
                    };
                    (
                        refs,
                        ev_time,
                        clk,
                        ObservationSourceSemantics {
                            view: view_str,
                            direction: rib_type_direction(rc.rib_type).to_string(),
                            vrf_rd,
                        },
                        ObservationPayload::BmpRouteChange {
                            action: match rc.action {
                                RouteAction::Announce => "announce".to_string(),
                                RouteAction::Withdraw => "withdraw".to_string(),
                            },
                            prefix: rc.prefix.to_string(),
                            path_id: rc.path_id,
                            peer_addr: peer_str,
                            peer_as: rc.peer_header.peer_as,
                            as_path: attrs
                                .and_then(|a| a.as_path.as_ref())
                                .map(|p| p.to_string()),
                            next_hop: attrs.and_then(|a| a.next_hop).map(|nh| nh.to_string()),
                            local_pref: attrs.and_then(|a| a.local_pref),
                            med: attrs.and_then(|a| a.multi_exit_disc),
                            origin: attrs.and_then(|a| a.origin).map(|o| o.to_string()),
                            communities: attrs
                                .map(|a| a.communities.iter().map(|c| c.to_string()).collect())
                                .unwrap_or_default(),
                        },
                        missing,
                    )
                }
                RibEventPayload::PeerUp {
                    peer_header,
                    remote_asn,
                    ..
                } => {
                    let pk = PeerKey::from_header(ev.speaker, peer_header);
                    let vrf_rd = pk.distinguisher_string();
                    let peer_str = peer_header.peer_address.to_string();
                    let view_str = format!("{:?}", peer_header.rib_type);
                    (
                        vec![
                            format!("device:{speaker_str}"),
                            format!("vrf:{vrf_rd}"),
                            format!("peer:{speaker_str}|{vrf_rd}|{peer_str}"),
                        ],
                        Some(peer_header.timestamp.to_rfc3339()),
                        "synchronized".to_string(),
                        ObservationSourceSemantics {
                            view: view_str,
                            direction: rib_type_direction(peer_header.rib_type).to_string(),
                            vrf_rd: vrf_rd.clone(),
                        },
                        ObservationPayload::BmpPeerTransition {
                            state: "up".to_string(),
                            peer_addr: peer_str,
                            peer_as: *remote_asn,
                            vrf_rd,
                            reason: None,
                        },
                        Vec::new(),
                    )
                }
                RibEventPayload::PeerDown {
                    peer_header,
                    reason,
                } => {
                    let pk = PeerKey::from_header(ev.speaker, peer_header);
                    let vrf_rd = pk.distinguisher_string();
                    let peer_str = peer_header.peer_address.to_string();
                    let view_str = format!("{:?}", peer_header.rib_type);
                    (
                        vec![
                            format!("device:{speaker_str}"),
                            format!("vrf:{vrf_rd}"),
                            format!("peer:{speaker_str}|{vrf_rd}|{peer_str}"),
                        ],
                        Some(peer_header.timestamp.to_rfc3339()),
                        "synchronized".to_string(),
                        ObservationSourceSemantics {
                            view: view_str,
                            direction: rib_type_direction(peer_header.rib_type).to_string(),
                            vrf_rd: vrf_rd.clone(),
                        },
                        ObservationPayload::BmpPeerTransition {
                            state: "down".to_string(),
                            peer_addr: peer_str,
                            peer_as: peer_header.peer_as,
                            vrf_rd,
                            reason: Some(reason.clone()),
                        },
                        Vec::new(),
                    )
                }
                RibEventPayload::EndOfRib {
                    peer_header,
                    afi_safi,
                } => {
                    let pk = PeerKey::from_header(ev.speaker, peer_header);
                    let vrf_rd = pk.distinguisher_string();
                    let peer_str = peer_header.peer_address.to_string();
                    let view_str = format!("{:?}", peer_header.rib_type);
                    (
                        vec![
                            format!("device:{speaker_str}"),
                            format!("vrf:{vrf_rd}"),
                            format!("peer:{speaker_str}|{vrf_rd}|{peer_str}"),
                        ],
                        Some(peer_header.timestamp.to_rfc3339()),
                        "synchronized".to_string(),
                        ObservationSourceSemantics {
                            view: view_str,
                            direction: rib_type_direction(peer_header.rib_type).to_string(),
                            vrf_rd: vrf_rd.clone(),
                        },
                        ObservationPayload::BmpEndOfRib {
                            peer_addr: peer_str,
                            peer_as: peer_header.peer_as,
                            vrf_rd,
                            afi_safi: afi_safi.clone(),
                        },
                        Vec::new(),
                    )
                }
                RibEventPayload::SpeakerUp {
                    sys_name,
                    sys_descr,
                } => (
                    vec![format!("device:{speaker_str}")],
                    None,
                    "unknown".to_string(),
                    ObservationSourceSemantics {
                        view: "session".to_string(),
                        direction: "control".to_string(),
                        vrf_rd: "0:0".to_string(),
                    },
                    ObservationPayload::BmpSpeakerTransition {
                        state: "up".to_string(),
                        sys_name: sys_name.clone(),
                        sys_descr: sys_descr.clone(),
                        reason: None,
                    },
                    Vec::new(),
                ),
                RibEventPayload::SpeakerDown { reason } => (
                    vec![format!("device:{speaker_str}")],
                    None,
                    "unknown".to_string(),
                    ObservationSourceSemantics {
                        view: "session".to_string(),
                        direction: "control".to_string(),
                        vrf_rd: "0:0".to_string(),
                    },
                    ObservationPayload::BmpSpeakerTransition {
                        state: "down".to_string(),
                        sys_name: None,
                        sys_descr: None,
                        reason: Some(reason.clone()),
                    },
                    Vec::new(),
                ),
                RibEventPayload::Stats { peer_header, .. } => {
                    let pk = PeerKey::from_header(ev.speaker, peer_header);
                    let vrf_rd = pk.distinguisher_string();
                    (
                        vec![format!("device:{speaker_str}"), format!("vrf:{vrf_rd}")],
                        Some(peer_header.timestamp.to_rfc3339()),
                        "synchronized".to_string(),
                        ObservationSourceSemantics {
                            view: format!("{:?}", peer_header.rib_type),
                            direction: rib_type_direction(peer_header.rib_type).to_string(),
                            vrf_rd,
                        },
                        ObservationPayload::BmpSpeakerTransition {
                            state: "stats".to_string(),
                            sys_name: None,
                            sys_descr: None,
                            reason: None,
                        },
                        Vec::new(),
                    )
                }
            };

        let completeness = if has_transport_gap {
            "gap"
        } else if !missing_fields.is_empty() {
            "partial"
        } else {
            "complete_for_observed_view"
        };

        ObservationEnvelope {
            schema_version: OBSERVATION_SCHEMA_VERSION,
            observation_id: obs_id,
            tenant_id: tenant_id.to_string(),
            entity_refs,
            event_time,
            received_time: ev.occurred_at.to_rfc3339(),
            clock_quality,
            source: ObservationSourceRef {
                kind: "bmp".to_string(),
                collector_id: collector_id.clone(),
                session_id: format!("{collector_id}:{speaker_str}:epoch-{epoch}"),
                speaker_addr: speaker_str,
                session_epoch: epoch,
                sequence: seq,
            },
            source_semantics: semantics,
            parser: ObservationParserMeta {
                name: "rbmp-core/bmp".to_string(),
                version: "0.2.0".to_string(),
            },
            raw_ref: format!("rib-event-journal://seq/{seq}/{}", ev.id),
            payload,
            quality: ObservationQuality {
                completeness: completeness.to_string(),
                missing_fields,
                reason: if has_transport_gap {
                    Some("active_bmp_transport_gap_on_speaker".to_string())
                } else {
                    None
                },
            },
        }
    }
}

// ── A06: Incident-Specific Evidence Coverage (`get_evidence_coverage`) ─────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EvidenceCoverageQuery {
    pub speaker: Option<String>,
    pub prefix: Option<String>,
    pub vrf_rd: Option<String>,
    pub tenant_id: Option<String>,
}

const ALL_BMP_VIEWS: &[RibType] = &[
    RibType::AdjRibInPrePolicy,
    RibType::AdjRibInPostPolicy,
    RibType::LocRib,
    RibType::LocRibFiltered,
    RibType::AdjRibOutPrePolicy,
    RibType::AdjRibOutPostPolicy,
];

/// Evaluate incident-scoped evidence coverage so a healthy unrelated router
/// never masks missing BMP/view/EOR/transport-gap evidence on the target speaker/VRF (A06).
pub async fn build_evidence_coverage_json(
    state: &RoutingServiceState,
    q: &EvidenceCoverageQuery,
) -> JsonValue {
    let latest_seq = state.event_journal.latest_seq().await;
    let overall = state.overall_completeness().await;
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;
    let events = state.event_journal.events_after_seq(0, 5_000).await;

    let target_speaker_ip: Option<IpAddr> = q.speaker.as_deref().and_then(|s| s.parse().ok());
    let mut target_speaker_found = false;
    let mut observed_views = BTreeSet::new();
    let mut eor_views = BTreeSet::new();
    let mut active_peers = 0usize;
    let mut down_peers = 0usize;
    let mut matching_transport_gap: Option<TransportGapRecord> = None;

    for spk in rib.speakers() {
        if let Some(target_ip) = target_speaker_ip {
            if spk.speaker_addr != target_ip {
                continue;
            }
        } else if let Some(ref spk_str) = q.speaker {
            if spk.speaker_addr.to_string() != *spk_str {
                continue;
            }
        }
        target_speaker_found = true;
        if let Some(g) = gaps.get(&spk.speaker_addr) {
            matching_transport_gap = Some(g.clone());
        }
        for (peer_key, peer_sess) in &spk.peers {
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            match peer_sess.state {
                PeerState::Up => active_peers += 1,
                PeerState::Down | PeerState::Unknown => down_peers += 1,
            }
            for (rt, _) in &peer_sess.eor_families {
                let view_name = format!("{:?}", rt);
                observed_views.insert(view_name.clone());
                eor_views.insert(view_name);
            }
        }
        for (peer_key, table) in rib.ribs_for_speaker(spk.speaker_addr) {
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            for rt in ALL_BMP_VIEWS {
                if table.iter_rib(*rt).next().is_some() {
                    observed_views.insert(format!("{:?}", rt));
                }
            }
        }
    }

    // Collect observation IDs from durable journal matching this incident scope
    let mut observation_ids = Vec::new();
    for ev in &events {
        if let Some(ref spk_filter) = q.speaker {
            if ev.speaker.to_string() != *spk_filter {
                continue;
            }
        }
        match &ev.payload {
            RibEventPayload::RouteChange(rc) => {
                if let Some(ref rd_filter) = q.vrf_rd {
                    if rc.peer_key.distinguisher_string() != *rd_filter {
                        continue;
                    }
                }
                observed_views.insert(format!("{:?}", rc.rib_type));
                if let Some(ref pfx_filter) = q.prefix {
                    if rc.prefix.to_string() != *pfx_filter {
                        continue;
                    }
                }
                observation_ids.push(format!("obs-bmp-seq-{}-{}", ev.event_key.sequence, ev.id));
            }
            RibEventPayload::EndOfRib { peer_header, .. }
            | RibEventPayload::PeerUp { peer_header, .. }
            | RibEventPayload::PeerDown { peer_header, .. } => {
                let pk = PeerKey::from_header(ev.speaker, peer_header);
                if let Some(ref rd_filter) = q.vrf_rd {
                    if pk.distinguisher_string() != *rd_filter {
                        continue;
                    }
                }
                observed_views.insert(format!("{:?}", peer_header.rib_type));
                observation_ids.push(format!("obs-bmp-seq-{}-{}", ev.event_key.sequence, ev.id));
            }
            _ => {}
        }
    }

    let missing_views: Vec<String> = ALL_BMP_VIEWS
        .iter()
        .map(|rt| format!("{:?}", rt))
        .filter(|v| !observed_views.contains(v))
        .collect();

    let scoped_completeness = if q.speaker.is_some() && !target_speaker_found {
        "missing_speaker"
    } else if matching_transport_gap.is_some() || down_peers > 0 {
        "gap"
    } else if active_peers == 0 || eor_views.is_empty() {
        "syncing"
    } else {
        "complete_for_observed_view"
    };

    let has_pre_policy = observed_views.contains("AdjRibInPrePolicy");
    let has_post_or_loc = observed_views.contains("AdjRibInPostPolicy")
        || observed_views.contains("LocRib")
        || observed_views.contains("LocRibFiltered");
    let has_adj_out = observed_views.contains("AdjRibOutPrePolicy")
        || observed_views.contains("AdjRibOutPostPolicy");

    let mut answerable = Vec::new();
    let mut unanswerable = Vec::new();

    if scoped_completeness == "complete_for_observed_view" && has_pre_policy {
        answerable.push("inbound_peer_advertisement_visibility".to_string());
    } else {
        unanswerable.push("inbound_peer_advertisement_visibility".to_string());
    }

    if scoped_completeness == "complete_for_observed_view" && has_pre_policy && has_post_or_loc {
        answerable.push("import_policy_and_loc_rib_selection".to_string());
    } else {
        unanswerable.push("import_policy_and_loc_rib_selection".to_string());
    }

    if scoped_completeness == "complete_for_observed_view" && has_adj_out {
        answerable.push("export_policy_and_adj_rib_out_visibility".to_string());
    } else {
        unanswerable.push("export_policy_and_adj_rib_out_visibility".to_string());
    }

    // BMP alone never proves FIB installation or data-plane packet forwarding (Strategy §6)
    unanswerable.push("fib_hardware_programming_and_packet_forwarding".to_string());

    json!({
        "api_version": ROUTING_API_VERSION,
        "as_of": Utc::now().to_rfc3339(),
        "durable_sequence": latest_seq,
        "scope": {
            "speaker": q.speaker,
            "vrf_rd": q.vrf_rd,
            "prefix": q.prefix,
            "tenant_id": q.tenant_id.clone().unwrap_or_else(|| "default".to_string()),
        },
        "target_speaker_found": target_speaker_found,
        "completeness_state": scoped_completeness,
        "global_completeness_state": overall,
        "active_peers_in_scope": active_peers,
        "down_peers_in_scope": down_peers,
        "transport_gap": matching_transport_gap,
        "observed_views": observed_views.into_iter().collect::<Vec<_>>(),
        "eor_views": eor_views.into_iter().collect::<Vec<_>>(),
        "missing_views": missing_views,
        "fib_forwarding_evidence_available": false,
        "answerable_questions": answerable,
        "unanswerable_questions": unanswerable,
        "observation_ids": observation_ids,
    })
}

// ── A07: Prefix Journey (`get_prefix_journey`) ────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PrefixJourneyQuery {
    pub prefix: Option<String>,
    pub vrf_rd: Option<String>,
    pub speaker: Option<String>,
    pub view: Option<String>,
    pub limit: Option<usize>,
    pub tenant_id: Option<String>,
}

pub async fn build_prefix_journey_json(
    state: &RoutingServiceState,
    q: &PrefixJourneyQuery,
) -> (StatusCode, JsonValue) {
    let Some(ref prefix) = q.prefix else {
        return (
            StatusCode::BAD_REQUEST,
            json!({
                "error": "missing_prefix_parameter",
                "message": "Query parameter 'prefix' is required for /prefix-journey"
            }),
        );
    };

    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let tenant_id = q.tenant_id.as_deref().unwrap_or("default");
    let latest_seq = state.event_journal.latest_seq().await;

    let cov = build_evidence_coverage_json(
        state,
        &EvidenceCoverageQuery {
            speaker: q.speaker.clone(),
            prefix: Some(prefix.clone()),
            vrf_rd: q.vrf_rd.clone(),
            tenant_id: Some(tenant_id.to_string()),
        },
    )
    .await;

    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;

    let mut current_views_present = BTreeSet::new();
    let mut active_hops = Vec::new();

    for spk in rib.speakers() {
        if let Some(ref spk_filter) = q.speaker {
            if spk.speaker_addr.to_string() != *spk_filter {
                continue;
            }
        }
        let spk_gap = gaps.contains_key(&spk.speaker_addr);
        for (peer_key, table) in rib.ribs_for_speaker(spk.speaker_addr) {
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let peer_sess = spk.peers.get(peer_key);
            for rt in ALL_BMP_VIEWS {
                let view_str = format!("{:?}", rt);
                if let Some(ref view_filter) = q.view {
                    if !view_str.eq_ignore_ascii_case(view_filter) {
                        continue;
                    }
                }
                for entry in table.iter_rib(*rt) {
                    if entry.prefix.to_string() != *prefix {
                        continue;
                    }
                    current_views_present.insert(view_str.clone());
                    let afi_safi = entry.prefix.afi_safi();
                    let eor_done = peer_sess
                        .map(|ps| ps.has_eor(*rt, afi_safi))
                        .unwrap_or(false);
                    let completeness = if spk_gap {
                        "gap"
                    } else if entry.llgr_state != LlgrState::Normal {
                        "stale"
                    } else if eor_done {
                        "complete_for_observed_view"
                    } else {
                        "syncing"
                    };
                    active_hops.push(json!({
                        "speaker_addr": spk.speaker_addr,
                        "vrf_rd": peer_key.distinguisher_string(),
                        "peer_addr": entry.peer_addr,
                        "peer_as": entry.peer_as,
                        "view": view_str,
                        "direction": rib_type_direction(*rt),
                        "prefix": entry.prefix.to_string(),
                        "path_id": entry.path_id,
                        "is_best": entry.is_best,
                        "as_path": entry.attributes.as_path.as_ref().map(|p| p.to_string()),
                        "next_hop": entry.attributes.next_hop,
                        "local_pref": entry.attributes.local_pref,
                        "med": entry.attributes.multi_exit_disc,
                        "origin": entry.attributes.origin.map(|o| o.to_string()),
                        "communities": entry.attributes.communities.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                        "observation_time": entry.received_at,
                        "completeness_state": completeness,
                    }));
                }
            }
        }
    }
    drop(rib);

    // Reconstruct durable event history & observation envelopes for this prefix/VRF/speaker
    let all_events = state.event_journal.events_after_seq(0, 5_000).await;
    let mut envelopes = Vec::new();
    let mut observation_ids = Vec::new();
    let mut announce_count = 0usize;
    let mut withdraw_count = 0usize;
    let mut as_paths_seen = BTreeSet::new();
    let mut next_hops_seen = BTreeSet::new();
    let mut local_prefs_seen = BTreeSet::new();

    for ev in &all_events {
        if let Some(ref spk_filter) = q.speaker {
            if ev.speaker.to_string() != *spk_filter {
                continue;
            }
        }
        if let RibEventPayload::RouteChange(rc) = &ev.payload {
            if rc.prefix.to_string() != *prefix {
                continue;
            }
            if let Some(ref rd_filter) = q.vrf_rd {
                if rc.peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let view_str = format!("{:?}", rc.rib_type);
            if let Some(ref view_filter) = q.view {
                if !view_str.eq_ignore_ascii_case(view_filter) {
                    continue;
                }
            }
            match rc.action {
                RouteAction::Announce => {
                    announce_count += 1;
                    if let Some(ref attrs) = rc.attributes {
                        if let Some(ref ap) = attrs.as_path {
                            as_paths_seen.insert(ap.to_string());
                        }
                        if let Some(nh) = attrs.next_hop {
                            next_hops_seen.insert(nh.to_string());
                        }
                        if let Some(lp) = attrs.local_pref {
                            local_prefs_seen.insert(lp);
                        }
                    }
                }
                RouteAction::Withdraw => {
                    withdraw_count += 1;
                }
            }
            let has_gap = gaps.contains_key(&ev.speaker);
            let env = ObservationEnvelope::from_rib_event(ev, tenant_id, has_gap);
            observation_ids.push(env.observation_id.clone());
            envelopes.push(env);
        }
    }
    drop(gaps);

    let total_events_matching = envelopes.len();
    let truncated = total_events_matching > limit;
    if envelopes.len() > limit {
        let skip = envelopes.len() - limit;
        envelopes = envelopes.into_iter().skip(skip).collect();
    }

    let observed_views: Vec<String> = cov["observed_views"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let views_observed_without_prefix: Vec<String> = observed_views
        .iter()
        .filter(|v| !current_views_present.contains(*v))
        .cloned()
        .collect();

    (
        StatusCode::OK,
        json!({
            "api_version": ROUTING_API_VERSION,
            "as_of": Utc::now().to_rfc3339(),
            "durable_sequence": latest_seq,
            "scope": {
                "prefix": prefix,
                "vrf_rd": q.vrf_rd,
                "speaker": q.speaker,
                "view": q.view,
                "tenant_id": tenant_id,
            },
            "completeness_state": cov["completeness_state"],
            "transport_gap": cov["transport_gap"],
            "active_hops": active_hops,
            "views_with_active_prefix": current_views_present.into_iter().collect::<Vec<_>>(),
            "observed_views_without_prefix": views_observed_without_prefix,
            "unobserved_views": cov["missing_views"],
            "transition_summary": {
                "announce_count": announce_count,
                "withdraw_count": withdraw_count,
                "flap_count": announce_count.min(withdraw_count),
                "distinct_as_paths": as_paths_seen.into_iter().collect::<Vec<_>>(),
                "distinct_next_hops": next_hops_seen.into_iter().collect::<Vec<_>>(),
                "distinct_local_prefs": local_prefs_seen.into_iter().collect::<Vec<_>>(),
            },
            "observation_ids": observation_ids,
            "observations": envelopes,
            "total_observations": total_events_matching,
            "truncated": truncated,
        }),
    )
}

// ── A07: Peer Timeline (`get_peer_timeline`) ──────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PeerTimelineQuery {
    pub speaker: Option<String>,
    pub peer: Option<String>,
    pub vrf_rd: Option<String>,
    pub limit: Option<usize>,
    pub tenant_id: Option<String>,
}

pub async fn build_peer_timeline_json(
    state: &RoutingServiceState,
    q: &PeerTimelineQuery,
) -> JsonValue {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let tenant_id = q.tenant_id.as_deref().unwrap_or("default");
    let latest_seq = state.event_journal.latest_seq().await;
    let all_events = state.event_journal.events_after_seq(0, 5_000).await;
    let gaps = state.transport_gaps.read().await;

    let mut session_epochs = BTreeSet::new();
    let mut transitions = Vec::new();
    let mut observation_ids = Vec::new();
    let mut announce_count = 0usize;
    let mut withdraw_count = 0usize;

    for ev in &all_events {
        if let Some(ref spk_filter) = q.speaker {
            if ev.speaker.to_string() != *spk_filter {
                continue;
            }
        }
        session_epochs.insert(ev.event_key.source.session_epoch);
        let has_gap = gaps.contains_key(&ev.speaker);

        match &ev.payload {
            RibEventPayload::SpeakerUp { .. } | RibEventPayload::SpeakerDown { .. } => {
                let env = ObservationEnvelope::from_rib_event(ev, tenant_id, has_gap);
                observation_ids.push(env.observation_id.clone());
                transitions.push(env);
            }
            RibEventPayload::PeerUp { peer_header, .. }
            | RibEventPayload::PeerDown { peer_header, .. }
            | RibEventPayload::EndOfRib { peer_header, .. } => {
                if let Some(ref peer_filter) = q.peer {
                    if peer_header.peer_address.to_string() != *peer_filter {
                        continue;
                    }
                }
                let pk = PeerKey::from_header(ev.speaker, peer_header);
                if let Some(ref rd_filter) = q.vrf_rd {
                    if pk.distinguisher_string() != *rd_filter {
                        continue;
                    }
                }
                let env = ObservationEnvelope::from_rib_event(ev, tenant_id, has_gap);
                observation_ids.push(env.observation_id.clone());
                transitions.push(env);
            }
            RibEventPayload::RouteChange(rc) => {
                if let Some(ref peer_filter) = q.peer {
                    if rc.peer_header.peer_address.to_string() != *peer_filter {
                        continue;
                    }
                }
                if let Some(ref rd_filter) = q.vrf_rd {
                    if rc.peer_key.distinguisher_string() != *rd_filter {
                        continue;
                    }
                }
                match rc.action {
                    RouteAction::Announce => announce_count += 1,
                    RouteAction::Withdraw => withdraw_count += 1,
                }
            }
            RibEventPayload::Stats { .. } => {}
        }
    }

    let mut active_transport_gaps = Vec::new();
    for (spk_ip, gap) in gaps.iter() {
        if let Some(ref spk_filter) = q.speaker {
            if spk_ip.to_string() != *spk_filter {
                continue;
            }
        }
        active_transport_gaps.push(gap.clone());
    }
    drop(gaps);

    let total_transitions = transitions.len();
    let truncated = total_transitions > limit;
    if transitions.len() > limit {
        let skip = transitions.len() - limit;
        transitions = transitions.into_iter().skip(skip).collect();
    }

    let cov = build_evidence_coverage_json(
        state,
        &EvidenceCoverageQuery {
            speaker: q.speaker.clone(),
            prefix: None,
            vrf_rd: q.vrf_rd.clone(),
            tenant_id: Some(tenant_id.to_string()),
        },
    )
    .await;

    json!({
        "api_version": ROUTING_API_VERSION,
        "as_of": Utc::now().to_rfc3339(),
        "durable_sequence": latest_seq,
        "scope": {
            "speaker": q.speaker,
            "peer": q.peer,
            "vrf_rd": q.vrf_rd,
            "tenant_id": tenant_id,
        },
        "completeness_state": cov["completeness_state"],
        "session_epochs": session_epochs.into_iter().collect::<Vec<_>>(),
        "route_event_summary": {
            "announce_events": announce_count,
            "withdraw_events": withdraw_count,
        },
        "transport_gaps": active_transport_gaps,
        "transitions": transitions,
        "observation_ids": observation_ids,
        "total_transitions": total_transitions,
        "truncated": truncated,
    })
}

// ── A07: Deterministic Temporal Snapshot Comparison (`compare_snapshots`) ──────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CompareSnapshotsQuery {
    pub before_seq: Option<u64>,
    pub after_seq: Option<u64>,
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub prefix: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SnapshotRouteRecord {
    speaker_addr: String,
    vrf_rd: String,
    peer_addr: String,
    peer_as: u32,
    view: String,
    prefix: String,
    path_id: Option<u32>,
    as_path: Option<String>,
    next_hop: Option<String>,
    local_pref: Option<u32>,
    observation_id: String,
    sequence: u64,
}

pub async fn build_compare_snapshots_json(
    state: &RoutingServiceState,
    q: &CompareSnapshotsQuery,
) -> JsonValue {
    let latest_seq = state.event_journal.latest_seq().await;
    let before_seq = q.before_seq.unwrap_or(0);
    let after_seq = q.after_seq.unwrap_or(latest_seq);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);

    let all_events = state.event_journal.events_after_seq(0, 10_000).await;

    let build_state_at = |target_seq: u64| -> BTreeMap<String, SnapshotRouteRecord> {
        let mut table: BTreeMap<String, SnapshotRouteRecord> = BTreeMap::new();
        for ev in &all_events {
            let seq = ev.event_key.sequence;
            if seq > target_seq {
                break;
            }
            if let Some(ref spk_filter) = q.speaker {
                if ev.speaker.to_string() != *spk_filter {
                    continue;
                }
            }
            match &ev.payload {
                RibEventPayload::PeerDown { peer_header, .. } => {
                    let pk = PeerKey::from_header(ev.speaker, peer_header);
                    let vrf_rd = pk.distinguisher_string();
                    if let Some(ref rd_filter) = q.vrf_rd {
                        if vrf_rd != *rd_filter {
                            continue;
                        }
                    }
                    let peer_str = peer_header.peer_address.to_string();
                    let spk_str = ev.speaker.to_string();
                    table.retain(|_, v| {
                        !(v.speaker_addr == spk_str && v.vrf_rd == vrf_rd && v.peer_addr == peer_str)
                    });
                }
                RibEventPayload::RouteChange(rc) => {
                    let vrf_rd = rc.peer_key.distinguisher_string();
                    if let Some(ref rd_filter) = q.vrf_rd {
                        if vrf_rd != *rd_filter {
                            continue;
                        }
                    }
                    let pfx_str = rc.prefix.to_string();
                    if let Some(ref pfx_filter) = q.prefix {
                        if pfx_str != *pfx_filter {
                            continue;
                        }
                    }
                    let view_str = format!("{:?}", rc.rib_type);
                    let peer_str = rc.peer_header.peer_address.to_string();
                    let spk_str = ev.speaker.to_string();
                    let key = format!(
                        "{spk_str}|{vrf_rd}|{peer_str}|{view_str}|{pfx_str}|{:?}",
                        rc.path_id
                    );
                    let obs_id = format!("obs-bmp-seq-{seq}-{}", ev.id);
                    match rc.action {
                        RouteAction::Announce => {
                            let attrs = rc.attributes.as_ref();
                            table.insert(
                                key,
                                SnapshotRouteRecord {
                                    speaker_addr: spk_str,
                                    vrf_rd,
                                    peer_addr: peer_str,
                                    peer_as: rc.peer_header.peer_as,
                                    view: view_str,
                                    prefix: pfx_str,
                                    path_id: rc.path_id,
                                    as_path: attrs
                                        .and_then(|a| a.as_path.as_ref())
                                        .map(|p| p.to_string()),
                                    next_hop: attrs
                                        .and_then(|a| a.next_hop)
                                        .map(|nh| nh.to_string()),
                                    local_pref: attrs.and_then(|a| a.local_pref),
                                    observation_id: obs_id,
                                    sequence: seq,
                                },
                            );
                        }
                        RouteAction::Withdraw => {
                            table.remove(&key);
                        }
                    }
                }
                _ => {}
            }
        }
        table
    };

    let before_map = build_state_at(before_seq);
    let after_map = build_state_at(after_seq);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let mut observation_ids = BTreeSet::new();

    for (k, after_rec) in &after_map {
        match before_map.get(k) {
            None => {
                observation_ids.insert(after_rec.observation_id.clone());
                added.push(json!(after_rec));
            }
            Some(before_rec) => {
                if before_rec.as_path != after_rec.as_path
                    || before_rec.next_hop != after_rec.next_hop
                    || before_rec.local_pref != after_rec.local_pref
                {
                    observation_ids.insert(before_rec.observation_id.clone());
                    observation_ids.insert(after_rec.observation_id.clone());
                    modified.push(json!({
                        "route_key": k,
                        "before": before_rec,
                        "after": after_rec,
                    }));
                }
            }
        }
    }

    for (k, before_rec) in &before_map {
        if !after_map.contains_key(k) {
            observation_ids.insert(before_rec.observation_id.clone());
            removed.push(json!(before_rec));
        }
    }

    added.truncate(limit);
    removed.truncate(limit);
    modified.truncate(limit);

    json!({
        "api_version": ROUTING_API_VERSION,
        "as_of": Utc::now().to_rfc3339(),
        "durable_sequence": latest_seq,
        "before_seq": before_seq,
        "after_seq": after_seq,
        "scope": {
            "speaker": q.speaker,
            "vrf_rd": q.vrf_rd,
            "prefix": q.prefix,
        },
        "added_routes": added,
        "removed_routes": removed,
        "modified_routes": modified,
        "observation_ids": observation_ids.into_iter().collect::<Vec<_>>(),
    })
}

// ── A09 & B22: Grounded Read-Only Prefix Investigator (`explain_missing_prefix`) ──

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExplainPrefixQuery {
    pub question: Option<String>,
    pub prefix: Option<String>,
    pub vrf_rd: Option<String>,
    pub speaker: Option<String>,
    pub tenant_id: Option<String>,
}

/// First product feature (Strategy §5, §6 & Handoff B22):
/// Grounded read-only "Explain a Missing Prefix" investigator.
///
/// - Never fabricates a withdrawal on BMP collector transport disconnect.
/// - Never infers installed FIB or data-plane packet reachability solely from BMP.
/// - Cites exact `observation_id`s and `sequence` numbers for every fact, supporting
///   evidence, and counterevidence.
/// - Returns honest terminal `status` (`"completed"` vs `"partial"`) and `epistemic_state`
///   (`"supported"` vs `"insufficient_evidence"`).
pub async fn build_explain_prefix_json(
    state: &RoutingServiceState,
    q: &ExplainPrefixQuery,
) -> (StatusCode, JsonValue) {
    let Some(ref prefix) = q.prefix else {
        return (
            StatusCode::BAD_REQUEST,
            json!({
                "error": "missing_prefix_parameter",
                "message": "Query parameter 'prefix' is required for /explain-prefix"
            }),
        );
    };

    let vrf_rd = q.vrf_rd.clone().unwrap_or_else(|| "0:0".to_string());
    let tenant_id = q.tenant_id.clone().unwrap_or_else(|| "default".to_string());
    let question = q.question.clone().unwrap_or_else(|| {
        format!(
            "The BGP session is up. Why can this service not reach prefix {} in VRF {}?",
            prefix, vrf_rd
        )
    });

    let cov = build_evidence_coverage_json(
        state,
        &EvidenceCoverageQuery {
            speaker: q.speaker.clone(),
            prefix: Some(prefix.clone()),
            vrf_rd: Some(vrf_rd.clone()),
            tenant_id: Some(tenant_id.clone()),
        },
    )
    .await;

    let (_, journey) = build_prefix_journey_json(
        state,
        &PrefixJourneyQuery {
            prefix: Some(prefix.clone()),
            vrf_rd: Some(vrf_rd.clone()),
            speaker: q.speaker.clone(),
            view: None,
            limit: Some(50),
            tenant_id: Some(tenant_id.clone()),
        },
    )
    .await;

    let timeline = build_peer_timeline_json(
        state,
        &PeerTimelineQuery {
            speaker: q.speaker.clone(),
            peer: None,
            vrf_rd: Some(vrf_rd.clone()),
            limit: Some(50),
            tenant_id: Some(tenant_id.clone()),
        },
    )
    .await;

    let latest_seq = state.event_journal.latest_seq().await;
    let completeness_state = cov["completeness_state"]
        .as_str()
        .unwrap_or("syncing")
        .to_string();
    let has_transport_gap = !cov["transport_gap"].is_null();

    let views_with_prefix: Vec<String> = journey["views_with_active_prefix"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let observed_views: Vec<String> = cov["observed_views"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let unobserved_views: Vec<String> = cov["missing_views"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut observed_facts = Vec::new();
    let mut supporting_ids = BTreeSet::new();
    let mut counter_ids = BTreeSet::new();

    if let Some(obs_arr) = journey["observations"].as_array() {
        for obs in obs_arr {
            let obs_id = obs["observation_id"].as_str().unwrap_or("").to_string();
            let seq = obs["source"]["sequence"].as_u64().unwrap_or(0);
            let spk = obs["source"]["speaker_addr"].as_str().unwrap_or("");
            let view = obs["source_semantics"]["view"].as_str().unwrap_or("");
            let action = obs["payload"]["action"].as_str().unwrap_or("");
            let peer = obs["payload"]["peer_addr"].as_str().unwrap_or("");
            observed_facts.push(json!({
                "fact_id": format!("fact-route-seq-{seq}"),
                "observation_id": obs_id,
                "sequence": seq,
                "speaker_addr": spk,
                "vrf_rd": vrf_rd,
                "view": view,
                "peer_addr": peer,
                "summary": format!("BMP {} of {} in {} from peer {} on speaker {} (seq {})", action, prefix, view, peer, spk, seq),
            }));
        }
    }

    let mut peer_up_obs_ids = Vec::new();
    let mut peer_down_obs_ids = Vec::new();
    let mut loc_rib_obs_ids = Vec::new();
    let mut pre_policy_announce_ids = Vec::new();
    let mut withdraw_obs_ids = Vec::new();

    if let Some(trans_arr) = timeline["transitions"].as_array() {
        for tr in trans_arr {
            let obs_id = tr["observation_id"].as_str().unwrap_or("").to_string();
            let seq = tr["source"]["sequence"].as_u64().unwrap_or(0);
            let spk = tr["source"]["speaker_addr"].as_str().unwrap_or("");
            let view = tr["source_semantics"]["view"].as_str().unwrap_or("");
            let ptype = tr["payload"]["payload_type"].as_str().unwrap_or("");
            if ptype == "bmp_peer_transition" {
                let st = tr["payload"]["state"].as_str().unwrap_or("");
                let peer = tr["payload"]["peer_addr"].as_str().unwrap_or("");
                if st == "up" {
                    peer_up_obs_ids.push(obs_id.clone());
                } else if st == "down" {
                    peer_down_obs_ids.push(obs_id.clone());
                }
                observed_facts.push(json!({
                    "fact_id": format!("fact-peer-seq-{seq}"),
                    "observation_id": obs_id,
                    "sequence": seq,
                    "speaker_addr": spk,
                    "vrf_rd": vrf_rd,
                    "view": view,
                    "peer_addr": peer,
                    "summary": format!("Peer {} transitioned to {} in {} on speaker {} (seq {})", peer, st, view, spk, seq),
                }));
            } else if ptype == "bmp_end_of_rib" {
                if view == "LocRib" || view == "AdjRibInPostPolicy" || view == "LocRibFiltered" {
                    loc_rib_obs_ids.push(obs_id.clone());
                }
                observed_facts.push(json!({
                    "fact_id": format!("fact-eor-seq-{seq}"),
                    "observation_id": obs_id,
                    "sequence": seq,
                    "speaker_addr": spk,
                    "vrf_rd": vrf_rd,
                    "view": view,
                    "summary": format!("End-of-RIB received for {} on speaker {} (seq {})", view, spk, seq),
                }));
            }
        }
    }

    if let Some(obs_arr) = journey["observations"].as_array() {
        for obs in obs_arr {
            let obs_id = obs["observation_id"].as_str().unwrap_or("").to_string();
            let view = obs["source_semantics"]["view"].as_str().unwrap_or("");
            let action = obs["payload"]["action"].as_str().unwrap_or("");
            if view == "AdjRibInPrePolicy" && action == "announce" {
                pre_policy_announce_ids.push(obs_id);
            } else if action == "withdraw" {
                withdraw_obs_ids.push(obs_id);
            } else if view == "LocRibFiltered" && action == "announce" {
                loc_rib_obs_ids.push(obs_id);
            }
        }
    }

    // Also include any observation ID from cov if loc_rib_obs_ids is still empty but LocRib is observed
    if loc_rib_obs_ids.is_empty() {
        if let Some(cov_ids) = cov["observation_ids"].as_array() {
            for cid in cov_ids {
                if let Some(s) = cid.as_str() {
                    loc_rib_obs_ids.push(s.to_string());
                }
            }
        }
    }

    let in_pre_policy = views_with_prefix.iter().any(|v| v == "AdjRibInPrePolicy");
    let in_post_or_loc = views_with_prefix
        .iter()
        .any(|v| v == "AdjRibInPostPolicy" || v == "LocRib");
    let in_loc_filtered = views_with_prefix.iter().any(|v| v == "LocRibFiltered");
    let post_or_loc_observed = observed_views
        .iter()
        .any(|v| v == "AdjRibInPostPolicy" || v == "LocRib" || v == "LocRibFiltered");
    let down_peers = cov["down_peers_in_scope"].as_u64().unwrap_or(0);

    let mut missing_observations = vec![
        "fib_and_data_plane_forwarding_state (BMP observes control-plane RIB views only, never hardware FIB or packet forwarding)".to_string(),
    ];
    for uv in &unobserved_views {
        missing_observations.push(format!("bmp_view:{uv}"));
    }

    let mut hypotheses = Vec::new();
    let (status, epistemic_state, root_cause_type, next_check, conclusion);

    if has_transport_gap {
        // Case B: Collector transport disconnect gap — never fabricate BGP withdrawal!
        status = "partial";
        epistemic_state = "insufficient_evidence";
        root_cause_type = "collector_transport_gap";
        for id in &pre_policy_announce_ids {
            supporting_ids.insert(id.clone());
        }
        missing_observations
            .push("continuous_bmp_session (active collector transport disconnect gap)".to_string());
        hypotheses.push(json!({
            "hypothesis_id": "hyp-collector-transport-gap",
            "category": "collector_transport_gap",
            "rank": 1,
            "status": "supported",
            "rationale": "BMP collector transport disconnected without a BMP Termination or BGP PeerDown; route state during a transport gap is stale/unknown and must not be interpreted as a BGP withdrawal.",
            "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
            "counterevidence_ids": [],
        }));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-upstream-prefix-withdrawn",
            "category": "upstream_prefix_withdrawn",
            "rank": 2,
            "status": "insufficient_evidence",
            "rationale": "Cannot confirm or rule out BGP withdrawal while BMP collector transport is disconnected.",
            "supporting_evidence_ids": [],
            "counterevidence_ids": [],
        }));
        next_check = json!({
            "tool_or_command": "get_peer_timeline",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd },
            "read_only": true,
            "reason": "Verify BMP collector session recovery and End-of-RIB resynchronization before interpreting route absence.",
        });
        conclusion = format!(
            "Observation gap on speaker {:?}: BMP transport disconnected. A missing or stale route during a collector gap is not evidence of BGP withdrawal.",
            q.speaker
        );
    } else if completeness_state == "missing_speaker" || completeness_state == "syncing" {
        status = "partial";
        epistemic_state = "insufficient_evidence";
        root_cause_type = "insufficient_evidence";
        missing_observations.push(format!(
            "completed_bmp_eor_synchronization (current state: {completeness_state})"
        ));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-insufficient-telemetry-sync",
            "category": "insufficient_evidence",
            "rank": 1,
            "status": "insufficient_evidence",
            "rationale": format!("Target speaker/VRF evidence completeness is '{}' (missing End-of-RIB or speaker not connected).", completeness_state),
            "supporting_evidence_ids": [],
            "counterevidence_ids": [],
        }));
        next_check = json!({
            "tool_or_command": "get_evidence_coverage",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd, "prefix": prefix },
            "read_only": true,
            "reason": "Confirm BMP session establishment and End-of-RIB for the target router and VRF.",
        });
        conclusion = format!(
            "Insufficient routing evidence for prefix {} in VRF {} (completeness_state = '{}').",
            prefix, vrf_rd, completeness_state
        );
    } else if down_peers > 0 && views_with_prefix.is_empty() {
        // Peer session is down
        status = "completed";
        epistemic_state = "supported";
        root_cause_type = "peer_session_down";
        for id in &peer_down_obs_ids {
            supporting_ids.insert(id.clone());
        }
        hypotheses.push(json!({
            "hypothesis_id": "hyp-peer-session-down",
            "category": "peer_session_down",
            "rank": 1,
            "status": "supported",
            "rationale": "BGP peer session in the target VRF transitioned to Down, clearing routes from the peer.",
            "supporting_evidence_ids": peer_down_obs_ids,
            "counterevidence_ids": [],
        }));
        next_check = json!({
            "tool_or_command": "get_peer_timeline",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd },
            "read_only": true,
            "reason": "Inspect PeerDown reason code and correlate with interface/BFD state.",
        });
        conclusion = format!(
            "Prefix {} is absent in VRF {} because the advertising BGP peer session is Down.",
            prefix, vrf_rd
        );
    } else if in_pre_policy && post_or_loc_observed && (!in_post_or_loc || in_loc_filtered) {
        // Case A: Present in AdjRibInPrePolicy, absent in LocRib/AdjRibInPostPolicy (or in LocRibFiltered)
        status = "completed";
        epistemic_state = "supported";
        root_cause_type = "import_policy_rejected_before_loc_rib";
        for id in &pre_policy_announce_ids {
            supporting_ids.insert(id.clone());
            counter_ids.insert(id.clone());
        }
        for id in &loc_rib_obs_ids {
            supporting_ids.insert(id.clone());
        }
        for id in &peer_up_obs_ids {
            counter_ids.insert(id.clone());
        }
        hypotheses.push(json!({
            "hypothesis_id": "hyp-import-policy-rejected",
            "category": "import_policy_rejected_before_loc_rib",
            "rank": 1,
            "status": "supported",
            "rationale": format!(
                "Prefix {} is present in AdjRibInPrePolicy in VRF {}, and post-policy/Loc-RIB view is synchronized on the same speaker, but the prefix is absent from LocRib{}.",
                prefix,
                vrf_rd,
                if in_loc_filtered { " (and explicitly recorded in LocRibFiltered)" } else { "" }
            ),
            "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
            "counterevidence_ids": [],
        }));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-peer-session-down",
            "category": "peer_session_down",
            "rank": 2,
            "status": "contradicted",
            "rationale": "Peer session is Up and actively advertising the prefix in AdjRibInPrePolicy.",
            "supporting_evidence_ids": [],
            "counterevidence_ids": counter_ids.iter().cloned().collect::<Vec<_>>(),
        }));
        next_check = json!({
            "tool_or_command": "get_changes",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd, "prefix": prefix },
            "read_only": true,
            "reason": "Inspect inbound route-map / prefix-list changes or run a read-only policy check on the target router.",
        });
        conclusion = format!(
            "Prefix {} is received from the BGP peer in AdjRibInPrePolicy (VRF {}), but rejected or not selected before LocRib installation.",
            prefix, vrf_rd
        );
    } else if views_with_prefix.is_empty() && !withdraw_obs_ids.is_empty() {
        // Explicit BGP withdrawal observed
        status = "completed";
        epistemic_state = "supported";
        root_cause_type = "upstream_prefix_withdrawn";
        for id in &withdraw_obs_ids {
            supporting_ids.insert(id.clone());
        }
        for id in &pre_policy_announce_ids {
            supporting_ids.insert(id.clone());
        }
        for id in &peer_up_obs_ids {
            counter_ids.insert(id.clone());
        }
        hypotheses.push(json!({
            "hypothesis_id": "hyp-upstream-prefix-withdrawn",
            "category": "upstream_prefix_withdrawn",
            "rank": 1,
            "status": "supported",
            "rationale": "Peer session remains Up, and a BMP RouteMonitoring BGP UPDATE explicitly withdrew the prefix.",
            "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
            "counterevidence_ids": [],
        }));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-peer-session-down",
            "category": "peer_session_down",
            "rank": 2,
            "status": "contradicted",
            "rationale": "Peer session is Up; the route was removed by an explicit BGP UPDATE withdrawal rather than a session reset.",
            "supporting_evidence_ids": [],
            "counterevidence_ids": counter_ids.iter().cloned().collect::<Vec<_>>(),
        }));
        next_check = json!({
            "tool_or_command": "compare_snapshots",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd, "prefix": prefix },
            "read_only": true,
            "reason": "Compare RIB snapshots before and after the withdrawal sequence to inspect upstream path changes.",
        });
        conclusion = format!(
            "Prefix {} in VRF {} was explicitly withdrawn by the upstream BGP peer while the session remained Up.",
            prefix, vrf_rd
        );
    } else if in_pre_policy && !post_or_loc_observed {
        // Case C: Only AdjRibInPrePolicy is observed; LocRib/post-policy and FIB are not observed!
        status = "partial";
        epistemic_state = "insufficient_evidence";
        root_cause_type = "insufficient_evidence";
        for id in &pre_policy_announce_ids {
            supporting_ids.insert(id.clone());
            counter_ids.insert(id.clone());
        }
        for id in &peer_up_obs_ids {
            counter_ids.insert(id.clone());
        }
        hypotheses.push(json!({
            "hypothesis_id": "hyp-post-policy-or-forwarding-unobserved",
            "category": "insufficient_evidence",
            "rank": 1,
            "status": "insufficient_evidence",
            "rationale": "We see the route in AdjRibInPrePolicy, but lack post-policy (AdjRibInPostPolicy/LocRib) and FIB/forwarding evidence; Bonsai never infers FIB installation or packet reachability from pre-policy BMP alone.",
            "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
            "counterevidence_ids": [],
        }));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-import-policy-rejected",
            "category": "import_policy_rejected_before_loc_rib",
            "rank": 2,
            "status": "plausible",
            "rationale": "Inbound route policy could drop the prefix after AdjRibInPrePolicy, but post-policy/Loc-RIB BMP views are not exported by this speaker.",
            "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
            "counterevidence_ids": [],
        }));
        hypotheses.push(json!({
            "hypothesis_id": "hyp-peer-session-down",
            "category": "peer_session_down",
            "rank": 3,
            "status": "contradicted",
            "rationale": "Peer session is Up and advertising the prefix in AdjRibInPrePolicy.",
            "supporting_evidence_ids": [],
            "counterevidence_ids": counter_ids.iter().cloned().collect::<Vec<_>>(),
        }));
        next_check = json!({
            "tool_or_command": "run_readonly_check",
            "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd, "prefix": prefix, "check": "show_bgp_loc_rib_and_fib_route" },
            "read_only": true,
            "reason": "We see the route pre-policy but lack post-policy and forwarding evidence; query post-policy Loc-RIB and FIB next-hop state on the target router.",
        });
        conclusion = format!(
            "We see prefix {} in AdjRibInPrePolicy (VRF {}), but lack post-policy/Loc-RIB and FIB/forwarding evidence; next check is to inspect Loc-RIB and FIB state.",
            prefix, vrf_rd
        );
    } else {
        // Either prefix is present in LocRib too (so BMP control plane looks healthy, data plane / FIB is unobserved)
        // or prefix was never seen at all on a synchronized speaker.
        if in_post_or_loc {
            status = "partial";
            epistemic_state = "insufficient_evidence";
            root_cause_type = "insufficient_evidence";
            for id in &pre_policy_announce_ids {
                supporting_ids.insert(id.clone());
                counter_ids.insert(id.clone());
            }
            for id in &loc_rib_obs_ids {
                supporting_ids.insert(id.clone());
                counter_ids.insert(id.clone());
            }
            hypotheses.push(json!({
                "hypothesis_id": "hyp-fib-or-dataplane-unobserved",
                "category": "forwarding_or_next_hop_unverified",
                "rank": 1,
                "status": "insufficient_evidence",
                "rationale": "Prefix is present in both inbound BMP and Loc-RIB control-plane views, but BMP alone does not verify FIB programming, next-hop ARP/ND resolution, or data-plane ACLs.",
                "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
                "counterevidence_ids": [],
            }));
            hypotheses.push(json!({
                "hypothesis_id": "hyp-import-policy-rejected",
                "category": "import_policy_rejected_before_loc_rib",
                "rank": 2,
                "status": "contradicted",
                "rationale": "Prefix is actively installed in the observed Loc-RIB view.",
                "supporting_evidence_ids": [],
                "counterevidence_ids": counter_ids.iter().cloned().collect::<Vec<_>>(),
            }));
            next_check = json!({
                "tool_or_command": "run_readonly_check",
                "scope": { "speaker": q.speaker, "vrf_rd": vrf_rd, "prefix": prefix, "check": "verify_fib_and_next_hop_reachability" },
                "read_only": true,
                "reason": "Control-plane Loc-RIB has the route; check FIB installation and next-hop reachability.",
            });
            conclusion = format!(
                "Prefix {} is present in Loc-RIB (VRF {}), but FIB programming and data-plane reachability cannot be inferred from BMP alone.",
                prefix, vrf_rd
            );
        } else {
            status = "completed";
            epistemic_state = "supported";
            root_cause_type = "prefix_not_advertised_by_peer";
            if let Some(cov_ids) = cov["observation_ids"].as_array() {
                for cid in cov_ids {
                    if let Some(s) = cid.as_str() {
                        supporting_ids.insert(s.to_string());
                    }
                }
            }
            hypotheses.push(json!({
                "hypothesis_id": "hyp-prefix-not-advertised",
                "category": "export_policy_or_adj_rib_out_filtered",
                "rank": 1,
                "status": "supported",
                "rationale": "Peer session is Up and synchronized (End-of-RIB received), and no announcement for this prefix was ever received in AdjRibInPrePolicy.",
                "supporting_evidence_ids": supporting_ids.iter().cloned().collect::<Vec<_>>(),
                "counterevidence_ids": [],
            }));
            next_check = json!({
                "tool_or_command": "get_prefix_journey",
                "scope": { "prefix": prefix, "vrf_rd": vrf_rd },
                "read_only": true,
                "reason": "Check upstream exporter AdjRibOutPostPolicy views to see if upstream export policy filtered the prefix.",
            });
            conclusion = format!(
                "Prefix {} has not been advertised by any synchronized BGP peer in VRF {}.",
                prefix, vrf_rd
            );
        }
    }

    (
        StatusCode::OK,
        json!({
            "api_version": ROUTING_API_VERSION,
            "status": status,
            "epistemic_state": epistemic_state,
            "root_cause_type": root_cause_type,
            "confidence_is_uncalibrated": true,
            "question": question,
            "scope": {
                "prefix": prefix,
                "vrf_rd": vrf_rd,
                "speaker": q.speaker,
                "tenant_id": tenant_id,
            },
            "as_of": Utc::now().to_rfc3339(),
            "durable_sequence": latest_seq,
            "completeness_state": completeness_state,
            "conclusion": conclusion,
            "observed_facts": observed_facts,
            "hypotheses": hypotheses,
            "supporting_evidence_ids": supporting_ids.into_iter().collect::<Vec<_>>(),
            "counterevidence_ids": counter_ids.into_iter().collect::<Vec<_>>(),
            "missing_observations": missing_observations,
            "next_check": next_check,
            "impact": {
                "affected_prefix": prefix,
                "affected_vrf_rd": vrf_rd,
                "affected_speaker": q.speaker,
            },
            "recommended_plan_id": JsonValue::Null,
        }),
    )
}

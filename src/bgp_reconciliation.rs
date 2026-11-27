//! Bounded reconciliation of native polled BGP neighbor evidence with BMP
//! peer-session evidence. This is an operational comparison only: BMP absence
//! is visibility missingness, never proof that a native BGP session is down.

use serde::Serialize;

use crate::remediation::NativeNosBgpCapture;
use crate::semantic_evidence::{self, EvidenceFreshness, FreshnessPolicy};
use crate::typed_facts::Lifecycle;

#[derive(Clone, Debug)]
pub struct BmpPeerEvidencePage {
    pub rows: Vec<BmpPeerEvidence>,
    pub total_matching_rows: usize,
    pub returned_rows: usize,
    pub retrieval_omitted_rows: usize,
}

#[derive(Clone, Debug)]
pub struct BmpPeerEvidence {
    pub session_id: String,
    pub device_address: String,
    pub peer_address: String,
    pub peer_as: Option<u32>,
    pub session_state: String,
    pub network_instance: String,
    pub route_distinguisher: String,
    /// RFC 7854 BMP peer type. This is an instance/visibility axis, not the
    /// native BGP internal/external peer classification.
    pub peer_type: Option<u8>,
    /// Time of the BMP Peer Up/Down transition carried by the evidence. An old
    /// transition is historical evidence; it is neither a fresh liveness sample
    /// nor proof that the state stopped being valid.
    pub updated_at_ns: i64,
    /// Time Bonsai received/persisted this BMP evidence when known. Zero means
    /// legacy evidence for which receipt freshness is unavailable.
    pub evidence_received_at_ns: i64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct NativeBmpReconciliationPolicy {
    pub max_observation_age_seconds: u64,
    pub max_capture_skew_seconds: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeBmpAgreement {
    CorroboratedEstablished,
    CorroboratedNotEstablished,
    ConflictingSessionState,
    ConflictingPeerAs,
    MissingBmpVisibility,
    AmbiguousBmpEvidence,
    VrfMappingUnknown,
    PeerScopeMappingUnknown,
    StaleEvidence,
    ExcessiveCaptureSkew,
    IncompleteNativeEvidence,
    UnknownSessionState,
    HistoricalBmpTransitionEvidence,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeBmpSessionAssessment {
    pub vrf_name: String,
    pub afi_safi: String,
    pub peer_address: Option<String>,
    pub peer_scope_id: Option<String>,
    pub agreement: NativeBmpAgreement,
    pub native_session_state: Option<String>,
    pub bmp_session_state: Option<String>,
    pub bmp_session_id: Option<String>,
    pub native_peer_as: Option<u32>,
    pub bmp_peer_as: Option<u32>,
    pub native_freshness: String,
    pub bmp_freshness: Option<String>,
    pub bmp_peer_type: Option<u8>,
    /// Session corroboration never asserts AFI/SAFI negotiation or route-family
    /// operational state.
    pub corroboration_level: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeBmpReconciliation {
    pub state: String,
    pub policy: NativeBmpReconciliationPolicy,
    pub assessments: Vec<NativeBmpSessionAssessment>,
    pub corroborated_established: usize,
    pub corroborated_not_established: usize,
    pub conflicting: usize,
    pub missing_bmp_visibility: usize,
    pub ambiguous: usize,
    pub unknown_or_stale: usize,
    pub total_native_sessions: usize,
    pub assessed_sessions: usize,
    pub omitted_native_sessions: usize,
    pub bmp_rows_input: usize,
    pub bmp_rows_input_limit_omitted: usize,
    pub bmp_rows_indexed: usize,
    pub bmp_exact_replays: usize,
    pub bmp_candidates_omitted: usize,
    pub corroboration_level: String,
}

fn rfc3339_from_ns(ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(ns).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn freshness_label(value: EvidenceFreshness) -> &'static str {
    match value {
        EvidenceFreshness::Current => "current",
        EvidenceFreshness::Historical => "historical",
        EvidenceFreshness::Stale => "stale",
        EvidenceFreshness::Withdrawn => "withdrawn",
        EvidenceFreshness::Superseded => "superseded",
        EvidenceFreshness::FutureObservation => "future_observation",
        EvidenceFreshness::NotYetValid => "not_yet_valid",
        EvidenceFreshness::UnknownFreshness => "unknown_freshness",
    }
}

fn freshness(
    observed_at_ns: i64,
    as_of_ns: i64,
    policy: NativeBmpReconciliationPolicy,
) -> EvidenceFreshness {
    let observed = rfc3339_from_ns(observed_at_ns);
    let as_of = chrono::DateTime::from_timestamp_nanos(as_of_ns);
    semantic_evidence::observation_freshness(
        Lifecycle::Active,
        &observed,
        None,
        None,
        as_of.fixed_offset(),
        FreshnessPolicy::observation_age(policy.max_observation_age_seconds),
    )
}

const MAX_ASSESSMENTS: usize = 500;
const MAX_BMP_ROWS_INDEXED: usize = 1_000;
const MAX_BMP_ROWS_INPUT_SCAN: usize = 10_000;
const MAX_CANDIDATES_PER_PEER: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStateSemantics {
    Established,
    RecognizedNotEstablished,
    Unknown,
}

fn session_state_semantics(raw: Option<&str>) -> SessionStateSemantics {
    let Some(state) = raw.map(str::trim).filter(|state| !state.is_empty()) else {
        return SessionStateSemantics::Unknown;
    };
    match state.to_ascii_lowercase().as_str() {
        "established" => SessionStateSemantics::Established,
        "idle" | "connect" | "active" | "opensent" | "open-sent" | "openconfirm"
        | "open-confirm" | "down" | "terminated" | "stop" | "stopped" => {
            SessionStateSemantics::RecognizedNotEstablished
        }
        _ => SessionStateSemantics::Unknown,
    }
}

fn canonical_ip(value: &str) -> Option<String> {
    value
        .split_once('%')
        .map(|(address, _)| address)
        .unwrap_or(value)
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

fn same_peer(left: &str, right: &str) -> bool {
    match (canonical_ip(left), canonical_ip(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

fn is_ipv6_link_local(value: &str) -> bool {
    canonical_ip(value)
        .and_then(|address| address.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| match ip {
            std::net::IpAddr::V6(ip) => ip.segments()[0] & 0xffc0 == 0xfe80,
            std::net::IpAddr::V4(_) => false,
        })
}

fn vrf_candidates<'a>(
    rows: &'a [&'a BmpPeerEvidence],
    vrf: &str,
) -> Result<Vec<&'a BmpPeerEvidence>, NativeBmpAgreement> {
    let exact_instance: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| !row.network_instance.is_empty() && row.network_instance == vrf)
        .collect();
    if !exact_instance.is_empty() {
        return Ok(exact_instance);
    }
    if vrf != "default" {
        // A BMP route distinguisher is preserved, but there is no independent
        // inventory/NOS RD→VRF binding here. Never infer the tenant from RD
        // equality or address overlap.
        return Err(NativeBmpAgreement::VrfMappingUnknown);
    }
    Ok(rows
        .iter()
        .copied()
        .filter(|row| {
            row.peer_type == Some(0)
                && row.network_instance.is_empty()
                && (row.route_distinguisher.is_empty() || row.route_distinguisher == "0")
        })
        .collect())
}

pub fn reconcile_native_bgp_with_bmp(
    capture: &NativeNosBgpCapture,
    native_captured_at_ns: i64,
    bmp_rows: &[BmpPeerEvidence],
    as_of_ns: i64,
    policy: NativeBmpReconciliationPolicy,
) -> NativeBmpReconciliation {
    let native_freshness = freshness(native_captured_at_ns, as_of_ns, policy);
    let mut peer_index: std::collections::BTreeMap<String, Vec<&BmpPeerEvidence>> =
        std::collections::BTreeMap::new();
    let mut bmp_rows_indexed = 0usize;
    let mut bmp_exact_replays = 0usize;
    let mut bmp_candidates_omitted = 0usize;
    let mut seen_exact = std::collections::BTreeSet::new();
    let bmp_rows_input_limit_omitted = bmp_rows.len().saturating_sub(MAX_BMP_ROWS_INPUT_SCAN);
    let mut matching_rows_seen = 0usize;
    for row in bmp_rows
        .iter()
        .take(MAX_BMP_ROWS_INPUT_SCAN)
        .filter(|row| row.device_address == capture.exporter_address)
    {
        matching_rows_seen += 1;
        if matching_rows_seen > MAX_BMP_ROWS_INDEXED {
            bmp_candidates_omitted += 1;
            continue;
        }
        let exact_key = (
            row.session_id.as_str(),
            row.peer_address.as_str(),
            row.peer_as,
            row.session_state.as_str(),
            row.network_instance.as_str(),
            row.route_distinguisher.as_str(),
            row.peer_type,
            row.updated_at_ns,
            row.evidence_received_at_ns,
        );
        if !seen_exact.insert(exact_key) {
            bmp_exact_replays += 1;
            continue;
        }
        let key = canonical_ip(&row.peer_address).unwrap_or_else(|| row.peer_address.clone());
        let candidates = peer_index.entry(key).or_default();
        if candidates.len() < MAX_CANDIDATES_PER_PEER {
            candidates.push(row);
            bmp_rows_indexed += 1;
        } else {
            bmp_candidates_omitted += 1;
        }
    }
    for candidates in peer_index.values_mut() {
        candidates.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    }
    let total_native_sessions = capture.neighbors.len();
    let mut out = NativeBmpReconciliation {
        state: "available".into(),
        policy,
        assessments: Vec::new(),
        corroborated_established: 0,
        corroborated_not_established: 0,
        conflicting: 0,
        missing_bmp_visibility: 0,
        ambiguous: 0,
        unknown_or_stale: 0,
        total_native_sessions,
        assessed_sessions: total_native_sessions.min(MAX_ASSESSMENTS),
        omitted_native_sessions: total_native_sessions.saturating_sub(MAX_ASSESSMENTS),
        bmp_rows_input: bmp_rows.len(),
        bmp_rows_input_limit_omitted,
        bmp_rows_indexed,
        bmp_exact_replays,
        bmp_candidates_omitted,
        corroboration_level: "transport_session_only_not_afi_safi".into(),
    };

    for neighbor in capture.neighbors.iter().take(MAX_ASSESSMENTS) {
        let mut assessment = NativeBmpSessionAssessment {
            vrf_name: neighbor.vrf_name.clone(),
            afi_safi: neighbor.afi_safi.clone(),
            peer_address: neighbor.neighbor_address.clone(),
            peer_scope_id: neighbor.neighbor_scope_id.clone(),
            agreement: NativeBmpAgreement::IncompleteNativeEvidence,
            native_session_state: neighbor.session_state.clone(),
            bmp_session_state: None,
            bmp_session_id: None,
            native_peer_as: neighbor.peer_as,
            bmp_peer_as: None,
            native_freshness: freshness_label(native_freshness).to_string(),
            bmp_freshness: None,
            bmp_peer_type: None,
            corroboration_level: "transport_session_only_not_afi_safi".into(),
        };
        let Some(peer) = neighbor.neighbor_address.as_deref() else {
            out.unknown_or_stale += 1;
            out.assessments.push(assessment);
            continue;
        };
        if neighbor.session_state.is_none() {
            out.unknown_or_stale += 1;
            out.assessments.push(assessment);
            continue;
        }
        if !native_freshness.is_current() {
            assessment.agreement = NativeBmpAgreement::StaleEvidence;
            out.unknown_or_stale += 1;
            out.assessments.push(assessment);
            continue;
        }

        let key = canonical_ip(peer).unwrap_or_else(|| peer.to_string());
        let peer_rows: Vec<_> = peer_index
            .get(&key)
            .map(|rows| {
                rows.iter()
                    .copied()
                    .filter(|row| same_peer(&row.peer_address, peer))
                    .collect()
            })
            .unwrap_or_default();
        if peer_rows.is_empty() {
            assessment.agreement = NativeBmpAgreement::MissingBmpVisibility;
            out.missing_bmp_visibility += 1;
            out.assessments.push(assessment);
            continue;
        }
        if neighbor.neighbor_scope_id.is_some() || is_ipv6_link_local(peer) {
            // BMP peer evidence does not carry the interface scope required
            // to disambiguate link-local peers. Preserve the scoped native
            // identity, but do not infer which BMP row owns that scope — and
            // do not treat an unscoped link-local native address as globally
            // unique merely because the vendor omitted the zone.
            assessment.agreement = NativeBmpAgreement::PeerScopeMappingUnknown;
            out.unknown_or_stale += 1;
            out.assessments.push(assessment);
            continue;
        }
        let candidates = match vrf_candidates(&peer_rows, &neighbor.vrf_name) {
            Ok(rows) => rows,
            Err(agreement) => {
                assessment.agreement = agreement;
                out.unknown_or_stale += 1;
                out.assessments.push(assessment);
                continue;
            }
        };
        if candidates.is_empty() {
            assessment.agreement = NativeBmpAgreement::MissingBmpVisibility;
            out.missing_bmp_visibility += 1;
            out.assessments.push(assessment);
            continue;
        }
        if candidates.len() != 1 {
            assessment.agreement = NativeBmpAgreement::AmbiguousBmpEvidence;
            out.ambiguous += 1;
            out.assessments.push(assessment);
            continue;
        }
        let bmp = candidates[0];
        let bmp_freshness = if bmp.evidence_received_at_ns > 0 {
            freshness(bmp.evidence_received_at_ns, as_of_ns, policy)
        } else {
            EvidenceFreshness::UnknownFreshness
        };
        assessment.bmp_session_id = Some(bmp.session_id.clone());
        assessment.bmp_session_state = Some(bmp.session_state.clone());
        assessment.bmp_peer_as = bmp.peer_as;
        assessment.bmp_peer_type = bmp.peer_type;
        assessment.bmp_freshness = Some(freshness_label(bmp_freshness).to_string());
        if !bmp_freshness.is_current() {
            // BMP updated_at is commonly the last Peer Up/Down transition, not
            // a refreshed liveness sample. Age makes the transition historical;
            // it neither proves the session down nor refreshes the old state.
            assessment.agreement = NativeBmpAgreement::HistoricalBmpTransitionEvidence;
            out.unknown_or_stale += 1;
        } else if native_captured_at_ns.abs_diff(bmp.evidence_received_at_ns)
            > policy.max_capture_skew_seconds * 1_000_000_000
        {
            // Capture skew compares evidence acquisition/receipt times. The BMP
            // transition timestamp is deliberately not reused as a freshness or
            // capture-time proxy.
            assessment.agreement = NativeBmpAgreement::ExcessiveCaptureSkew;
            out.unknown_or_stale += 1;
        } else if let (Some(native_as), Some(bmp_as)) = (neighbor.peer_as, bmp.peer_as)
            && native_as != bmp_as
        {
            assessment.agreement = NativeBmpAgreement::ConflictingPeerAs;
            out.conflicting += 1;
        } else {
            let native_state = match neighbor.session_state_semantics.as_str() {
                "established" => SessionStateSemantics::Established,
                "recognized_not_established" => SessionStateSemantics::RecognizedNotEstablished,
                // Legacy rows predate the explicit field; derive conservatively
                // from the preserved raw label rather than assuming a meaning.
                "legacy_unknown" | "" => session_state_semantics(neighbor.session_state.as_deref()),
                _ => SessionStateSemantics::Unknown,
            };
            let bmp_state = session_state_semantics(Some(&bmp.session_state));
            assessment.agreement = match (native_state, bmp_state) {
                (SessionStateSemantics::Unknown, _) | (_, SessionStateSemantics::Unknown) => {
                    NativeBmpAgreement::UnknownSessionState
                }
                (SessionStateSemantics::Established, SessionStateSemantics::Established) => {
                    NativeBmpAgreement::CorroboratedEstablished
                }
                (
                    SessionStateSemantics::RecognizedNotEstablished,
                    SessionStateSemantics::RecognizedNotEstablished,
                ) => NativeBmpAgreement::CorroboratedNotEstablished,
                _ => NativeBmpAgreement::ConflictingSessionState,
            };
            match assessment.agreement {
                NativeBmpAgreement::CorroboratedEstablished => out.corroborated_established += 1,
                NativeBmpAgreement::CorroboratedNotEstablished => {
                    out.corroborated_not_established += 1
                }
                NativeBmpAgreement::ConflictingSessionState => out.conflicting += 1,
                NativeBmpAgreement::UnknownSessionState => out.unknown_or_stale += 1,
                _ => {}
            }
        }
        out.assessments.push(assessment);
    }
    let unresolved = out.missing_bmp_visibility > 0
        || out.ambiguous > 0
        || out.unknown_or_stale > 0
        || out.omitted_native_sessions > 0
        || capture.neighbors.is_empty();
    if out.conflicting > 0 {
        out.state = "conflicting_evidence".into();
    } else if unresolved
        && (out.corroborated_established > 0 || out.corroborated_not_established > 0)
    {
        out.state = "partially_unresolved".into();
    } else if unresolved {
        out.state = "unresolved_prerequisites".into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remediation::parse_nokia_srl_native_bgp_neighbors;

    const NATIVE: &str =
        include_str!("../tests/fixtures/bn006_srl_26_3_1/neighbors-established.json");
    const VERSION: &str = "v26.3.1-410-g98ed16c774c";
    const NOW: i64 = 1_769_000_000_000_000_000;

    fn capture() -> NativeNosBgpCapture {
        parse_nokia_srl_native_bgp_neighbors(NATIVE, "172.31.216.3", VERSION, "ipv4-unicast")
            .unwrap()
    }

    fn policy() -> NativeBmpReconciliationPolicy {
        NativeBmpReconciliationPolicy {
            max_observation_age_seconds: 300,
            max_capture_skew_seconds: 60,
        }
    }

    fn bmp(state: &str, peer_as: u32, updated_at_ns: i64) -> BmpPeerEvidence {
        BmpPeerEvidence {
            session_id: "bmp:peer".into(),
            device_address: "172.31.216.3".into(),
            peer_address: "10.206.0.2".into(),
            peer_as: Some(peer_as),
            session_state: state.into(),
            network_instance: String::new(),
            route_distinguisher: "0".into(),
            peer_type: Some(0),
            updated_at_ns,
            evidence_received_at_ns: updated_at_ns,
        }
    }

    #[test]
    fn established_native_and_bmp_peer_are_corroborated_not_fabricated() {
        let capture = capture();
        let rows = vec![bmp("established", 65002, NOW - 10_000_000_000)];
        let out =
            reconcile_native_bgp_with_bmp(&capture, NOW - 5_000_000_000, &rows, NOW, policy());
        assert_eq!(out.state, "available");
        assert_eq!(out.corroborated_established, 1);
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::CorroboratedEstablished
        );
    }

    #[test]
    fn missing_or_conflicting_bmp_never_means_session_down() {
        let capture = capture();
        let missing = reconcile_native_bgp_with_bmp(&capture, NOW, &[], NOW, policy());
        assert_eq!(missing.missing_bmp_visibility, 1);
        assert_eq!(missing.state, "unresolved_prerequisites");
        assert_eq!(
            missing.assessments[0].agreement,
            NativeBmpAgreement::MissingBmpVisibility
        );

        let rows = vec![bmp("idle", 65002, NOW)];
        let conflict = reconcile_native_bgp_with_bmp(&capture, NOW, &rows, NOW, policy());
        assert_eq!(conflict.conflicting, 1);
        assert_eq!(conflict.state, "conflicting_evidence");
        assert_eq!(
            conflict.assessments[0].agreement,
            NativeBmpAgreement::ConflictingSessionState
        );
    }

    #[test]
    fn interface_scoped_peer_is_not_matched_to_unscoped_bmp_identity() {
        let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
        raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0]["peer-address"] =
            serde_json::json!("fe80::2%ethernet-1/1");
        let capture = parse_nokia_srl_native_bgp_neighbors(
            &raw.to_string(),
            "172.31.216.3",
            VERSION,
            "ipv4-unicast",
        )
        .unwrap();
        let mut row = bmp("established", 65002, NOW);
        row.peer_address = "fe80::2".into();
        let out = reconcile_native_bgp_with_bmp(&capture, NOW, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::PeerScopeMappingUnknown
        );
        assert_eq!(
            out.assessments[0].peer_scope_id.as_deref(),
            Some("ethernet-1/1")
        );
    }

    #[test]
    fn unscoped_link_local_peer_is_not_globally_unique_identity() {
        let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
        raw["network-instance"][0]["protocols"]["bgp"]["neighbor"][0]["peer-address"] =
            serde_json::json!("fe80::2");
        let capture = parse_nokia_srl_native_bgp_neighbors(
            &raw.to_string(),
            "172.31.216.3",
            VERSION,
            "ipv4-unicast",
        )
        .unwrap();
        assert_eq!(capture.neighbors[0].neighbor_scope_id, None);
        let mut row = bmp("established", 65002, NOW);
        row.peer_address = "fe80::2".into();
        let out = reconcile_native_bgp_with_bmp(&capture, NOW, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::PeerScopeMappingUnknown
        );
    }

    #[test]
    fn named_vrf_rd_is_not_guessed_and_asn_conflict_is_explicit() {
        let mut raw: serde_json::Value = serde_json::from_str(NATIVE).unwrap();
        raw["network-instance"][0]["name"] = serde_json::json!("blue");
        let named = parse_nokia_srl_native_bgp_neighbors(
            &raw.to_string(),
            "172.31.216.3",
            VERSION,
            "ipv4-unicast",
        )
        .unwrap();
        let mut row = bmp("established", 65002, NOW);
        row.route_distinguisher = "429496729600".into();
        let out = reconcile_native_bgp_with_bmp(&named, NOW, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::VrfMappingUnknown
        );

        let rows = vec![bmp("established", 65003, NOW)];
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &rows, NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::ConflictingPeerAs
        );
    }

    #[test]
    fn unknown_session_state_never_becomes_corroborated_not_established() {
        let rows = vec![bmp("unrecognized-transition", 65002, NOW)];
        let mut capture = capture();
        capture.neighbors[0].session_state = Some("vendor-specific-unknown".into());
        let out = reconcile_native_bgp_with_bmp(&capture, NOW, &rows, NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::UnknownSessionState
        );
        assert_eq!(out.corroborated_not_established, 0);
        assert_eq!(out.unknown_or_stale, 1);
    }

    #[test]
    fn recognized_non_established_states_can_corroborate_only_as_non_established() {
        let rows = vec![bmp("down", 65002, NOW)];
        let mut capture = capture();
        capture.neighbors[0].session_state = Some("active".into());
        capture.neighbors[0].session_state_semantics = "recognized_not_established".into();
        let out = reconcile_native_bgp_with_bmp(&capture, NOW, &rows, NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::CorroboratedNotEstablished
        );
        assert_eq!(out.corroborated_not_established, 1);
        assert_eq!(
            out.assessments[0].corroboration_level,
            "transport_session_only_not_afi_safi"
        );
    }

    #[test]
    fn non_global_bmp_peer_type_does_not_match_default_native_vrf() {
        let mut row = bmp("established", 65002, NOW);
        row.peer_type = Some(1);
        row.route_distinguisher = "0".into();
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::MissingBmpVisibility
        );
    }

    #[test]
    fn old_transition_with_fresh_receipt_is_not_misread_as_stale_or_down() {
        let mut row = bmp(
            "established",
            65002,
            NOW - 30 * 24 * 60 * 60 * 1_000_000_000,
        );
        row.evidence_received_at_ns = NOW - 5_000_000_000;
        let out =
            reconcile_native_bgp_with_bmp(&capture(), NOW - 4_000_000_000, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::CorroboratedEstablished
        );
        assert_eq!(out.corroborated_established, 1);
    }

    #[test]
    fn legacy_bmp_row_without_receipt_time_has_unknown_freshness() {
        let mut row = bmp("established", 65002, NOW);
        row.evidence_received_at_ns = 0;
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &[row], NOW, policy());
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::HistoricalBmpTransitionEvidence
        );
        assert_eq!(
            out.assessments[0].bmp_freshness.as_deref(),
            Some("unknown_freshness")
        );
    }

    #[test]
    fn exact_bmp_replay_dedupes_but_conflicting_context_remains_ambiguous() {
        let row = bmp("established", 65002, NOW);
        let rows = vec![row.clone(), row];
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &rows, NOW, policy());
        assert_eq!(out.bmp_exact_replays, 1);
        assert_eq!(out.corroborated_established, 1);

        let first = bmp("established", 65002, NOW);
        let mut second = first.clone();
        second.session_id = "bmp:duplicate-global-context".into();
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &[first, second], NOW, policy());
        assert_eq!(out.bmp_exact_replays, 0);
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::AmbiguousBmpEvidence
        );
    }

    #[test]
    fn native_population_is_bounded_with_truthful_denominator() {
        let mut capture = capture();
        let one = capture.neighbors[0].clone();
        capture.neighbors = vec![one; 600];
        let out = reconcile_native_bgp_with_bmp(&capture, NOW, &[], NOW, policy());
        assert_eq!(out.total_native_sessions, 600);
        assert_eq!(out.assessed_sessions, 500);
        assert_eq!(out.omitted_native_sessions, 100);
        assert_eq!(out.assessments.len(), 500);
    }

    #[test]
    fn distributed_hundred_thousand_ten_thousand_session_workloads_stay_bounded() {
        for scale in [100usize, 1_000, 10_000] {
            let mut capture = capture();
            let template = capture.neighbors[0].clone();
            capture.neighbors = (0..scale)
                .map(|i| {
                    let mut neighbor = template.clone();
                    neighbor.neighbor_address = Some(format!(
                        "10.{}.{}.{}",
                        (i / 65_536) % 256,
                        (i / 256) % 256,
                        i % 256
                    ));
                    neighbor
                })
                .collect();
            let rows: Vec<_> = (0..scale)
                .map(|i| {
                    let mut row = bmp("established", 65002, NOW);
                    row.session_id = format!("bmp:session-{i}");
                    row.peer_address =
                        format!("10.{}.{}.{}", (i / 65_536) % 256, (i / 256) % 256, i % 256);
                    row
                })
                .collect();
            let started = std::time::Instant::now();
            let out = reconcile_native_bgp_with_bmp(&capture, NOW, &rows, NOW, policy());
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "scale {scale} reconciliation exceeded focused bounded-workload budget"
            );
            assert_eq!(out.total_native_sessions, scale);
            assert_eq!(out.assessments.len(), scale.min(500));
            assert_eq!(out.omitted_native_sessions, scale.saturating_sub(500));
            assert_eq!(out.bmp_rows_input, scale);
            assert_eq!(out.bmp_rows_input_limit_omitted, 0);
            assert!(out.bmp_rows_indexed <= 1_000);
            assert!(out.corroborated_established <= 500);
        }
    }

    #[test]
    fn concentrated_bmp_candidates_are_capped_and_truthfully_counted() {
        let rows: Vec<_> = (0..64)
            .map(|i| {
                let mut row = bmp("established", 65002, NOW);
                row.session_id = format!("bmp:concentrated-{i}");
                row
            })
            .collect();
        let out = reconcile_native_bgp_with_bmp(&capture(), NOW, &rows, NOW, policy());
        assert_eq!(out.bmp_rows_indexed, 16);
        assert_eq!(out.bmp_candidates_omitted, 48);
        assert_eq!(
            out.assessments[0].agreement,
            NativeBmpAgreement::AmbiguousBmpEvidence
        );
    }
}

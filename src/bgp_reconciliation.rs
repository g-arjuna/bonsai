//! Bounded reconciliation of native polled BGP neighbor evidence with BMP
//! peer-session evidence. This is an operational comparison only: BMP absence
//! is visibility missingness, never proof that a native BGP session is down.

use serde::Serialize;

use crate::remediation::NativeNosBgpCapture;
use crate::semantic_evidence::{self, EvidenceFreshness, FreshnessPolicy};
use crate::typed_facts::Lifecycle;

#[derive(Clone, Debug)]
pub struct BmpPeerEvidence {
    pub session_id: String,
    pub device_address: String,
    pub peer_address: String,
    pub peer_as: Option<u32>,
    pub session_state: String,
    pub network_instance: String,
    pub route_distinguisher: String,
    pub updated_at_ns: i64,
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
            row.network_instance.is_empty()
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
    let device_rows: Vec<_> = bmp_rows
        .iter()
        .filter(|row| row.device_address == capture.exporter_address)
        .collect();
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
    };

    for neighbor in &capture.neighbors {
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

        let peer_rows: Vec<_> = device_rows
            .iter()
            .copied()
            .filter(|row| same_peer(&row.peer_address, peer))
            .collect();
        if peer_rows.is_empty() {
            assessment.agreement = NativeBmpAgreement::MissingBmpVisibility;
            out.missing_bmp_visibility += 1;
            out.assessments.push(assessment);
            continue;
        }
        if neighbor.neighbor_scope_id.is_some() {
            // BMP peer evidence does not carry the interface scope required
            // to disambiguate link-local peers. Preserve the scoped native
            // identity, but do not infer which BMP row owns that scope.
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
        let bmp_freshness = freshness(bmp.updated_at_ns, as_of_ns, policy);
        assessment.bmp_session_id = Some(bmp.session_id.clone());
        assessment.bmp_session_state = Some(bmp.session_state.clone());
        assessment.bmp_peer_as = bmp.peer_as;
        assessment.bmp_freshness = Some(freshness_label(bmp_freshness).to_string());
        if !bmp_freshness.is_current() {
            assessment.agreement = NativeBmpAgreement::StaleEvidence;
            out.unknown_or_stale += 1;
        } else if native_captured_at_ns.abs_diff(bmp.updated_at_ns)
            > policy.max_capture_skew_seconds * 1_000_000_000
        {
            assessment.agreement = NativeBmpAgreement::ExcessiveCaptureSkew;
            out.unknown_or_stale += 1;
        } else if let (Some(native_as), Some(bmp_as)) = (neighbor.peer_as, bmp.peer_as)
            && native_as != bmp_as
        {
            assessment.agreement = NativeBmpAgreement::ConflictingPeerAs;
            out.conflicting += 1;
        } else {
            let native_established = neighbor
                .session_state
                .as_deref()
                .is_some_and(|state| state.eq_ignore_ascii_case("established"));
            let bmp_established = bmp.session_state.eq_ignore_ascii_case("established");
            assessment.agreement = match (native_established, bmp_established) {
                (true, true) => NativeBmpAgreement::CorroboratedEstablished,
                (false, false) => NativeBmpAgreement::CorroboratedNotEstablished,
                _ => NativeBmpAgreement::ConflictingSessionState,
            };
            match assessment.agreement {
                NativeBmpAgreement::CorroboratedEstablished => out.corroborated_established += 1,
                NativeBmpAgreement::CorroboratedNotEstablished => {
                    out.corroborated_not_established += 1
                }
                NativeBmpAgreement::ConflictingSessionState => out.conflicting += 1,
                _ => {}
            }
        }
        out.assessments.push(assessment);
    }
    if out.conflicting > 0 {
        out.state = "conflicting_evidence".into();
    } else if out.corroborated_established == 0
        && (out.missing_bmp_visibility > 0
            || out.ambiguous > 0
            || out.unknown_or_stale > 0
            || capture.neighbors.is_empty())
    {
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
            updated_at_ns,
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
}

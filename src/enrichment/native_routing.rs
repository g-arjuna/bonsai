//! Shared canonical identity, acquisition-order and admission helpers for
//! native routing evidence (BGP sessions and route-table shadow captures).
//!
//! Management addresses are acquisition metadata, never canonical evidence
//! identity. A positively verified current read must resolve the current
//! inventory target and compare its stable external/canonical identity with
//! the structured identity stored in the evidence.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::TargetConfig;

pub const NATIVE_ROUTING_IDENTITY_SCOPE: &str = "inventory";
pub const NATIVE_ROUTING_DEFAULT_NAMESPACE: &str = "default";
pub const NATIVE_BGP_NOKIA_PROFILE: &str = "nokia.srlinux.fixed-readonly-native-bgp-v1";
pub const NATIVE_BGP_FRR_OFFLINE_PROFILE: &str = "frr.offline-native-bgp-summary-v1";
pub const NATIVE_ROUTE_FRR_PROFILE: &str = "frr.fixed-readonly-native-routes-v1";

pub const PROVENANCE_AUTHORIZED_LIVE: &str = "authorized_live_acquisition";
pub const PROVENANCE_SYNTHETIC_CONTROL: &str = "synthetic_integration_control";
pub const PROVENANCE_HISTORICAL_FIXTURE: &str = "genuine_historical_fixture";
pub const PROVENANCE_RETAINED_SHADOW: &str = "retained_shadow_evidence";

/// Structured identity of one retained native-routing evidence stream.
/// `capture_id` is evidence metadata; the durable slot remains one current
/// shadow observation per canonical device/scope/namespace/family/profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NativeRoutingIdentity {
    pub canonical_device_id: String,
    pub identity_scope: String,
    pub network_namespace: String,
    pub afi_safi: String,
    pub acquisition_profile: String,
    pub source_provenance: String,
    pub capture_id: String,
}

impl NativeRoutingIdentity {
    pub fn new(
        target: &TargetConfig,
        afi_safi: &str,
        acquisition_profile: &str,
        source_provenance: &str,
        capture_started_at_ns: i64,
        exporter_address: &str,
    ) -> Self {
        let canonical_device_id = canonical_device_id(target);
        let capture_material = format!(
            "bonsai.native-routing.capture.v1\0{canonical_device_id}\0{afi_safi}\0{acquisition_profile}\0{capture_started_at_ns}\0{exporter_address}"
        );
        Self {
            canonical_device_id,
            identity_scope: NATIVE_ROUTING_IDENTITY_SCOPE.into(),
            network_namespace: NATIVE_ROUTING_DEFAULT_NAMESPACE.into(),
            afi_safi: afi_safi.into(),
            acquisition_profile: acquisition_profile.into(),
            source_provenance: source_provenance.into(),
            capture_id: format!(
                "native-routing-capture-{}",
                hex::encode(Sha256::digest(capture_material.as_bytes()))
            ),
        }
    }
}

/// Canonical inventory identity established by PR #95/schema-bound
/// acquisition: stable external identity when present, otherwise the canonical
/// address fallback. Callers must treat the fallback as weaker evidence when
/// evaluating management-address reassignment.
pub fn canonical_device_id(target: &TargetConfig) -> String {
    crate::registry::device_identity_key(target)
}

pub fn has_stable_external_identity(target: &TargetConfig) -> bool {
    target
        .external_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
}

/// Identity used only to calculate the canonical storage slot for a query.
/// Capture membership and acquisition provenance are still validated from the
/// stored row; they are never established by this lookup-shaped identity.
pub fn query_identity(
    target: &TargetConfig,
    afi_safi: &str,
    acquisition_profile: &str,
) -> NativeRoutingIdentity {
    NativeRoutingIdentity {
        canonical_device_id: canonical_device_id(target),
        identity_scope: NATIVE_ROUTING_IDENTITY_SCOPE.into(),
        network_namespace: NATIVE_ROUTING_DEFAULT_NAMESPACE.into(),
        afi_safi: afi_safi.into(),
        acquisition_profile: acquisition_profile.into(),
        source_provenance: PROVENANCE_AUTHORIZED_LIVE.into(),
        capture_id: "current-query-scope".into(),
    }
}

fn valid_component(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value.chars().any(|c| c.is_control() || c == '\0')
}

pub fn validate_afi_safi(afi_safi: &str) -> Result<()> {
    if !matches!(afi_safi, "ipv4-unicast" | "ipv6-unicast") {
        bail!("unsupported native routing AFI/SAFI")
    }
    Ok(())
}

pub fn validate_identity(
    identity: &NativeRoutingIdentity,
    afi_safi: &str,
    approved_profiles: &[&str],
) -> Result<()> {
    validate_afi_safi(afi_safi)?;
    if identity.afi_safi != afi_safi {
        bail!("native routing identity AFI/SAFI mismatch")
    }
    if identity.identity_scope != NATIVE_ROUTING_IDENTITY_SCOPE {
        bail!("native routing identity scope is not the approved inventory scope")
    }
    if identity.network_namespace != NATIVE_ROUTING_DEFAULT_NAMESPACE {
        bail!("unsupported native routing network namespace")
    }
    if !approved_profiles.contains(&identity.acquisition_profile.as_str()) {
        bail!("native routing acquisition profile is not approved for this evidence class")
    }
    if !matches!(
        identity.source_provenance.as_str(),
        PROVENANCE_AUTHORIZED_LIVE
            | PROVENANCE_SYNTHETIC_CONTROL
            | PROVENANCE_HISTORICAL_FIXTURE
            | PROVENANCE_RETAINED_SHADOW
    ) {
        bail!("native routing source provenance is not explicit")
    }
    if !valid_component(&identity.canonical_device_id, 512)
        || !valid_component(&identity.capture_id, 160)
    {
        bail!("native routing canonical/capture identity is malformed")
    }
    Ok(())
}

pub fn canonical_observation_id(kind: &str, identity: &NativeRoutingIdentity) -> String {
    let material = format!(
        "bonsai.native-routing.v1\0{kind}\0{}\0{}\0{}\0{}\0{}",
        identity.canonical_device_id,
        identity.identity_scope,
        identity.network_namespace,
        identity.afi_safi,
        identity.acquisition_profile
    );
    format!(
        "native-routing-{kind}-{}",
        hex::encode(Sha256::digest(material.as_bytes()))
    )
}

/// Exact pre-correction BGP key shape. Retained only so historical rows can be
/// read as explicitly unverified legacy evidence; never migrate them silently.
pub fn legacy_bgp_observation_id(address: &str, family: &str) -> String {
    format!(
        "native-bgp-{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&(address, family)).expect("string tuple")
        ))
    )
}

/// Exact pre-correction route key shape, likewise legacy/unverified only.
pub fn legacy_route_observation_id(address: &str, afi_safi: &str) -> String {
    format!(
        "native-routes-{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&(address, afi_safi)).expect("string tuple")
        ))
    )
}

pub fn validate_acquisition_interval(
    evidence_name: &str,
    capture_started_at_ns: i64,
    source_observed_at_ns: Option<i64>,
    captured_at_ns: i64,
    ingested_at_ns: Option<i64>,
) -> Result<()> {
    if capture_started_at_ns <= 0 || captured_at_ns <= 0 || capture_started_at_ns > captured_at_ns {
        bail!("{evidence_name} acquisition interval is contradictory")
    }
    if let Some(source) = source_observed_at_ns {
        if source <= 0 || source > captured_at_ns {
            bail!("{evidence_name} source observation time is outside the acquisition interval")
        }
    }
    if let Some(ingested) = ingested_at_ns {
        if ingested < captured_at_ns {
            bail!("{evidence_name} ingestion time precedes acquisition completion")
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionOrderDecision {
    AcceptNewer,
    ExactReplay,
    EqualPositionConflict,
    RejectOlder,
}

pub fn acquisition_order_decision<T>(
    prior: Option<(i64, &T)>,
    candidate_started_at_ns: i64,
    candidate: &T,
) -> AcquisitionOrderDecision
where
    T: PartialEq,
{
    let Some((prior_started_at_ns, prior_value)) = prior else {
        return AcquisitionOrderDecision::AcceptNewer;
    };
    if candidate_started_at_ns < prior_started_at_ns {
        return AcquisitionOrderDecision::RejectOlder;
    }
    if candidate_started_at_ns == prior_started_at_ns {
        if prior_value == candidate {
            AcquisitionOrderDecision::ExactReplay
        } else {
            AcquisitionOrderDecision::EqualPositionConflict
        }
    } else {
        AcquisitionOrderDecision::AcceptNewer
    }
}

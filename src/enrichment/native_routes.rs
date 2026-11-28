//! Inventory-bound persistence for native route table observations.
//!
//! This is retained shadow evidence only: it never mutates authoritative
//! graph forwarding state and never creates a competing routing database.
//! The monotonic compare-write contract mirrors native BGP persistence —
//! exact replay is idempotent, stale writes are rejected and equal-time
//! conflicting observations fail explicitly.
use crate::{
    enrichment::native_routing::{
        NATIVE_ROUTE_FRR_PROFILE, NativeRoutingIdentity, acquisition_order_decision,
        canonical_observation_id, validate_acquisition_interval, validate_identity,
    },
    graph::{ConfigItemRecord, GraphStore},
    native_routes::NativeRouteCapture,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredNativeRouteObservation {
    pub captured_at_ns: i64,
    pub capture_started_at_ns: i64,
    /// Acquisition provenance class: verified_ssh_read_only_cli for live
    /// capture, synthetic_fixture_replay for controls, or another explicit
    /// label. Consumers must never present replay as live acquisition.
    pub transport: String,
    pub raw_routes: String,
    pub capture: NativeRouteCapture,
    /// Address family of the capture command (`ipv4-unicast` for
    /// `show ip route vrf all json`, `ipv6-unicast` for the IPv6 form).
    pub afi_safi: String,
    /// Positive inventory/acquisition identity established at collection time.
    /// Legacy address-keyed rows predate this field and must remain unverified.
    #[serde(default)]
    pub inventory_identity: Option<String>,
    #[serde(default)]
    pub acquisition_profile: Option<String>,
    /// Positive canonical inventory evidence identity. Absence means legacy
    /// address-keyed evidence and never current verified inventory identity.
    #[serde(default)]
    pub routing_identity: Option<NativeRoutingIdentity>,
    /// Best source observation timestamp when positively exposed by the source.
    #[serde(default)]
    pub source_observed_at_ns: Option<i64>,
    /// Local durable ingestion timestamp; processing metadata, not source time.
    #[serde(default)]
    pub ingested_at_ns: Option<i64>,
    /// Positive source ordering/generation token when established.
    #[serde(default)]
    pub source_ordering_token: Option<String>,
}

/// Historical pre-correction address/family key. New canonical observations
/// use `observation_identity_key`; this helper remains only for explicitly
/// unverified legacy rows and compatibility migration reads.
pub fn observation_id(address: &str, afi_safi: &str) -> String {
    crate::enrichment::native_routing::legacy_route_observation_id(address, afi_safi)
}

pub fn observation_identity_key(identity: &NativeRoutingIdentity) -> String {
    canonical_observation_id("routes", identity)
}

pub fn validate_afi(afi_safi: &str) -> Result<()> {
    if !matches!(afi_safi, "ipv4-unicast" | "ipv6-unicast") {
        bail!("unsupported native route AFI/SAFI")
    }
    Ok(())
}

fn semantic_json(observation: &StoredNativeRouteObservation) -> serde_json::Value {
    let mut value = serde_json::to_value(observation).expect("serialize native route observation");
    if let Some(object) = value.as_object_mut() {
        object.remove("ingested_at_ns");
    }
    value
}

fn validate_stored_observation(observation: &StoredNativeRouteObservation) -> Result<()> {
    validate_afi(&observation.afi_safi)?;
    validate_acquisition_interval(
        "native route evidence",
        observation.capture_started_at_ns,
        observation.source_observed_at_ns,
        observation.captured_at_ns,
        observation.ingested_at_ns,
    )?;
    if observation.transport.trim().is_empty()
        || observation.transport.len() > 128
        || observation.transport.chars().any(char::is_control)
    {
        bail!("native route transport/provenance label is malformed");
    }
    if observation.raw_routes.len() > 8 * 1024 * 1024 {
        bail!("native route retained raw evidence exceeds bounded admission envelope");
    }
    let actual_digest = hex::encode(Sha256::digest(observation.raw_routes.as_bytes()));
    if actual_digest != observation.capture.raw_sample_sha256 {
        bail!("native route raw-output digest does not match capture metadata");
    }
    if observation.capture.parser_id.trim().is_empty()
        || observation.capture.parser_version.trim().is_empty()
    {
        bail!("native route parser provenance is incomplete");
    }
    if observation.capture.routes.len() > 100_000 {
        bail!("native route population exceeds bounded admission envelope");
    }
    let mut next_hop_count = 0usize;
    let expected_v4 = observation.afi_safi == "ipv4-unicast";
    for route in &observation.capture.routes {
        next_hop_count = next_hop_count.saturating_add(route.next_hops.len());
        if next_hop_count > 100_000 * 8 {
            bail!("native route next-hop population exceeds bounded admission envelope");
        }
        if route.vrf_name.len() > 255 || route.vrf_name.chars().any(char::is_control) {
            bail!("native route VRF identity is malformed");
        }
        if let Some((prefix_ip, _)) = crate::native_routes::parse_prefix(&route.destination_prefix)
        {
            if prefix_ip.is_ipv4() != expected_v4 {
                bail!("native route observation contains incompatible mixed-family state");
            }
        }
    }
    if let Some(identity) = &observation.routing_identity {
        validate_identity(identity, &observation.afi_safi, &[NATIVE_ROUTE_FRR_PROFILE])?;
        if observation.acquisition_profile.as_deref() != Some(identity.acquisition_profile.as_str())
        {
            bail!("native route acquisition profile does not match canonical evidence identity");
        }
    }
    Ok(())
}

pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredNativeRouteObservation,
) -> Result<String> {
    let mut observation = observation.clone();
    if observation.ingested_at_ns.is_none() {
        observation.ingested_at_ns = Some(crate::graph::common::now_ns());
    }
    validate_stored_observation(&observation)?;
    let id = observation
        .routing_identity
        .as_ref()
        .map(observation_identity_key)
        .unwrap_or_else(|| {
            observation_id(&observation.capture.exporter_address, &observation.afi_safi)
        });
    let content_json = serde_json::to_string(&observation)?;
    let candidate_started = observation.capture_started_at_ns;
    let candidate_semantic = semantic_json(&observation);
    let exporter = observation.capture.exporter_address.clone();
    let write_id = id.clone();
    store
        .compare_write_config_items(vec![id.clone()], move |existing| {
            if let Some(Some(prior)) = existing.get(&write_id) {
                if prior.config_class != "native_route_observation" {
                    bail!("native route persistence storage-class collision");
                }
                let prior_obs: StoredNativeRouteObservation =
                    serde_json::from_str(&prior.content_json)
                        .context("invalid prior native route observation")?;
                validate_stored_observation(&prior_obs)?;
                let prior_semantic = semantic_json(&prior_obs);
                match acquisition_order_decision(
                    Some((prior_obs.capture_started_at_ns, &prior_semantic)),
                    candidate_started,
                    &candidate_semantic,
                ) {
                    crate::enrichment::native_routing::AcquisitionOrderDecision::RejectOlder => {
                        bail!(
                            "older-start native route acquisition cannot overwrite a newer accepted observation"
                        );
                    }
                    crate::enrichment::native_routing::AcquisitionOrderDecision::EqualPositionConflict => {
                        bail!("equal-position conflicting native route observation rejected");
                    }
                    crate::enrichment::native_routing::AcquisitionOrderDecision::ExactReplay => {
                        return Ok((Vec::new(), ()));
                    }
                    crate::enrichment::native_routing::AcquisitionOrderDecision::AcceptNewer => {}
                }
            }
            Ok((
                vec![ConfigItemRecord {
                    id: write_id.clone(),
                    config_class: "native_route_observation".into(),
                    vendor: "frr".into(),
                    name: exporter,
                    version: "1".into(),
                    content_json,
                    enabled: true,
                    created_by: "native-route-collector".into(),
                }],
                (),
            ))
        })
        .await?;
    Ok(id)
}

pub async fn latest_observation(
    store: &GraphStore,
    address: &str,
    afi_safi: &str,
) -> Result<StoredNativeRouteObservation> {
    validate_afi(afi_safi)?;
    let id = observation_id(address, afi_safi);
    let item = store
        .get_config_item(&id)
        .await?
        .filter(|i| i.enabled)
        .context("native_route_evidence_unavailable: collect registered device state first")?;
    if item.config_class != "native_route_observation" {
        bail!("native route storage-class mismatch");
    }
    let obs: StoredNativeRouteObservation = serde_json::from_str(&item.content_json)?;
    validate_stored_observation(&obs)?;
    if obs.capture.exporter_address != address || obs.afi_safi != afi_safi {
        bail!("native route observation identity mismatch")
    }
    Ok(obs)
}

/// Canonical first, legacy fallback second. The storage class is returned so
/// consumers can deny current operational conclusions for legacy rows instead
/// of silently migrating or trusting them.
pub async fn read_observation(
    store: &GraphStore,
    identity: Option<&NativeRoutingIdentity>,
    legacy_address: &str,
    afi_safi: &str,
) -> Result<(StoredNativeRouteObservation, &'static str)> {
    validate_afi(afi_safi)?;
    if let Some(identity) = identity {
        validate_identity(identity, afi_safi, &[NATIVE_ROUTE_FRR_PROFILE])?;
        let id = observation_identity_key(identity);
        if let Some(item) = store.get_config_item(&id).await?.filter(|i| i.enabled) {
            if item.config_class != "native_route_observation" {
                bail!("native route storage-class mismatch");
            }
            let obs: StoredNativeRouteObservation = serde_json::from_str(&item.content_json)?;
            validate_stored_observation(&obs)?;
            if obs
                .routing_identity
                .as_ref()
                .map(|i| i.canonical_device_id.as_str())
                != Some(identity.canonical_device_id.as_str())
            {
                bail!("native route canonical identity mismatch");
            }
            return Ok((obs, "canonical_identity"));
        }
    }
    let legacy = latest_observation(store, legacy_address, afi_safi).await?;
    Ok((legacy, "legacy_address_keyed_unverified"))
}

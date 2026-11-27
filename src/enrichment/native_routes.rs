//! Inventory-bound persistence for native route table observations.
//!
//! This is retained shadow evidence only: it never mutates authoritative
//! graph forwarding state and never creates a competing routing database.
//! The monotonic compare-write contract mirrors native BGP persistence —
//! exact replay is idempotent, stale writes are rejected and equal-time
//! conflicting observations fail explicitly.
use crate::{
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
}

pub fn observation_id(address: &str, afi_safi: &str) -> String {
    format!(
        "native-routes-{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&(address, afi_safi)).expect("string tuple")
        ))
    )
}

pub fn validate_afi(afi_safi: &str) -> Result<()> {
    if !matches!(afi_safi, "ipv4-unicast" | "ipv6-unicast") {
        bail!("unsupported native route AFI/SAFI")
    }
    Ok(())
}

pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredNativeRouteObservation,
) -> Result<String> {
    validate_afi(&observation.afi_safi)?;
    let id = observation_id(&observation.capture.exporter_address, &observation.afi_safi);
    let content_json = serde_json::to_string(observation)?;
    let captured_at_ns = observation.captured_at_ns;
    let exporter = observation.capture.exporter_address.clone();
    let write_id = id.clone();
    store
        .compare_write_config_items(vec![id.clone()], move |existing| {
            if let Some(Some(prior)) = existing.get(&write_id) {
                let prior_obs: StoredNativeRouteObservation =
                    serde_json::from_str(&prior.content_json)
                        .context("invalid prior native route observation")?;
                if prior_obs.captured_at_ns > captured_at_ns {
                    bail!("stale native route observation rejected by monotonic persistence fence");
                }
                if prior_obs.captured_at_ns == captured_at_ns
                    && prior.content_json != content_json.as_str()
                {
                    bail!("equal-time conflicting native route observation rejected");
                }
                if prior.content_json == content_json {
                    return Ok((Vec::new(), ()));
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
    let obs: StoredNativeRouteObservation = serde_json::from_str(&item.content_json)?;
    if obs.capture.exporter_address != address || obs.afi_safi != afi_safi {
        bail!("native route observation identity mismatch")
    }
    Ok(obs)
}

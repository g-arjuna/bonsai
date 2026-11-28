//! Inventory-bound, fixed-command native state capture. No device configuration writes.
use crate::{
    config::TargetConfig,
    credentials::ResolvedCredential,
    enrichment::native_routing::{
        NATIVE_BGP_FRR_OFFLINE_PROFILE, NATIVE_BGP_NOKIA_PROFILE, NativeRoutingIdentity,
        PROVENANCE_AUTHORIZED_LIVE, acquisition_order_decision, canonical_device_id,
        canonical_observation_id, validate_acquisition_interval, validate_identity,
    },
    graph::{ConfigItemRecord, GraphStore},
    remediation::{NativeNosBgpCapture, parse_nokia_srl_native_bgp_neighbors},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredNativeBgpObservation {
    pub captured_at_ns: i64,
    pub capture_started_at_ns: i64,
    pub transport: String,
    pub raw_neighbors: String,
    pub raw_system_information: String,
    pub capture: NativeNosBgpCapture,
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
pub fn observation_id(address: &str, family: &str) -> String {
    crate::enrichment::native_routing::legacy_bgp_observation_id(address, family)
}

pub fn observation_identity_key(identity: &NativeRoutingIdentity) -> String {
    canonical_observation_id("bgp", identity)
}

pub fn validate_family(family: &str) -> Result<()> {
    if !matches!(family, "ipv4-unicast" | "ipv6-unicast") {
        bail!("unsupported native AFI/SAFI")
    }
    Ok(())
}

fn semantic_json(observation: &StoredNativeBgpObservation) -> serde_json::Value {
    let mut value = serde_json::to_value(observation).expect("serialize native BGP observation");
    if let Some(object) = value.as_object_mut() {
        object.remove("ingested_at_ns");
    }
    value
}

fn validate_stored_observation(observation: &StoredNativeBgpObservation) -> Result<()> {
    validate_family(&observation.capture.afi_safi)?;
    validate_acquisition_interval(
        "native BGP evidence",
        observation.capture_started_at_ns,
        observation.source_observed_at_ns,
        observation.captured_at_ns,
        observation.ingested_at_ns,
    )?;
    if observation.transport.trim().is_empty()
        || observation.transport.len() > 128
        || observation.transport.chars().any(char::is_control)
    {
        bail!("native BGP transport/provenance label is malformed");
    }
    if observation.raw_neighbors.len() > 4 * 1024 * 1024
        || observation.raw_system_information.len() > 4 * 1024 * 1024
    {
        bail!("native BGP retained raw evidence exceeds bounded admission envelope");
    }
    let actual_digest = hex::encode(Sha256::digest(observation.raw_neighbors.as_bytes()));
    if actual_digest != observation.capture.raw_sample_sha256 {
        bail!("native BGP raw-output digest does not match capture metadata");
    }
    if observation.capture.parser_id.trim().is_empty()
        || observation.capture.parser_version.trim().is_empty()
    {
        bail!("native BGP parser provenance is incomplete");
    }
    if observation.capture.neighbors.len() > 10_000 {
        bail!("native BGP neighbor population exceeds bounded admission envelope");
    }
    for neighbor in &observation.capture.neighbors {
        if neighbor.afi_safi != observation.capture.afi_safi {
            bail!("native BGP observation contains incompatible mixed-family state");
        }
        if neighbor.vrf_name.len() > 255 || neighbor.vrf_name.chars().any(char::is_control) {
            bail!("native BGP neighbor VRF identity is malformed");
        }
    }
    if let Some(identity) = &observation.routing_identity {
        validate_identity(
            identity,
            &observation.capture.afi_safi,
            &[NATIVE_BGP_NOKIA_PROFILE, NATIVE_BGP_FRR_OFFLINE_PROFILE],
        )?;
        if observation.acquisition_profile.as_deref() != Some(identity.acquisition_profile.as_str())
        {
            bail!("native BGP acquisition profile does not match canonical evidence identity");
        }
    }
    Ok(())
}

pub async fn collect_native_bgp(
    target: &TargetConfig,
    credentials: &ResolvedCredential,
    family: &str,
    known_hosts: Option<&Path>,
) -> Result<StoredNativeBgpObservation> {
    validate_family(family)?;
    if crate::platform_registry::resolve_target_platform(target).platform_id != "nokia.srlinux" {
        bail!("native BGP collector currently supports Nokia SR Linux only")
    }
    let address = target.primary_ip();
    address
        .parse::<std::net::IpAddr>()
        .context("native collection requires an inventory IP address")?;
    let started = crate::graph::common::now_ns();
    let python = std::env::var_os("BONSAI_PYTHON_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let local = PathBuf::from(".venv/bin/python3");
            if local.exists() {
                local
            } else {
                PathBuf::from("python3")
            }
        });
    let mut cmd = tokio::process::Command::new(python);
    cmd.kill_on_drop(true)
        .arg("scripts/native_bgp_capture.py")
        .arg("--host")
        .arg(address)
        .arg("--username")
        .arg(&credentials.username)
        .env("BONSAI_CAPTURE_PASSWORD", credentials.password.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = known_hosts {
        cmd.arg("--known-hosts").arg(path);
    }
    let output = crate::enrichment::frr_interfaces::run_capture_helper_bounded(
        &mut cmd,
        std::time::Duration::from_secs(30),
    )
    .await
    .context("native capture bounded helper execution failed")?;
    if !output.status.success() {
        bail!(
            "native capture failed (helper stderr {} bytes); check inventory credentials and pinned SSH host keys",
            output.stderr_bytes
        )
    }
    #[derive(Deserialize)]
    struct Raw {
        neighbors: String,
        system_information: String,
    }
    let raw: Raw =
        serde_json::from_slice(&output.stdout).context("invalid native capture envelope")?;
    if raw.neighbors.len() > 4 * 1024 * 1024 || raw.system_information.len() > 4 * 1024 * 1024 {
        bail!("native capture exceeds bounds")
    }
    let version: serde_json::Value = serde_json::from_str(&raw.system_information)?;
    let version = version
        .get("version")
        .and_then(|v| v.as_str())
        .context("native system information has no version")?;
    let capture = parse_nokia_srl_native_bgp_neighbors(&raw.neighbors, address, version, family)
        .map_err(anyhow::Error::msg)?;
    Ok(StoredNativeBgpObservation {
        captured_at_ns: crate::graph::common::now_ns(),
        capture_started_at_ns: started,
        transport: "verified_ssh_read_only_cli".into(),
        raw_neighbors: raw.neighbors,
        raw_system_information: raw.system_information,
        capture,
        inventory_identity: Some(canonical_device_id(target)),
        acquisition_profile: Some("nokia.srlinux.fixed-readonly-native-bgp-v1".into()),
        routing_identity: Some(NativeRoutingIdentity::new(
            target,
            family,
            "nokia.srlinux.fixed-readonly-native-bgp-v1",
            PROVENANCE_AUTHORIZED_LIVE,
            started,
            address,
        )),
        source_observed_at_ns: None,
        ingested_at_ns: None,
        source_ordering_token: None,
    })
}

pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredNativeBgpObservation,
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
            observation_id(
                &observation.capture.exporter_address,
                &observation.capture.afi_safi,
            )
        });
    let content_json = serde_json::to_string(&observation)?;
    let candidate_started = observation.capture_started_at_ns;
    let candidate_semantic = semantic_json(&observation);
    let exporter = observation.capture.exporter_address.clone();
    let write_id = id.clone();
    store
        .compare_write_config_items(vec![id.clone()], move |existing| {
            if let Some(Some(prior)) = existing.get(&write_id) {
                if prior.config_class != "native_bgp_observation" {
                    bail!("native BGP persistence storage-class collision");
                }
                let prior_obs: StoredNativeBgpObservation =
                    serde_json::from_str(&prior.content_json)
                        .context("invalid prior native BGP observation")?;
                validate_stored_observation(&prior_obs)?;
                let prior_semantic = semantic_json(&prior_obs);
                match acquisition_order_decision(
                    Some((prior_obs.capture_started_at_ns, &prior_semantic)),
                    candidate_started,
                    &candidate_semantic,
                ) {
                    crate::enrichment::native_routing::AcquisitionOrderDecision::RejectOlder => {
                        bail!(
                            "older-start native BGP acquisition cannot overwrite a newer accepted observation"
                        );
                    }
                    crate::enrichment::native_routing::AcquisitionOrderDecision::EqualPositionConflict => {
                        bail!("equal-position conflicting native BGP observation rejected");
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
                    config_class: "native_bgp_observation".into(),
                    vendor: "nokia.srlinux".into(),
                    name: exporter,
                    version: "1".into(),
                    content_json,
                    enabled: true,
                    created_by: "native-bgp-collector".into(),
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
    family: &str,
) -> Result<StoredNativeBgpObservation> {
    validate_family(family)?;
    let id = observation_id(address, family);
    let item = store
        .get_config_item(&id)
        .await?
        .filter(|i| i.enabled)
        .context("native_bgp_evidence_unavailable: collect registered device state first")?;
    if item.config_class != "native_bgp_observation" {
        bail!("native BGP storage-class mismatch");
    }
    let obs: StoredNativeBgpObservation = serde_json::from_str(&item.content_json)?;
    validate_stored_observation(&obs)?;
    if obs.capture.exporter_address != address || obs.capture.afi_safi != family {
        bail!("native observation identity mismatch")
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
    family: &str,
) -> Result<(StoredNativeBgpObservation, &'static str)> {
    validate_family(family)?;
    if let Some(identity) = identity {
        validate_identity(
            identity,
            family,
            &[NATIVE_BGP_NOKIA_PROFILE, NATIVE_BGP_FRR_OFFLINE_PROFILE],
        )?;
        let id = observation_identity_key(identity);
        if let Some(item) = store.get_config_item(&id).await?.filter(|i| i.enabled) {
            if item.config_class != "native_bgp_observation" {
                bail!("native BGP storage-class mismatch");
            }
            let obs: StoredNativeBgpObservation = serde_json::from_str(&item.content_json)?;
            validate_stored_observation(&obs)?;
            if obs
                .routing_identity
                .as_ref()
                .map(|i| i.canonical_device_id.as_str())
                != Some(identity.canonical_device_id.as_str())
            {
                bail!("native BGP canonical identity mismatch");
            }
            return Ok((obs, "canonical_identity"));
        }
    }
    let legacy = latest_observation(store, legacy_address, family).await?;
    Ok((legacy, "legacy_address_keyed_unverified"))
}

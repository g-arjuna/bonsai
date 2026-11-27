//! Inventory-bound, fixed-command native state capture. No device configuration writes.
use crate::{
    config::TargetConfig,
    credentials::ResolvedCredential,
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
}

pub fn observation_id(address: &str, family: &str) -> String {
    format!(
        "native-bgp-{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&(address, family)).expect("string tuple")
        ))
    )
}

pub fn validate_family(family: &str) -> Result<()> {
    if !matches!(family, "ipv4-unicast" | "ipv6-unicast") {
        bail!("unsupported native AFI/SAFI")
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
        inventory_identity: Some(format!("registered-target:{address}")),
        acquisition_profile: Some("nokia.srlinux.fixed-readonly-native-bgp-v1".into()),
    })
}

pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredNativeBgpObservation,
) -> Result<String> {
    let id = observation_id(
        &observation.capture.exporter_address,
        &observation.capture.afi_safi,
    );
    let content_json = serde_json::to_string(observation)?;
    let captured_at_ns = observation.captured_at_ns;
    let exporter = observation.capture.exporter_address.clone();
    let write_id = id.clone();
    store
        .compare_write_config_items(vec![id.clone()], move |existing| {
            if let Some(Some(prior)) = existing.get(&write_id) {
                let prior_obs: StoredNativeBgpObservation =
                    serde_json::from_str(&prior.content_json)
                        .context("invalid prior native BGP observation")?;
                if prior_obs.captured_at_ns > captured_at_ns {
                    bail!("stale native BGP observation rejected by monotonic persistence fence");
                }
                if prior_obs.captured_at_ns == captured_at_ns
                    && prior.content_json != content_json.as_str()
                {
                    bail!("equal-time conflicting native BGP observation rejected");
                }
                if prior.content_json == content_json {
                    return Ok((Vec::new(), ()));
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
    let obs: StoredNativeBgpObservation = serde_json::from_str(&item.content_json)?;
    if obs.capture.exporter_address != address || obs.capture.afi_safi != family {
        bail!("native observation identity mismatch")
    }
    Ok(obs)
}

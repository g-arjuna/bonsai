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

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredNativeBgpObservation {
    pub captured_at_ns: i64,
    pub capture_started_at_ns: i64,
    pub transport: String,
    pub raw_neighbors: String,
    pub raw_system_information: String,
    pub capture: NativeNosBgpCapture,
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
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .context("native capture timed out")?
        .context("native capture helper failed to start")?;
    if !output.status.success() {
        bail!("native capture failed; check inventory credentials and pinned SSH host keys")
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
    store
        .upsert_config_item(ConfigItemRecord {
            id: id.clone(),
            config_class: "native_bgp_observation".into(),
            vendor: "nokia.srlinux".into(),
            name: observation.capture.exporter_address.clone(),
            version: "1".into(),
            content_json: serde_json::to_string(observation)?,
            enabled: true,
            created_by: "native-bgp-collector".into(),
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
        .list_config_items(Some("native_bgp_observation".into()))
        .await?
        .into_iter()
        .find(|i| i.id == id && i.enabled)
        .context("native_bgp_evidence_unavailable: collect registered device state first")?;
    let obs: StoredNativeBgpObservation = serde_json::from_str(&item.content_json)?;
    if obs.capture.exporter_address != address || obs.capture.afi_safi != family {
        bail!("native observation identity mismatch")
    }
    Ok(obs)
}

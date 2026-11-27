//! Inventory-bound, fixed-command standalone-FRR interface observation.
//!
//! This module is deliberately narrow: it acquires only the approved
//! read-only FRR JSON commands over verified SSH, dispatches the structured
//! interface observation through the offline normalizer registry, and persists
//! bounded shadow evidence. It performs no device writes and no authoritative
//! graph projection.
use crate::{
    config::TargetConfig,
    credentials::ResolvedCredential,
    frr_interface_json::{FRR_JSON_ACQUISITION_CONTRACT, FrrInterfaceJsonObservation},
    graph::{ConfigItemRecord, GraphStore},
    interface_normalizer::{
        CaptureOutcome, CommandCapture, NormalizationOutput, ObservationContext,
    },
    neighbor_normalizer::{
        ArpndObservation, ArpndSourceProfile, InterfaceIdentityEvidence, MacObservation,
        MacSourceProfile, normalize_arpnd, normalize_mac,
    },
    normalizer_registry::{InterfaceObservationInput, NormalizerRegistry},
    nos_features::{HardwareScope, ProfileQuery, SoftwareRelease, builtin_registry},
    typed_facts::PayloadState,
    vlan_normalizer::{VlanObservation, VlanSourceProfile, normalize_vlan},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock, Weak},
};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore},
};

pub const FRR_INTERFACE_COMMAND: &str = "vtysh -c 'show interface json'";
pub const FRR_VERSION_COMMAND: &str = "vtysh -c 'show version json'";
pub const FRR_NEIGHBOR_COMMAND: &str = "ip neigh show";
pub const FRR_MAC_COMMAND: &str = "bridge fdb show";
pub const FRR_VLAN_COMMAND: &str = "bridge -c vlan show";
pub const FRR_INTERFACE_FEATURE: &str = "interfaces.frr_json";
pub const FRR_L2_CAPTURE_CONTRACT: &str = "frr_verified_ssh_l2_snapshot_v1";
pub const FRR_INTERFACE_CAPTURE_CONTRACT: &str = FRR_L2_CAPTURE_CONTRACT;
const CAPTURE_DEADLINE_SECS: u64 = 45;
const MAX_CONCURRENT_CAPTURES: usize = 2;
const MAX_STORED_FACTS: usize = 256;
const MAX_STORED_DIAGNOSTICS: usize = 128;
const MAX_STORED_RAW_BYTES: usize = 1024 * 1024;
const MAX_STORED_OBSERVATION_BYTES: usize = 6 * 1024 * 1024;
const MAX_HELPER_STDOUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;

fn capture_permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_CAPTURES)))
}

fn device_capture_permits() -> &'static std::sync::Mutex<BTreeMap<String, Weak<Semaphore>>> {
    static PERMITS: OnceLock<std::sync::Mutex<BTreeMap<String, Weak<Semaphore>>>> = OnceLock::new();
    PERMITS.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

/// Admission held for the complete device acquisition lifecycle: capture,
/// bounded interpretation and the persistence/compare-write boundary. The
/// registry stores only weak semaphore references, so completed operations do
/// not leave one permanent strong allocation per inventory churn event.
pub struct NativeCaptureAdmission {
    device_key: String,
    device_semaphore: Arc<Semaphore>,
    _device_permit: OwnedSemaphorePermit,
    _global_permit: OwnedSemaphorePermit,
}

impl Drop for NativeCaptureAdmission {
    fn drop(&mut self) {
        // Admission is non-queueing (`try_acquire`), so when this owning
        // operation drops there cannot be a waiter still using the semaphore.
        // Remove only the exact semaphore generation we owned. This bounds
        // inventory-churn key retention instead of merely weakening the value
        // while retaining an ever-growing set of canonical device keys.
        if let Ok(mut devices) = device_capture_permits().lock() {
            let owned_generation = devices
                .get(&self.device_key)
                .and_then(Weak::upgrade)
                .is_some_and(|current| Arc::ptr_eq(&current, &self.device_semaphore));
            if owned_generation {
                devices.remove(&self.device_key);
            }
        }
    }
}

impl NativeCaptureAdmission {
    pub fn device_key(&self) -> &str {
        &self.device_key
    }
}

pub fn try_acquire_capture_admission(device_key: &str) -> Result<NativeCaptureAdmission> {
    let semaphore = {
        let mut devices = device_capture_permits()
            .lock()
            .map_err(|_| anyhow::anyhow!("frr interface device admission lock poisoned"))?;
        devices.retain(|_, weak| weak.strong_count() > 0);
        if let Some(existing) = devices.get(device_key).and_then(Weak::upgrade) {
            existing
        } else {
            let created = Arc::new(Semaphore::new(1));
            devices.insert(device_key.to_string(), Arc::downgrade(&created));
            created
        }
    };
    let device_permit = semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("frr_interface_admission_overloaded"))?;
    let global_permit = match capture_permits().clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(error) => {
            // Do not leave a new canonical key behind when global admission
            // refuses the operation before an admission object exists to own
            // that key's lifecycle.
            if let Ok(mut devices) = device_capture_permits().lock() {
                let owned_generation = devices
                    .get(device_key)
                    .and_then(Weak::upgrade)
                    .is_some_and(|current| Arc::ptr_eq(&current, &semaphore));
                if owned_generation {
                    devices.remove(device_key);
                }
            }
            return Err(anyhow::anyhow!(
                "frr_interface_admission_overloaded: {error}"
            ));
        }
    };
    Ok(NativeCaptureAdmission {
        device_key: device_key.to_string(),
        device_semaphore: semaphore,
        _device_permit: device_permit,
        _global_permit: global_permit,
    })
}

#[cfg(test)]
fn device_capture_permit_key_present(key: &str) -> bool {
    device_capture_permits()
        .lock()
        .expect("device permit lock")
        .contains_key(key)
}

struct BoundedChildOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

struct BoundedStreamOutput {
    retained: Vec<u8>,
    total_bytes: u64,
    overflow: bool,
}

async fn read_bounded_stream<R>(
    mut reader: R,
    max_bytes: usize,
) -> std::io::Result<BoundedStreamOutput>
where
    R: tokio::io::AsyncRead + Unpin,
{
    // Retain at most max_bytes+1 bytes as proof of overflow and stop reading
    // immediately when that bound is crossed. The orchestrating caller owns
    // child termination; continuing to drain an overflowing stream would let
    // a hostile descendant consume the full deadline and pipe bandwidth.
    let mut retained = Vec::with_capacity((max_bytes + 1).min(8192));
    let mut total = 0u64;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if retained.len() <= max_bytes {
            let remaining = (max_bytes + 1).saturating_sub(retained.len());
            retained.extend_from_slice(&buf[..n.min(remaining)]);
        }
        if total > max_bytes as u64 {
            return Ok(BoundedStreamOutput {
                retained,
                total_bytes: total,
                overflow: true,
            });
        }
    }
    Ok(BoundedStreamOutput {
        retained,
        total_bytes: total,
        overflow: false,
    })
}

async fn terminate_capture_child(
    child: &mut tokio::process::Child,
    stdout_task: &mut tokio::task::JoinHandle<std::io::Result<BoundedStreamOutput>>,
    stderr_task: &mut tokio::task::JoinHandle<std::io::Result<BoundedStreamOutput>>,
) {
    // Tokio's kill waits for the direct child to be reaped. Descendants that
    // inherited pipes are not part of the child lifecycle and cannot be
    // awaited safely; their reader tasks are explicitly aborted below so no
    // unbounded detached work remains after this function returns. A task
    // that just reported overflow may already be completed, so these handles
    // must not be polled a second time.
    let _ = child.kill().await;
    let _ = child.wait().await;
    stdout_task.abort();
    stderr_task.abort();
}

async fn run_capture_helper_bounded(
    cmd: &mut tokio::process::Command,
    deadline: std::time::Duration,
) -> Result<BoundedChildOutput> {
    let mut child = cmd
        .spawn()
        .context("frr interface capture helper failed to start")?;
    let stdout = child
        .stdout
        .take()
        .context("frr interface capture helper stdout pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("frr interface capture helper stderr pipe unavailable")?;
    let mut stdout_task = tokio::spawn(read_bounded_stream(stdout, MAX_HELPER_STDOUT_BYTES));
    let mut stderr_task = tokio::spawn(read_bounded_stream(stderr, MAX_HELPER_STDERR_BYTES));
    let timeout = tokio::time::sleep(deadline);
    tokio::pin!(timeout);
    let mut status = None;
    let mut stdout_done = None;
    let mut stderr_done = None;

    loop {
        if status.is_some() && stdout_done.is_some() && stderr_done.is_some() {
            break;
        }
        tokio::select! {
            wait_status = child.wait(), if status.is_none() => {
                status = Some(wait_status.context("frr interface capture helper wait failed")?);
            }
            out = &mut stdout_task, if stdout_done.is_none() => {
                let out = out
                    .context("frr interface capture stdout drain task failed")??;
                if out.overflow {
                    terminate_capture_child(&mut child, &mut stdout_task, &mut stderr_task).await;
                    bail!("frr interface capture helper stdout exceeds bounded retention limit")
                }
                stdout_done = Some(out);
            }
            err = &mut stderr_task, if stderr_done.is_none() => {
                let err = err
                    .context("frr interface capture stderr drain task failed")??;
                if err.overflow {
                    terminate_capture_child(&mut child, &mut stdout_task, &mut stderr_task).await;
                    bail!("frr interface capture helper stderr exceeds bounded retention limit")
                }
                stderr_done = Some(err);
            }
            _ = &mut timeout => {
                terminate_capture_child(&mut child, &mut stdout_task, &mut stderr_task).await;
                bail!("frr interface capture deadline exceeded; child terminated and reaped")
            }
        }
    }

    let stdout = stdout_done.expect("stdout drain completed");
    let stderr = stderr_done.expect("stderr drain completed");
    Ok(BoundedChildOutput {
        status: status.expect("child status completed"),
        stdout: stdout.retained,
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedHostKey {
    pub algorithm: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFrrInterfaceShadowObservation {
    pub observation_id: String,
    pub device_address: String,
    /// Canonical inventory identity used in typed-fact subjects. Stable
    /// external identity is preferred; mutable hostname and management address
    /// remain attributes, with canonical address only the documented fallback.
    pub device_id: String,
    #[serde(default)]
    pub collector_id: String,
    pub captured_at_ns: i64,
    pub capture_started_at_ns: i64,
    pub captured_at: String,
    pub transport: String,
    pub acquisition_contract: String,
    pub interface_command: String,
    pub version_command: String,
    #[serde(default)]
    pub neighbor_command: Option<String>,
    #[serde(default)]
    pub mac_command: Option<String>,
    #[serde(default)]
    pub vlan_command: Option<String>,
    pub host_key: VerifiedHostKey,
    pub platform: String,
    pub release_raw: String,
    /// Immutable artifact identity declared by inventory/operator metadata.
    /// It is required for the narrow `_git` lab-build qualification and is
    /// never represented as device-attested evidence.
    pub release_build: Option<String>,
    pub network_namespace: String,
    /// Why that namespace claim is justified. The fixed SSH session starts
    /// in the device/container default network namespace and has no netns
    /// selection feature; per-interface VRF bindings remain separate facts.
    pub network_namespace_evidence: String,
    pub device_hostname_reported: Option<String>,
    pub interface_output_sha256: String,
    pub interface_output_bytes: usize,
    /// Raw output is retained only within this explicit bound. Larger inputs
    /// remain evidence-addressed by digest and are marked omitted.
    pub raw_interface_json: Option<String>,
    pub raw_output_omitted_reason: Option<String>,
    #[serde(default)]
    pub neighbor_output_sha256: Option<String>,
    #[serde(default)]
    pub neighbor_output_bytes: Option<usize>,
    #[serde(default)]
    pub raw_neighbor_output: Option<String>,
    #[serde(default)]
    pub raw_neighbor_output_omitted_reason: Option<String>,
    #[serde(default)]
    pub mac_output_sha256: Option<String>,
    #[serde(default)]
    pub mac_output_bytes: Option<usize>,
    #[serde(default)]
    pub raw_mac_output: Option<String>,
    #[serde(default)]
    pub raw_mac_output_omitted_reason: Option<String>,
    #[serde(default)]
    pub vlan_output_sha256: Option<String>,
    #[serde(default)]
    pub vlan_output_bytes: Option<usize>,
    #[serde(default)]
    pub raw_vlan_output: Option<String>,
    #[serde(default)]
    pub raw_vlan_output_omitted_reason: Option<String>,
    pub profile_id: String,
    pub profile_fingerprint: Option<String>,
    pub normalizer_id: String,
    pub normalizer_revision: String,
    pub mapping_revision: Option<String>,
    pub qualification: String,
    pub shadow_only: bool,
    pub capture_id: String,
    pub session_epoch: u64,
    pub sequence: u64,
    pub facts_emitted: usize,
    pub facts_validated: usize,
    pub facts_retained: usize,
    pub fact_retention_truncated: bool,
    pub facts: Vec<serde_json::Value>,
    pub diagnostics_retained: usize,
    pub diagnostic_retention_truncated: bool,
    pub interface_diagnostics: Vec<serde_json::Value>,
    pub normalization_report: serde_json::Value,
    pub acquisition_classification: String,
    /// Semantic interpretation outcome independent of transport and storage
    /// success (`complete_usable`, `partial_usable`, `genuine_complete_empty`,
    /// or the explicit degraded payload states). Missing in pre-correction
    /// shadow records; readers must not infer usability from ABI validity.
    #[serde(default)]
    pub interpretation_outcome: String,
    #[serde(default)]
    pub usable_entity_facts: usize,
    #[serde(default)]
    pub status_facts: usize,
}

/// Bounded latest-attempt evidence. This is deliberately a small metadata +
/// status-fact record, not a duplicate raw capture. Successful usable captures
/// reference the retained shadow observation instead; degraded observations
/// keep their status facts here without replacing last usable evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFrrInterfaceAcquisitionAttempt {
    pub attempt_id: String,
    pub device_address: String,
    pub device_id: String,
    pub network_namespace: String,
    pub started_at_ns: i64,
    pub finished_at_ns: i64,
    /// `success`, `partial_success`, `degraded` or `failed`.
    pub outcome: String,
    pub failure_class: Option<String>,
    pub failure_detail: Option<String>,
    pub interpretation_outcome: Option<String>,
    pub capture_id: Option<String>,
    pub session_epoch: u64,
    pub sequence: u64,
    pub observation_id: Option<String>,
    pub transport: Option<String>,
    pub acquisition_contract: Option<String>,
    pub profile_id: Option<String>,
    pub normalizer_revision: Option<String>,
    pub mapping_revision: Option<String>,
    pub qualification: Option<String>,
    pub interface_output_sha256: Option<String>,
    pub interface_output_bytes: Option<usize>,
    pub facts_emitted: usize,
    pub facts_validated: usize,
    pub usable_entity_facts: usize,
    pub status_facts: usize,
    #[serde(default)]
    pub degraded_facts: Vec<serde_json::Value>,
    #[serde(default)]
    pub normalization_report: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFrrInterfaceAcquisitionState {
    pub device_address: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub network_namespace: String,
    pub updated_at_ns: i64,
    pub last_attempt_started_at_ns: i64,
    pub last_attempt_finished_at_ns: i64,
    /// `success` or `failed`; authorization/admission refusals that never
    /// started acquisition are intentionally not represented as attempts.
    pub last_attempt_outcome: String,
    pub last_failure_class: Option<String>,
    /// Static sanitized detail only. Device output, credential material and
    /// helper stderr are never stored here.
    pub last_failure_detail: Option<String>,
    pub consecutive_failures: u32,
    pub last_successful_observation_id: Option<String>,
    pub last_successful_captured_at_ns: Option<i64>,
    pub last_successful_capture_started_at_ns: Option<i64>,
    #[serde(default)]
    pub last_interpretation_outcome: Option<String>,
    #[serde(default)]
    pub last_attempt_evidence_id: Option<String>,
}

pub const FRR_INTERFACE_OBSERVATION_CLASS: &str = "native_interface_shadow_observation";
pub const FRR_INTERFACE_ACQUISITION_CLASS: &str = "native_interface_acquisition_state";
pub const FRR_INTERFACE_ATTEMPT_CLASS: &str = "native_interface_latest_attempt";
const NATIVE_INTERFACE_KEY_DOMAIN: &str = "bonsai.native-interface.v2";

fn native_key(kind: &str, device_id: &str, network_namespace: &str) -> String {
    let material = format!(
        "{NATIVE_INTERFACE_KEY_DOMAIN}\0{kind}\0{FRR_INTERFACE_FEATURE}\0{device_id}\0{network_namespace}"
    );
    format!(
        "native-interface-{kind}-{}",
        hex::encode(Sha256::digest(material.as_bytes()))
    )
}

/// Stable shadow-slot identity for the canonical device + feature + network
/// namespace. Management address is evidence metadata, never key material.
pub fn observation_id(device_id: &str, network_namespace: &str) -> String {
    native_key("shadow-observation", device_id, network_namespace)
}

fn latest_attempt_id(device_id: &str, network_namespace: &str) -> String {
    native_key("latest-attempt", device_id, network_namespace)
}

fn acquisition_state_id(device_id: &str, network_namespace: &str) -> String {
    native_key("acquisition-state", device_id, network_namespace)
}

fn legacy_address_observation_id(address: &str) -> String {
    format!(
        "native-frr-interface-{}",
        hex::encode(Sha256::digest(address.as_bytes()))
    )
}

fn legacy_address_acquisition_state_id(address: &str) -> String {
    format!(
        "native-frr-interface-acquisition-{}",
        hex::encode(Sha256::digest(address.as_bytes()))
    )
}

/// Canonical inventory identity contract shared with schema-bound acquisition:
/// stable external identity when present, otherwise the canonical inventory
/// address. Hostname is descriptive metadata and is not an identity fallback.
pub fn canonical_device_id(target: &TargetConfig) -> String {
    crate::registry::device_identity_key(target)
}

/// Interpretation outcome is independent from SSH transport success and
/// typed-fact ABI validity. Returns `(outcome, usable, entity_facts,
/// status_facts)`; only usable outcomes may replace last-usable evidence.
fn classify_interpretation(output: &NormalizationOutput) -> (&'static str, bool, usize, usize) {
    let entity_facts = output
        .facts
        .iter()
        .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
        .count();
    let status_facts = output
        .facts
        .iter()
        .filter(|fact| fact.subject.entity.is_none() && fact.payload.state.is_some())
        .count();
    if entity_facts > 0 {
        let degraded_capture = output
            .report
            .sources
            .iter()
            .any(|source| source.outcome != CaptureOutcome::Complete);
        let incomplete = degraded_capture
            || output.report.entities_omitted > 0
            || output.report.invalid_entities > 0
            || output.report.facts_rejected > 0
            || output.report.output_facts_omitted > 0
            || output.report.addresses_omitted > 0;
        return if incomplete {
            ("partial_usable", true, entity_facts, status_facts)
        } else {
            ("complete_usable", true, entity_facts, status_facts)
        };
    }
    let state = output
        .facts
        .iter()
        .find_map(|fact| fact.payload.state)
        .unwrap_or(PayloadState::NoUsableRecords);
    let outcome = match state {
        PayloadState::Empty
            if output
                .report
                .sources
                .iter()
                .all(|source| source.outcome == CaptureOutcome::Complete) =>
        {
            "genuine_complete_empty"
        }
        PayloadState::Empty => "no_usable_records",
        PayloadState::NoUsableRecords => "no_usable_records",
        PayloadState::OutputBudgetExhausted => "output_budget_exhausted",
        PayloadState::InvalidInput => "invalid_input",
        PayloadState::CaptureFailed => "capture_failed",
        PayloadState::Unsupported => "unsupported_release_or_format",
        PayloadState::UnstructuredFallback | PayloadState::UnknownQuality => "invalid_input",
    };
    let usable = outcome == "genuine_complete_empty";
    (outcome, usable, entity_facts, status_facts)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCaptureEnvelope {
    host: String,
    port: u16,
    transport: String,
    host_key: VerifiedHostKey,
    version_command: String,
    interface_command: String,
    version_output: String,
    interface_output: String,
    neighbor_command: String,
    mac_command: String,
    vlan_command: String,
    neighbor_output: String,
    mac_output: String,
    vlan_output: String,
    release_raw: Option<String>,
    device_hostname: Option<String>,
}

fn interface_identity_evidence(
    facts: &[crate::typed_facts::TypedFact],
) -> InterfaceIdentityEvidence {
    let mut by_native_name = BTreeMap::new();
    for fact in facts {
        let Some(identity) = fact.subject.interface_identity.clone() else {
            continue;
        };
        // Only the exact source-native display name may key this corroboration
        // map. For subinterfaces the canonical subject entity is deliberately
        // the parent/base interface, so using that entity as a key would
        // misattribute parent-interface neighbor/FDB evidence to the child.
        if let Some(native_name) = identity
            .vendor_native_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            by_native_name
                .entry(native_name.to_string())
                .or_insert(identity);
        }
    }
    InterfaceIdentityEvidence { by_native_name }
}

fn validate_capture_envelope(raw: &RawCaptureEnvelope, address: &str) -> Result<()> {
    if raw.host != address
        || raw.port != 22
        || raw.transport != "verified_ssh_read_only_cli"
        || raw.interface_command != FRR_INTERFACE_COMMAND
        || raw.neighbor_command != FRR_NEIGHBOR_COMMAND
        || raw.mac_command != FRR_MAC_COMMAND
        || raw.vlan_command != FRR_VLAN_COMMAND
        || raw.version_command != FRR_VERSION_COMMAND
    {
        bail!("frr interface capture envelope identity/contract mismatch")
    }
    Ok(())
}

/// Acquire, select, dispatch and validate one bounded shadow observation.
/// The target must be inventory-registered before this function is called;
/// this function itself only enforces the narrow platform/credential shape.
pub async fn collect_normalize_frr_interfaces(
    target: &TargetConfig,
    credentials: &ResolvedCredential,
    known_hosts: Option<&Path>,
    collector_id: &str,
    admission: &NativeCaptureAdmission,
) -> Result<StoredFrrInterfaceShadowObservation> {
    if crate::platform_registry::resolve_target_platform(target).platform_id != "frr" {
        bail!("frr interface collector requires a standalone-frr inventory target")
    }
    let address = target.primary_ip();
    address
        .parse::<std::net::IpAddr>()
        .context("frr interface collection requires an inventory IP address")?;
    let known_hosts = known_hosts
        .context("pinned SSH host-key configuration is required for native interface collection")?;
    // The only currently qualified structured interface profile is bound to
    // one immutable artifact. Missing or different declared build metadata is
    // knowable before network contact and must refuse at admission rather
    // than use the device as an oracle for supportability.
    if target.software_build.as_deref() != Some(crate::nos_features::FRR_1061_STRUCTURED_ARTIFACT) {
        bail!(
            "frr interface shadow dispatch refused: exact qualified software build metadata is absent or does not match the supported artifact"
        )
    }
    let device_id = canonical_device_id(target);
    if admission.device_key() != device_id {
        bail!("native interface acquisition admission identity mismatch")
    }
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
        .arg("scripts/frr_interface_capture.py")
        .arg("--host")
        .arg(address)
        .arg("--username")
        .arg(&credentials.username)
        .env("BONSAI_CAPTURE_PASSWORD", credentials.password.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.arg("--known-hosts").arg(known_hosts);
    let output = run_capture_helper_bounded(
        &mut cmd,
        std::time::Duration::from_secs(CAPTURE_DEADLINE_SECS),
    )
    .await?;
    // Keep the admission permit through normalization and validation, not
    // merely the SSH subprocess: large-but-in-budget structured input is CPU
    // and memory work in this process and must count against the same bound.
    if !output.status.success() {
        bail!(
            "frr interface capture failed (helper stderr {} bytes); verify credentials and pinned SSH host keys",
            output.stderr_bytes
        )
    }
    let raw: RawCaptureEnvelope =
        serde_json::from_slice(&output.stdout).context("invalid frr interface capture envelope")?;
    validate_capture_envelope(&raw, address)?;
    if output.stdout_bytes == 0 {
        bail!("frr interface capture helper produced no envelope")
    }
    if raw.interface_output.len() > 4 * 1024 * 1024
        || raw.version_output.len() > 4 * 1024 * 1024
        || raw.neighbor_output.len() > 4 * 1024 * 1024
        || raw.mac_output.len() > 4 * 1024 * 1024
        || raw.vlan_output.len() > 4 * 1024 * 1024
    {
        bail!("frr interface capture exceeds helper bounds")
    }
    let release_raw = raw
        .release_raw
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .context("frr version evidence is unavailable")?
        .to_string();
    let release_build = target.software_build.clone();
    let address_string = address.to_string();
    let collector_id = collector_id.to_string();
    let RawCaptureEnvelope {
        transport,
        host_key,
        version_command,
        interface_command,
        neighbor_command,
        mac_command,
        vlan_command,
        interface_output,
        neighbor_output,
        mac_output,
        vlan_output,
        device_hostname,
        ..
    } = raw;

    // JSON decoding, profile selection, native normalization, typed-fact
    // validation and serialization are bounded CPU work. Keep them off the
    // async request worker; the caller-owned admission object remains held by
    // the detached acquisition operation while this blocking job executes.
    tokio::task::spawn_blocking(move || -> Result<StoredFrrInterfaceShadowObservation> {
        let release = SoftwareRelease {
            raw: release_raw.clone(),
            train: None,
            build: release_build.clone(),
            components: BTreeMap::new(),
        };
        let namespace = "default";
        let profiles = builtin_registry();
        let receipt = profiles.select_receipt(&ProfileQuery {
            platform: "frr".into(),
            version: release_raw.clone(),
            release: Some(release),
            hardware: Some(HardwareScope {
                namespace: Some(namespace.into()),
                ..Default::default()
            }),
        });
        let captured_at_ns = crate::graph::common::now_ns();
        let captured_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let capture_id = format!("frr-interface-capture-{captured_at_ns}");
        // Order observations by acquisition start, not processing completion.
        // A slower older capture must not become current merely because it
        // finished after a newer capture; replaying this same observation
        // preserves rather than refreshes its original temporal position.
        let session_epoch = started as u64;
        let run = {
            let mut context =
                ObservationContext::minimal(&device_id, &collector_id, &capture_id, &captured_at);
            context.session_epoch = session_epoch;
            context.sequence = 1;
            context.network_namespace = Some(namespace);
            context.platform = Some("frr");
            context.release_raw = Some(&release_raw);
            context.release_build = release_build.as_deref();
            let capture = CommandCapture::new(&interface_output, CaptureOutcome::Complete)
                .captured_at(&captured_at);
            let observation = FrrInterfaceJsonObservation {
                capture,
                context,
                acquisition_contract: FRR_JSON_ACQUISITION_CONTRACT,
            };
            let registry = NormalizerRegistry::builtin();
            registry
                .dispatch_input(
                    &receipt,
                    FRR_INTERFACE_FEATURE,
                    InterfaceObservationInput::FrrJson(&observation),
                )
                .map_err(|e| anyhow::anyhow!("frr interface shadow dispatch refused: {e:?}"))?
        };
        let interface_identities = interface_identity_evidence(&run.output.facts);
        let mut neighbor_context =
            ObservationContext::minimal(&device_id, &collector_id, &capture_id, &captured_at);
        neighbor_context.session_epoch = session_epoch;
        neighbor_context.sequence = 1;
        neighbor_context.network_namespace = Some(namespace);
        neighbor_context.platform = Some("frr");
        neighbor_context.release_raw = Some(&release_raw);
        neighbor_context.release_build = release_build.as_deref();
        let neighbor_run = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::LinuxIpNeighShow,
            capture: CommandCapture::new(&neighbor_output, CaptureOutcome::Complete)
                .captured_at(&captured_at),
            context: neighbor_context,
            interface_identities: Some(&interface_identities),
        });
        let mut mac_context =
            ObservationContext::minimal(&device_id, &collector_id, &capture_id, &captured_at);
        mac_context.session_epoch = session_epoch;
        mac_context.sequence = 1;
        mac_context.network_namespace = Some(namespace);
        mac_context.platform = Some("frr");
        mac_context.release_raw = Some(&release_raw);
        mac_context.release_build = release_build.as_deref();
        let mac_run = normalize_mac(&MacObservation {
            profile: MacSourceProfile::LinuxBridgeFdbShow,
            capture: CommandCapture::new(&mac_output, CaptureOutcome::Complete)
                .captured_at(&captured_at),
            context: mac_context,
            interface_identities: Some(&interface_identities),
        });
        let mut vlan_context =
            ObservationContext::minimal(&device_id, &collector_id, &capture_id, &captured_at);
        vlan_context.session_epoch = session_epoch;
        vlan_context.sequence = 1;
        vlan_context.network_namespace = Some(namespace);
        vlan_context.platform = Some("frr");
        vlan_context.release_raw = Some(&release_raw);
        vlan_context.release_build = release_build.as_deref();
        let vlan_run = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::LinuxBridgeCVlanShow,
            capture: CommandCapture::new(&vlan_output, CaptureOutcome::Complete)
                .captured_at(&captured_at),
            context: vlan_context,
        });
        let typed_facts: Vec<_> = run
            .output
            .facts
            .iter()
            .chain(neighbor_run.facts.iter())
            .chain(mac_run.facts.iter())
            .chain(vlan_run.facts.iter())
            .collect();
        if !typed_facts.iter().all(|fact| fact.validate().is_ok()) {
            bail!("frr native normalizers emitted an invalid typed fact")
        }
        let (mut interpretation_outcome, usable_observation, _interface_entity_facts, _interface_status_facts) =
            classify_interpretation(&run.output);
        let family_degraded = [
            (&neighbor_run.report.refusal_reason, neighbor_run.report.malformed_records + neighbor_run.report.facts_rejected + neighbor_run.report.output_facts_omitted),
            (&mac_run.report.refusal_reason, mac_run.report.malformed_records + mac_run.report.facts_rejected + mac_run.report.output_facts_omitted),
            (&vlan_run.report.refusal_reason, vlan_run.report.malformed_records + vlan_run.report.facts_rejected + vlan_run.report.output_facts_omitted),
        ]
        .iter()
        .any(|(refusal, bad)| refusal.is_some() || *bad > 0);
        if family_degraded && usable_observation {
            interpretation_outcome = "partial_usable";
        }
        let usable_entity_facts = typed_facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let status_facts = typed_facts
            .iter()
            .filter(|fact| fact.subject.entity.is_none() && fact.payload.state.is_some())
            .count();
        let facts_emitted = typed_facts.len();
        let facts_retained = facts_emitted.min(MAX_STORED_FACTS);
        let fact_retention_truncated = facts_retained != facts_emitted;
        if fact_retention_truncated && usable_observation {
            interpretation_outcome = "partial_usable";
        }
        let mut facts = Vec::with_capacity(facts_retained);
        for fact in typed_facts.iter().take(facts_retained) {
            facts.push(serde_json::to_value(fact)?);
        }
        let diagnostics_retained = run.output.interfaces.len().min(MAX_STORED_DIAGNOSTICS);
        let mut interface_diagnostics = Vec::with_capacity(diagnostics_retained);
        for diagnostic in run.output.interfaces.iter().take(diagnostics_retained) {
            interface_diagnostics.push(serde_json::to_value(diagnostic)?);
        }
        let interface_sha = hex::encode(Sha256::digest(interface_output.as_bytes()));
        let interface_output_bytes = interface_output.len();
        let raw_within_retention = interface_output.len() <= MAX_STORED_RAW_BYTES;
        let neighbor_sha = hex::encode(Sha256::digest(neighbor_output.as_bytes()));
        let mac_sha = hex::encode(Sha256::digest(mac_output.as_bytes()));
        let vlan_sha = hex::encode(Sha256::digest(vlan_output.as_bytes()));
        let neighbor_within_retention = neighbor_output.len() <= MAX_STORED_RAW_BYTES;
        let mac_within_retention = mac_output.len() <= MAX_STORED_RAW_BYTES;
        let vlan_within_retention = vlan_output.len() <= MAX_STORED_RAW_BYTES;
        let mut normalization_report = serde_json::to_value(&run.output.report)?;
        normalization_report["feature_families"] = serde_json::json!([
            {"feature":"interfaces.frr_json","facts_emitted":run.output.facts.len(),"report":serde_json::to_value(&run.output.report)?},
            {"feature":"arpnd.linux_ip_neigh","facts_emitted":neighbor_run.facts.len(),"output_sha256":neighbor_sha,"output_bytes":neighbor_output.len(),"report":serde_json::to_value(&neighbor_run.report)?,"coverage":serde_json::to_value(&neighbor_run.coverage)?},
            {"feature":"mac.linux_bridge_fdb","facts_emitted":mac_run.facts.len(),"output_sha256":mac_sha,"output_bytes":mac_output.len(),"report":serde_json::to_value(&mac_run.report)?,"coverage":serde_json::to_value(&mac_run.coverage)?},
            {"feature":"vlan.linux_bridge_vlan","facts_emitted":vlan_run.facts.len(),"output_sha256":vlan_sha,"output_bytes":vlan_output.len(),"report":serde_json::to_value(&vlan_run.report)?,"coverage":serde_json::to_value(&vlan_run.coverage)?}
        ]);
        Ok(StoredFrrInterfaceShadowObservation {
            observation_id: observation_id(&device_id, namespace),
            device_address: address_string,
            device_id,
            collector_id,
            captured_at_ns,
            capture_started_at_ns: started,
            captured_at,
            transport,
            acquisition_contract: FRR_INTERFACE_CAPTURE_CONTRACT.into(),
            interface_command,
            version_command,
            neighbor_command: Some(neighbor_command),
            mac_command: Some(mac_command),
            vlan_command: Some(vlan_command),
            host_key,
            platform: "frr".into(),
            release_raw,
            release_build,
            network_namespace: namespace.into(),
            network_namespace_evidence:
                "fixed verified SSH session in device default network namespace; no namespace override"
                    .into(),
            device_hostname_reported: device_hostname,
            interface_output_sha256: interface_sha,
            interface_output_bytes,
            raw_interface_json: raw_within_retention.then_some(interface_output),
            raw_output_omitted_reason: (!raw_within_retention)
                .then_some("raw output exceeds 1 MiB shadow retention bound".into()),
            neighbor_output_sha256: Some(neighbor_sha),
            neighbor_output_bytes: Some(neighbor_output.len()),
            raw_neighbor_output: neighbor_within_retention.then_some(neighbor_output),
            raw_neighbor_output_omitted_reason: (!neighbor_within_retention)
                .then_some("raw neighbor output exceeds 1 MiB shadow retention bound".into()),
            mac_output_sha256: Some(mac_sha),
            mac_output_bytes: Some(mac_output.len()),
            raw_mac_output: mac_within_retention.then_some(mac_output),
            raw_mac_output_omitted_reason: (!mac_within_retention)
                .then_some("raw MAC output exceeds 1 MiB shadow retention bound".into()),
            vlan_output_sha256: Some(vlan_sha),
            vlan_output_bytes: Some(vlan_output.len()),
            raw_vlan_output: vlan_within_retention.then_some(vlan_output),
            raw_vlan_output_omitted_reason: (!vlan_within_retention)
                .then_some("raw VLAN output exceeds 1 MiB shadow retention bound".into()),
            profile_id: run.profile_id,
            profile_fingerprint: run.profile_fingerprint,
            normalizer_id: run.normalizer_id,
            normalizer_revision: run.normalizer_revision,
            mapping_revision: run.mapping_revision,
            qualification: format!("{:?}", run.qualification),
            shadow_only: run.shadow_only,
            capture_id,
            session_epoch,
            sequence: 1,
            facts_emitted,
            facts_validated: facts_emitted,
            facts_retained,
            fact_retention_truncated,
            facts,
            diagnostics_retained,
            diagnostic_retention_truncated: diagnostics_retained != run.output.interfaces.len(),
            interface_diagnostics,
            normalization_report,
            acquisition_classification: "live_authorized_ssh_readonly".into(),
            interpretation_outcome: interpretation_outcome.into(),
            usable_entity_facts,
            status_facts,
        })
    })
    .await
    .context("frr interface normalization worker failed")?
}

fn validate_stored_observation(observation: &StoredFrrInterfaceShadowObservation) -> Result<()> {
    if observation.device_id.trim().is_empty() || observation.device_address.trim().is_empty() {
        bail!("native interface observation canonical identity is incomplete")
    }
    let expected = observation_id(&observation.device_id, &observation.network_namespace);
    if observation.observation_id != expected {
        bail!("native interface observation identity does not match its canonical storage key")
    }
    if observation.network_namespace.trim().is_empty()
        || observation.capture_id.trim().is_empty()
        || observation.facts_retained != observation.facts.len()
    {
        bail!("native interface observation metadata is internally inconsistent")
    }
    if observation.collector_id.trim().is_empty() {
        // Pre-correction shadow records did not duplicate collector_id at the
        // top level. They are not migrated; new writes must carry it. Reading
        // such a record is rejected rather than silently reattributing it.
        bail!("native interface observation predates canonical source identity")
    }
    let mut interface_entity_facts = 0usize;
    for fact in &observation.facts {
        let typed: crate::typed_facts::TypedFact = serde_json::from_value(fact.clone())
            .context("native interface typed fact is undecodable")?;
        typed
            .validate()
            .map_err(|e| anyhow::anyhow!("native interface typed fact validation failed: {e:?}"))?;
        if typed.fact_kind == "interface.state" && typed.subject.entity.is_some() {
            interface_entity_facts += 1;
        }
        if typed.subject.device_id != observation.device_id
            || typed.subject.namespace != "interface"
            || typed.subject.network_namespace.as_deref()
                != Some(observation.network_namespace.as_str())
            || typed.source.capture_id != observation.capture_id
            || typed.source.session_epoch != observation.session_epoch
            || typed.source.sequence != observation.sequence
            || typed.source.collector_id != observation.collector_id
        {
            bail!("native interface typed-fact subject/source identity mismatch")
        }
    }
    let report_entities = observation
        .normalization_report
        .get("entities_accepted")
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as usize);
    if let Some(entities) = report_entities {
        if entities != interface_entity_facts {
            bail!("native interface entity count does not match normalization report")
        }
    }
    Ok(())
}

fn validate_state_identity(
    state: &StoredFrrInterfaceAcquisitionState,
    device_id: &str,
    network_namespace: &str,
) -> Result<()> {
    if state.device_id != device_id || state.network_namespace != network_namespace {
        bail!("native interface acquisition state canonical identity mismatch")
    }
    let expected = acquisition_state_id(device_id, network_namespace);
    // The id is implicit in the storage key rather than duplicated in the
    // payload; compare the key inputs and let callers use the canonical key.
    if expected.trim().is_empty() {
        bail!("native interface acquisition state key is invalid")
    }
    Ok(())
}

fn decode_enabled<T: for<'de> Deserialize<'de>>(
    item: Option<ConfigItemRecord>,
    class: &str,
    what: &str,
) -> Result<Option<T>> {
    let Some(item) = item else { return Ok(None) };
    if item.config_class != class {
        bail!("{what} storage class mismatch")
    }
    if !item.enabled {
        // An invalidated tombstone is not evidence and must never be served.
        // At the write boundary it is simply absent baseline state, allowing a
        // later explicitly re-registered canonical identity to acquire fresh
        // evidence without resurrecting the invalidated payload.
        return Ok(None);
    }
    serde_json::from_str(&item.content_json)
        .with_context(|| format!("{what} is undecodable; refusing overwrite or reuse"))
}

fn observation_config_item(
    observation: &StoredFrrInterfaceShadowObservation,
    content_json: String,
) -> ConfigItemRecord {
    ConfigItemRecord {
        id: observation.observation_id.clone(),
        config_class: FRR_INTERFACE_OBSERVATION_CLASS.into(),
        vendor: "frr".into(),
        name: observation.device_id.clone(),
        version: "2".into(),
        content_json,
        enabled: true,
        created_by: "native-frr-interface-collector".into(),
    }
}

fn attempt_config_item(attempt: &StoredFrrInterfaceAcquisitionAttempt) -> Result<ConfigItemRecord> {
    Ok(ConfigItemRecord {
        id: attempt.attempt_id.clone(),
        config_class: FRR_INTERFACE_ATTEMPT_CLASS.into(),
        vendor: "frr".into(),
        name: attempt.device_id.clone(),
        version: "2".into(),
        content_json: serde_json::to_string(attempt)?,
        enabled: true,
        created_by: "native-frr-interface-collector".into(),
    })
}

fn state_config_item(state: &StoredFrrInterfaceAcquisitionState) -> Result<ConfigItemRecord> {
    Ok(ConfigItemRecord {
        id: acquisition_state_id(&state.device_id, &state.network_namespace),
        config_class: FRR_INTERFACE_ACQUISITION_CLASS.into(),
        vendor: "frr".into(),
        name: state.device_id.clone(),
        version: "2".into(),
        content_json: serde_json::to_string(state)?,
        enabled: true,
        created_by: "native-frr-interface-collector".into(),
    })
}

fn is_usable_interpretation(outcome: &str) -> bool {
    matches!(
        outcome,
        "complete_usable" | "partial_usable" | "genuine_complete_empty"
    )
}

fn attempt_from_observation(
    observation: &StoredFrrInterfaceShadowObservation,
    failure_class: Option<&str>,
    failure_detail: Option<&str>,
) -> StoredFrrInterfaceAcquisitionAttempt {
    let outcome = match observation.interpretation_outcome.as_str() {
        "complete_usable" | "genuine_complete_empty" => "success",
        "partial_usable" => "partial_success",
        _ => "degraded",
    };
    let degraded_facts = if outcome == "degraded" {
        observation
            .facts
            .iter()
            .filter(|fact| fact.get("subject").and_then(|s| s.get("entity")).is_none())
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    StoredFrrInterfaceAcquisitionAttempt {
        attempt_id: latest_attempt_id(&observation.device_id, &observation.network_namespace),
        device_address: observation.device_address.clone(),
        device_id: observation.device_id.clone(),
        network_namespace: observation.network_namespace.clone(),
        started_at_ns: observation.capture_started_at_ns,
        finished_at_ns: observation.captured_at_ns,
        outcome: outcome.into(),
        failure_class: failure_class.map(str::to_string),
        failure_detail: failure_detail.map(str::to_string),
        interpretation_outcome: Some(observation.interpretation_outcome.clone()),
        capture_id: Some(observation.capture_id.clone()),
        session_epoch: observation.session_epoch,
        sequence: observation.sequence,
        observation_id: is_usable_interpretation(&observation.interpretation_outcome)
            .then_some(observation.observation_id.clone()),
        transport: Some(observation.transport.clone()),
        acquisition_contract: Some(observation.acquisition_contract.clone()),
        profile_id: Some(observation.profile_id.clone()),
        normalizer_revision: Some(observation.normalizer_revision.clone()),
        mapping_revision: observation.mapping_revision.clone(),
        qualification: Some(observation.qualification.clone()),
        interface_output_sha256: Some(observation.interface_output_sha256.clone()),
        interface_output_bytes: Some(observation.interface_output_bytes),
        facts_emitted: observation.facts_emitted,
        facts_validated: observation.facts_validated,
        usable_entity_facts: observation.usable_entity_facts,
        status_facts: observation.status_facts,
        degraded_facts,
        normalization_report: Some(observation.normalization_report.clone()),
    }
}

fn transport_failure_attempt(
    target: &TargetConfig,
    started_at_ns: i64,
    finished_at_ns: i64,
    failure_class: &str,
    failure_detail: &str,
) -> StoredFrrInterfaceAcquisitionAttempt {
    let device_id = canonical_device_id(target);
    StoredFrrInterfaceAcquisitionAttempt {
        attempt_id: latest_attempt_id(&device_id, "default"),
        device_address: target.primary_ip().to_string(),
        device_id,
        network_namespace: "default".into(),
        started_at_ns,
        finished_at_ns,
        outcome: "failed".into(),
        failure_class: Some(failure_class.into()),
        failure_detail: Some(failure_detail.into()),
        interpretation_outcome: None,
        capture_id: None,
        session_epoch: started_at_ns as u64,
        sequence: 1,
        observation_id: None,
        transport: None,
        acquisition_contract: Some(FRR_INTERFACE_CAPTURE_CONTRACT.into()),
        profile_id: None,
        normalizer_revision: None,
        mapping_revision: None,
        qualification: None,
        interface_output_sha256: None,
        interface_output_bytes: None,
        facts_emitted: 0,
        facts_validated: 0,
        usable_entity_facts: 0,
        status_facts: 0,
        degraded_facts: Vec::new(),
        normalization_report: None,
    }
}

fn failed_attempt(outcome: &str) -> bool {
    matches!(outcome, "failed" | "degraded")
}

/// Merge one completed attempt into prior state. Returns `(state,
/// attempt_is_latest)`. An older usable observation may still refresh the
/// last-usable evidence pointer when it is newer than the previously retained
/// usable evidence, but it must never erase a newer latest-attempt failure.
fn merge_attempt_state(
    prior: Option<StoredFrrInterfaceAcquisitionState>,
    attempt: &StoredFrrInterfaceAcquisitionAttempt,
) -> (StoredFrrInterfaceAcquisitionState, bool) {
    let now = crate::graph::common::now_ns();
    let mut state = prior.unwrap_or_else(|| StoredFrrInterfaceAcquisitionState {
        device_address: attempt.device_address.clone(),
        device_id: attempt.device_id.clone(),
        network_namespace: attempt.network_namespace.clone(),
        updated_at_ns: 0,
        last_attempt_started_at_ns: 0,
        last_attempt_finished_at_ns: 0,
        last_attempt_outcome: "none".into(),
        last_failure_class: None,
        last_failure_detail: None,
        consecutive_failures: 0,
        last_successful_observation_id: None,
        last_successful_captured_at_ns: None,
        last_successful_capture_started_at_ns: None,
        last_interpretation_outcome: None,
        last_attempt_evidence_id: None,
    });
    if state.device_id.is_empty() {
        state.device_id = attempt.device_id.clone();
    }
    if state.network_namespace.is_empty() {
        state.network_namespace = attempt.network_namespace.clone();
    }
    let attempt_is_latest = attempt.started_at_ns > state.last_attempt_started_at_ns;
    if attempt_is_latest {
        state.consecutive_failures = if failed_attempt(&attempt.outcome) {
            if failed_attempt(&state.last_attempt_outcome) {
                state.consecutive_failures.saturating_add(1)
            } else {
                1
            }
        } else {
            0
        };
        state.device_address = attempt.device_address.clone();
        state.last_attempt_started_at_ns = attempt.started_at_ns;
        state.last_attempt_finished_at_ns = attempt.finished_at_ns;
        state.last_attempt_outcome = attempt.outcome.clone();
        state.last_failure_class = attempt.failure_class.clone();
        state.last_failure_detail = attempt.failure_detail.clone();
        state.last_interpretation_outcome = attempt.interpretation_outcome.clone();
        state.last_attempt_evidence_id = Some(attempt.attempt_id.clone());
    }
    if let Some(observation_id) = attempt.observation_id.as_ref() {
        let success_started = state.last_successful_capture_started_at_ns.unwrap_or(0);
        if attempt.started_at_ns >= success_started {
            state.last_successful_observation_id = Some(observation_id.clone());
            state.last_successful_captured_at_ns = Some(attempt.finished_at_ns);
            state.last_successful_capture_started_at_ns = Some(attempt.started_at_ns);
        }
    }
    state.updated_at_ns = now;
    (state, attempt_is_latest)
}

async fn persist_attempt_transaction(
    store: &GraphStore,
    observation: Option<StoredFrrInterfaceShadowObservation>,
    attempt: StoredFrrInterfaceAcquisitionAttempt,
) -> Result<()> {
    let mut ids = vec![
        attempt.attempt_id.clone(),
        acquisition_state_id(&attempt.device_id, &attempt.network_namespace),
    ];
    let observation_json = if let Some(observation) = observation.as_ref() {
        if observation.observation_id
            != observation_id(&observation.device_id, &observation.network_namespace)
        {
            bail!("native interface observation identity does not match its storage key")
        }
        ids.push(observation.observation_id.clone());
        let content = serde_json::to_string(observation)?;
        if content.len() > MAX_STORED_OBSERVATION_BYTES {
            bail!(
                "bounded native interface shadow observation exceeds {} bytes",
                MAX_STORED_OBSERVATION_BYTES
            )
        }
        Some(content)
    } else {
        None
    };
    let attempt_key = attempt.attempt_id.clone();
    let state_key = acquisition_state_id(&attempt.device_id, &attempt.network_namespace);
    store
        .compare_write_config_items(ids, move |existing| {
            let prior_state: Option<StoredFrrInterfaceAcquisitionState> = decode_enabled(
                existing.get(&state_key).cloned().flatten(),
                FRR_INTERFACE_ACQUISITION_CLASS,
                "native interface acquisition state",
            )?;
            if let Some(prior) = prior_state.as_ref() {
                validate_state_identity(prior, &attempt.device_id, &attempt.network_namespace)?;
            }
            if let Some(observation) = observation.as_ref() {
                let existing_observation: Option<StoredFrrInterfaceShadowObservation> =
                    decode_enabled(
                        existing.get(&observation.observation_id).cloned().flatten(),
                        FRR_INTERFACE_OBSERVATION_CLASS,
                        "native interface shadow evidence",
                    )?;
                if let Some(existing) = existing_observation.as_ref() {
                    validate_stored_observation(existing)?;
                    let existing_position = (existing.session_epoch, existing.sequence);
                    let candidate_position = (observation.session_epoch, observation.sequence);
                    if candidate_position <= existing_position {
                        bail!(
                            "stale or replayed native interface observation rejected at epoch {} sequence {}",
                            observation.session_epoch,
                            observation.sequence
                        )
                    }
                }
            }

            let prior_attempt: Option<StoredFrrInterfaceAcquisitionAttempt> = decode_enabled(
                existing.get(&attempt_key).cloned().flatten(),
                FRR_INTERFACE_ATTEMPT_CLASS,
                "native interface latest-attempt evidence",
            )?;
            let attempt_is_newer_than_evidence = prior_attempt.as_ref().is_none_or(|prior| {
                (attempt.session_epoch, attempt.sequence) > (prior.session_epoch, prior.sequence)
                    && attempt.started_at_ns > prior.started_at_ns
            });
            let (state, attempt_is_latest) = merge_attempt_state(prior_state, &attempt);
            validate_state_identity(&state, &attempt.device_id, &attempt.network_namespace)?;
            let mut writes = Vec::new();
            if attempt_is_latest && attempt_is_newer_than_evidence {
                writes.push(attempt_config_item(&attempt)?);
            } else if observation.is_none() {
                bail!(
                    "stale or replayed native interface acquisition attempt rejected at epoch {} sequence {}",
                    attempt.session_epoch,
                    attempt.sequence
                )
            }
            if let (Some(observation), Some(content)) = (observation.as_ref(), observation_json) {
                writes.push(observation_config_item(observation, content));
            }
            writes.push(state_config_item(&state)?);
            Ok((writes, ()))
        })
        .await
}

/// Atomically persist one usable shadow observation and its acquisition
/// attempt/state. Degraded payload-only observations must use
/// `persist_degraded_observation` so they cannot replace last-usable evidence.
pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredFrrInterfaceShadowObservation,
) -> Result<String> {
    validate_stored_observation(observation)?;
    if !is_usable_interpretation(&observation.interpretation_outcome) {
        bail!(
            "degraded native interface interpretation '{}' cannot be admitted as usable evidence",
            observation.interpretation_outcome
        )
    }
    let attempt = attempt_from_observation(observation, None, None);
    persist_attempt_transaction(store, Some(observation.clone()), attempt).await?;
    Ok(observation.observation_id.clone())
}

/// Persist bounded degraded-interpretation evidence and attempt state without
/// touching the retained last-usable shadow observation.
pub async fn persist_degraded_observation(
    store: &GraphStore,
    observation: &StoredFrrInterfaceShadowObservation,
    failure_class: &str,
    failure_detail: &str,
) -> Result<String> {
    validate_stored_observation(observation)?;
    if is_usable_interpretation(&observation.interpretation_outcome) {
        bail!("usable native interface observation cannot be persisted as degraded")
    }
    let attempt = attempt_from_observation(observation, Some(failure_class), Some(failure_detail));
    let id = attempt.attempt_id.clone();
    persist_attempt_transaction(store, None, attempt).await?;
    Ok(id)
}

pub async fn record_acquisition_failure(
    store: &GraphStore,
    target: &TargetConfig,
    started_at_ns: i64,
    failure_class: &str,
    failure_detail: &str,
) -> Result<()> {
    let finished_at_ns = crate::graph::common::now_ns();
    let attempt = transport_failure_attempt(
        target,
        started_at_ns,
        finished_at_ns,
        failure_class,
        failure_detail,
    );
    persist_attempt_transaction(store, None, attempt).await
}

pub async fn acquisition_state(
    store: &GraphStore,
    target: &TargetConfig,
) -> Result<Option<StoredFrrInterfaceAcquisitionState>> {
    let device_id = canonical_device_id(target);
    let id = acquisition_state_id(&device_id, "default");
    let Some(item) = store.get_config_item(&id).await? else {
        return Ok(None);
    };
    if item.config_class != FRR_INTERFACE_ACQUISITION_CLASS {
        bail!("native interface acquisition state storage class mismatch")
    }
    if !item.enabled {
        return Ok(None);
    }
    let state: StoredFrrInterfaceAcquisitionState = serde_json::from_str(&item.content_json)
        .context("existing native interface acquisition state is undecodable")?;
    validate_state_identity(&state, &device_id, "default")?;
    Ok(Some(state))
}

pub async fn latest_attempt_evidence(
    store: &GraphStore,
    target: &TargetConfig,
) -> Result<Option<StoredFrrInterfaceAcquisitionAttempt>> {
    let device_id = canonical_device_id(target);
    let id = latest_attempt_id(&device_id, "default");
    let Some(item) = store.get_config_item(&id).await? else {
        return Ok(None);
    };
    if item.config_class != FRR_INTERFACE_ATTEMPT_CLASS {
        bail!("native interface latest-attempt evidence storage class mismatch")
    }
    if !item.enabled {
        return Ok(None);
    }
    let attempt: StoredFrrInterfaceAcquisitionAttempt = serde_json::from_str(&item.content_json)
        .context("existing native interface latest-attempt evidence is undecodable")?;
    if attempt.attempt_id != id
        || attempt.device_id != device_id
        || attempt.network_namespace != "default"
    {
        bail!("native interface latest-attempt identity mismatch")
    }
    Ok(Some(attempt))
}

pub async fn latest_observation(
    store: &GraphStore,
    target: &TargetConfig,
) -> Result<StoredFrrInterfaceShadowObservation> {
    let device_id = canonical_device_id(target);
    let id = observation_id(&device_id, "default");
    let item = store
        .get_config_item(&id)
        .await?
        .filter(|item| item.enabled)
        .context("native interface shadow evidence unavailable")?;
    if item.config_class != FRR_INTERFACE_OBSERVATION_CLASS {
        bail!("native interface shadow evidence storage class mismatch")
    }
    let observation: StoredFrrInterfaceShadowObservation =
        serde_json::from_str(&item.content_json)?;
    if observation.device_id != device_id {
        bail!("native interface observation canonical device identity mismatch")
    }
    validate_stored_observation(&observation)?;
    Ok(observation)
}

/// Explicitly invalidate retained native-interface records when an inventory
/// identity is removed. Existing large shadow payloads are replaced by small
/// tombstones; absent identities do not gain new records. Legacy address-keyed
/// v1 records are deliberately not migrated or reassigned.
pub async fn invalidate_device_native_interface_state(
    store: &GraphStore,
    removed: &TargetConfig,
) -> Result<usize> {
    let device_id = canonical_device_id(removed);
    let namespace = "default";
    let ids = vec![
        (
            observation_id(&device_id, namespace),
            FRR_INTERFACE_OBSERVATION_CLASS,
        ),
        (
            acquisition_state_id(&device_id, namespace),
            FRR_INTERFACE_ACQUISITION_CLASS,
        ),
        (
            latest_attempt_id(&device_id, namespace),
            FRR_INTERFACE_ATTEMPT_CLASS,
        ),
        // Pre-correction records were keyed solely by management address. They
        // are never migrated onto a canonical identity; when that inventory
        // address is removed, tombstone the exact legacy slots as explicit
        // non-transferable evidence lifecycle cleanup.
        (
            legacy_address_observation_id(&removed.primary_ip()),
            FRR_INTERFACE_OBSERVATION_CLASS,
        ),
        (
            legacy_address_acquisition_state_id(&removed.primary_ip()),
            FRR_INTERFACE_ACQUISITION_CLASS,
        ),
    ];
    let now = crate::graph::common::now_ns();
    store
        .compare_write_config_items(
            ids.iter().map(|(id, _)| id.clone()).collect(),
            move |existing| {
                let mut writes = Vec::new();
                for (id, class) in ids.iter() {
                    let Some(Some(item)) = existing.get(id) else {
                        continue;
                    };
                    if !item.enabled || item.content_json.len() < 128 {
                        continue;
                    }
                    writes.push(ConfigItemRecord {
                        id: id.clone(),
                        config_class: (*class).into(),
                        vendor: "frr".into(),
                        name: device_id.clone(),
                        version: "invalidated".into(),
                        content_json: serde_json::json!({
                            "invalidated": true,
                            "reason": "inventory_identity_removed",
                            "device_id": device_id,
                            "network_namespace": namespace,
                            "invalidated_at_ns": now,
                        })
                        .to_string(),
                        enabled: false,
                        created_by: "native-frr-interface-lifecycle".into(),
                    });
                }
                let count = writes.len();
                Ok((writes, count))
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    const FRR_1061_LAB_ARTIFACT: &str = "quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b";

    #[test]
    fn fixed_commands_match_the_approved_contract() {
        assert_eq!(FRR_INTERFACE_COMMAND, "vtysh -c 'show interface json'");
        assert_eq!(FRR_NEIGHBOR_COMMAND, "ip neigh show");
        assert_eq!(FRR_MAC_COMMAND, "bridge fdb show");
        assert_eq!(FRR_VLAN_COMMAND, "bridge -c vlan show");
        assert_eq!(
            FRR_INTERFACE_CAPTURE_CONTRACT,
            "frr_verified_ssh_l2_snapshot_v1"
        );
    }

    fn valid_capture_envelope() -> RawCaptureEnvelope {
        RawCaptureEnvelope {
            host: "192.0.2.1".into(),
            port: 22,
            transport: "verified_ssh_read_only_cli".into(),
            host_key: VerifiedHostKey {
                algorithm: "ssh-ed25519".into(),
                sha256: "SHA256:test".into(),
            },
            version_command: FRR_VERSION_COMMAND.into(),
            interface_command: FRR_INTERFACE_COMMAND.into(),
            version_output: "{}".into(),
            interface_output: "{}".into(),
            neighbor_command: FRR_NEIGHBOR_COMMAND.into(),
            mac_command: FRR_MAC_COMMAND.into(),
            vlan_command: FRR_VLAN_COMMAND.into(),
            neighbor_output: String::new(),
            mac_output: String::new(),
            vlan_output: String::new(),
            release_raw: Some("10.6.1_git".into()),
            device_hostname: Some("frr2".into()),
        }
    }

    #[test]
    fn capture_envelope_requires_the_exact_multifamily_fixed_command_contract() {
        let raw = valid_capture_envelope();
        validate_capture_envelope(&raw, "192.0.2.1").expect("exact fixed-command envelope");

        let mut wrong_command = valid_capture_envelope();
        wrong_command.vlan_command = "bridge vlan show".into();
        let err = validate_capture_envelope(&wrong_command, "192.0.2.1")
            .expect_err("substitute VLAN command must be refused");
        assert!(err.to_string().contains("identity/contract mismatch"));

        let err = validate_capture_envelope(&raw, "192.0.2.2")
            .expect_err("envelope for another host must be refused");
        assert!(err.to_string().contains("identity/contract mismatch"));
    }

    #[test]
    fn capture_envelope_missing_a_family_output_is_not_positive_empty_evidence() {
        let mut value = serde_json::to_value(valid_capture_envelope()).expect("envelope json");
        value
            .as_object_mut()
            .expect("object")
            .remove("neighbor_output");
        let err = match serde_json::from_value::<RawCaptureEnvelope>(value) {
            Err(err) => err,
            Ok(_) => panic!("missing neighbor output must fail envelope decoding"),
        };
        assert!(err.to_string().contains("neighbor_output"));
    }

    #[tokio::test]
    async fn unqualified_software_build_is_refused_before_network_contact() {
        let credential = ResolvedCredential {
            username: "obs".into(),
            password: Zeroizing::new("not-used".into()),
        };
        for build in [None, Some("example.test/frr@sha256:unqualified")] {
            let target = TargetConfig {
                address: "192.0.2.1".into(),
                enabled: true,
                vendor: Some("frr".into()),
                software_build: build.map(str::to_string),
                ..Default::default()
            };
            let admission = try_acquire_capture_admission(&canonical_device_id(&target))
                .expect("test admission");
            let err = collect_normalize_frr_interfaces(
                &target,
                &credential,
                Some(Path::new("/dev/null")),
                "bn02404-test",
                &admission,
            )
            .await
            .expect_err("missing or wrong artifact must refuse before capture");
            assert!(
                err.to_string().contains("exact qualified software build"),
                "{err:?}"
            );
        }
    }

    #[tokio::test]
    async fn helper_deadline_terminates_and_reaps_a_hung_child() {
        let mut cmd = tokio::process::Command::new("python3");
        cmd.kill_on_drop(true)
            .arg("-c")
            .arg("import time; time.sleep(30)")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let err = run_capture_helper_bounded(&mut cmd, std::time::Duration::from_millis(50))
            .await
            .err()
            .expect("hung child must hit the absolute deadline");
        assert!(err.to_string().contains("deadline exceeded"), "{err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "deadline kill/reap did not return promptly"
        );
    }

    #[tokio::test]
    async fn helper_output_limit_is_an_explicit_refusal() {
        let mut cmd = tokio::process::Command::new("python3");
        cmd.kill_on_drop(true)
            .arg("-c")
            .arg(
                "import sys; chunk='x'*65536; [sys.stdout.write(chunk) for _ in range((32*1024*1024)//65536+1)]",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let err = run_capture_helper_bounded(&mut cmd, std::time::Duration::from_secs(10))
            .await
            .err()
            .expect("oversized helper output must be refused");
        assert!(
            err.to_string().contains("stdout exceeds bounded retention"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn helper_stderr_overflow_is_an_explicit_bounded_refusal() {
        let mut cmd = tokio::process::Command::new("python3");
        cmd.kill_on_drop(true)
            .arg("-c")
            .arg("import sys; chunk='x'*65536; [sys.stderr.write(chunk) for _ in range(2)]")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let err = run_capture_helper_bounded(&mut cmd, std::time::Duration::from_secs(10))
            .await
            .err()
            .expect("oversized helper stderr must be refused");
        assert!(
            err.to_string().contains("stderr exceeds bounded retention"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn helper_deadline_covers_exit_with_inherited_output_pipe_open() {
        let mut cmd = tokio::process::Command::new("python3");
        cmd.kill_on_drop(true)
            .arg("-c")
            .arg(
                "import subprocess,sys; subprocess.Popen(['python3','-c','import time; time.sleep(2)'], stdout=sys.stdout, stderr=sys.stderr); sys.exit(0)",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let err = run_capture_helper_bounded(&mut cmd, std::time::Duration::from_millis(100))
            .await
            .err()
            .expect("inherited open pipe must not bypass the absolute deadline");
        assert!(err.to_string().contains("deadline exceeded"), "{err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "reader tasks must be aborted rather than detached indefinitely"
        );
    }

    #[tokio::test]
    async fn sequential_device_admission_does_not_retain_inventory_churn_keys() {
        for i in 0..64 {
            let key = format!("bn02404-admission-churn-{i}");
            let first = try_acquire_capture_admission(&key).expect("first admission");
            assert!(
                try_acquire_capture_admission(&key).is_err(),
                "same canonical device acquisition must be non-queueing while active"
            );
            drop(first);
            assert!(
                !device_capture_permit_key_present(&key),
                "completed admission must evict canonical device key {key}"
            );
        }
    }

    /// Genuine authorized-lab acceptance for the exact fixed SSH capture and
    /// qualified shadow dispatch path. This is intentionally ignored in
    /// hermetic CI: it requires the dedicated lab device, a pinned host key
    /// and task-local credentials supplied through the environment.
    #[tokio::test]
    #[ignore = "requires authorized live FRR lab and pinned SSH host key"]
    async fn live_frr_ssh_capture_dispatches_validated_shadow_facts() {
        let host = std::env::var("BONSAI_FRR_LIVE_HOST").expect("live host");
        let username = std::env::var("BONSAI_FRR_LIVE_USERNAME").expect("live username");
        let password = std::env::var("BONSAI_FRR_LIVE_PASSWORD").expect("live password");
        let known_hosts = std::env::var("BONSAI_FRR_LIVE_KNOWN_HOSTS").expect("known hosts");
        let out_path = std::env::var("BONSAI_FRR_LIVE_EVIDENCE").expect("evidence output path");
        let target = TargetConfig {
            address: host.clone(),
            enabled: true,
            vendor: Some("frr".into()),
            hostname: Some("frr2".into()),
            external_id: Some("containerlab:clab-bonsai-bn003-vrf-frr2".into()),
            software_build: Some(FRR_1061_LAB_ARTIFACT.into()),
            ..Default::default()
        };
        let credential = ResolvedCredential {
            username,
            password: Zeroizing::new(password),
        };
        let admission = try_acquire_capture_admission(&canonical_device_id(&target))
            .expect("live test admission");
        let observation = collect_normalize_frr_interfaces(
            &target,
            &credential,
            Some(Path::new(&known_hosts)),
            "bn02404-live-test",
            &admission,
        )
        .await
        .expect("live capture and qualified dispatch");
        assert_eq!(observation.device_address, host);
        assert_eq!(observation.transport, "verified_ssh_read_only_cli");
        assert_eq!(observation.interface_command, FRR_INTERFACE_COMMAND);
        assert_eq!(observation.release_raw, "10.6.1_git");
        assert_eq!(
            observation.release_build.as_deref(),
            Some(FRR_1061_LAB_ARTIFACT)
        );
        assert_eq!(observation.network_namespace, "default");
        assert_eq!(observation.qualification, "Qualified");
        assert!(observation.shadow_only);
        assert_eq!(observation.facts_emitted, 6);
        assert_eq!(observation.facts_validated, 6);
        assert_eq!(observation.facts_retained, 6);
        assert!(
            observation
                .facts
                .iter()
                .any(|fact| fact["fact_kind"] == "interface.state")
        );
        assert!(
            observation
                .facts
                .iter()
                .any(|fact| fact["fact_kind"] == "mac.forwarding")
        );
        assert!(
            observation.normalization_report["feature_families"]
                .as_array()
                .is_some_and(|f| f.len() == 4)
        );
        assert!(!observation.fact_retention_truncated);
        assert_eq!(observation.normalization_report["entities_accepted"], 4);
        assert_eq!(
            observation.normalization_report["expected_interface_total"],
            serde_json::Value::Null
        );
        assert!(observation.raw_interface_json.is_some());
        std::fs::write(&out_path, serde_json::to_vec_pretty(&observation).unwrap()).unwrap();
    }
}

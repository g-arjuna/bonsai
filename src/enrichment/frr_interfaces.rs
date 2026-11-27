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
    semantic_evidence::{
        AcquisitionScopeContract, ScopedCaptureCoverage, ScopedPopulationCoverage,
    },
    typed_facts::PayloadState,
    vlan_normalizer::{VlanObservation, VlanSourceProfile, normalize_vlan},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
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
pub const FRR_ARPND_FEATURE: &str = "arpnd.linux_ip_neigh";
pub const FRR_MAC_FEATURE: &str = "mac.linux_bridge_fdb";
pub const FRR_VLAN_FEATURE: &str = "vlan.linux_bridge_vlan";
pub const FRR_L2_CAPTURE_CONTRACT: &str = "frr_verified_ssh_l2_snapshot_v1";
pub const FRR_INTERFACE_CAPTURE_CONTRACT: &str = FRR_L2_CAPTURE_CONTRACT;
const CAPTURE_DEADLINE_SECS: u64 = 45;
const MAX_CONCURRENT_CAPTURES: usize = 2;
const MAX_STORED_FACTS: usize = 256;
/// Deterministic per-family retention share. No single family can consume
/// the aggregate shadow-evidence budget and starve independently acquired
/// families. 4 families × 64 facts exactly fills the aggregate 256-fact bound.
const MAX_STORED_FACTS_PER_FAMILY: usize = 64;
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CommandExecutionMetadata {
    pub command_key: String,
    pub command: String,
    /// `complete`, `failed`, `timed_out`, `unsupported`, `truncated` or
    /// `not_attempted`. This is execution evidence only; a missing output is
    /// never represented as a successful empty capture.
    pub outcome: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub started_at_ns: Option<i64>,
    #[serde(default)]
    pub completed_at_ns: Option<i64>,
    #[serde(default)]
    pub output_bytes: Option<usize>,
    #[serde(default)]
    pub output_sha256: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FamilyAcquisitionAttemptState {
    pub outcome: String,
    #[serde(default)]
    pub normalization_outcome: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub started_at_ns: Option<i64>,
    #[serde(default)]
    pub completed_at_ns: Option<i64>,
    #[serde(default)]
    pub output_bytes: Option<usize>,
    #[serde(default)]
    pub output_sha256: Option<String>,
    #[serde(default)]
    pub capture_id: Option<String>,
    #[serde(default)]
    pub session_epoch: Option<u64>,
    #[serde(default)]
    pub sequence: Option<u64>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub facts_emitted: usize,
    #[serde(default)]
    pub facts_validated: usize,
    #[serde(default)]
    pub report: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FamilyRetainedEvidenceState {
    /// `current` means this evidence came from the latest family acquisition;
    /// `last_good` means an older accepted population was preserved after a
    /// newer failed or partial family acquisition. `partial_current` is bounded
    /// first-use evidence that has not yet established a complete last-good
    /// population.
    pub evidence_state: String,
    pub capture_id: String,
    pub session_epoch: u64,
    pub sequence: u64,
    pub observed_at: String,
    #[serde(default)]
    pub output_bytes: Option<usize>,
    #[serde(default)]
    pub output_sha256: Option<String>,
    #[serde(default)]
    pub raw_output_retained: bool,
    #[serde(default)]
    pub raw_output_omitted_reason: Option<String>,
    #[serde(default)]
    pub facts_emitted: usize,
    #[serde(default)]
    pub facts_validated: usize,
    #[serde(default)]
    pub facts_retained: usize,
    #[serde(default)]
    pub facts_omitted_by_retention: usize,
    #[serde(default)]
    pub retention_truncated: bool,
    #[serde(default)]
    pub positive_empty_population: bool,
    #[serde(default)]
    pub coverage: Option<serde_json::Value>,
    #[serde(default)]
    pub report: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoredFeatureFamilyState {
    pub feature: String,
    pub fact_kind: String,
    #[serde(default)]
    pub support_state: String,
    pub latest_attempt: FamilyAcquisitionAttemptState,
    #[serde(default)]
    pub retained: Option<FamilyRetainedEvidenceState>,
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
    /// Per-command execution timing/outcome metadata for the acquisition
    /// batch. The batch is sequential and explicitly non-atomic.
    #[serde(default)]
    pub command_execution: Vec<CommandExecutionMetadata>,
    /// Authoritative per-family latest-attempt and retained-evidence lifecycle
    /// state. The aggregate `facts` vector is a union of the retained family
    /// populations, not a claim that every family was freshly acquired at the
    /// aggregate capture timestamp.
    #[serde(default)]
    pub family_states: Vec<StoredFeatureFamilyState>,
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
    pub family_states: Vec<StoredFeatureFamilyState>,
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
    /// Restart-durable per-family lifecycle summary copied from the latest
    /// admitted attempt. Last-good evidence itself remains in the shadow
    /// observation record; this field prevents process restart from erasing
    /// the distinction between current, failed, partial and retained evidence.
    #[serde(default)]
    pub family_states: Vec<StoredFeatureFamilyState>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCommandCaptureEnvelope {
    pub command_key: String,
    pub command: String,
    pub outcome: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub started_at_ns: Option<i64>,
    #[serde(default)]
    pub completed_at_ns: Option<i64>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub output_bytes: Option<usize>,
    #[serde(default)]
    pub output_sha256: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub stderr_bytes: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAcquisitionBatchEnvelope {
    pub started_at: String,
    pub started_at_ns: i64,
    pub completed_at: String,
    pub completed_at_ns: i64,
    pub deadline_seconds: u64,
    pub atomic_snapshot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCaptureEnvelope {
    pub host: String,
    pub port: u16,
    pub transport: String,
    pub host_key: VerifiedHostKey,
    pub acquisition_batch: RawAcquisitionBatchEnvelope,
    pub commands: BTreeMap<String, RawCommandCaptureEnvelope>,
    pub release_raw: Option<String>,
    pub device_hostname: Option<String>,
}

impl RawCaptureEnvelope {
    pub fn command(&self, key: &str) -> Option<&RawCommandCaptureEnvelope> {
        self.commands.get(key)
    }
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

fn sha256_text(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn expected_family_command(key: &str) -> &'static str {
    match key {
        "version" => FRR_VERSION_COMMAND,
        "interfaces" => FRR_INTERFACE_COMMAND,
        "arpnd" => FRR_NEIGHBOR_COMMAND,
        "mac" => FRR_MAC_COMMAND,
        "vlan" => FRR_VLAN_COMMAND,
        _ => "",
    }
}

fn validate_command_capture(raw: &RawCaptureEnvelope, key: &str) -> Result<()> {
    let command = raw
        .command(key)
        .ok_or_else(|| anyhow::anyhow!("frr capture envelope is missing command slot {key}"))?;
    if command.command_key != key || command.command != expected_family_command(key) {
        bail!("frr interface capture envelope identity/contract mismatch")
    }
    let valid_outcome = matches!(
        command.outcome.as_str(),
        "complete" | "failed" | "timed_out" | "unsupported" | "truncated" | "not_attempted"
    );
    if !valid_outcome {
        bail!("frr capture envelope command {key} has an unknown execution outcome")
    }
    if command.started_at.is_none()
        || command.completed_at.is_none()
        || command.started_at_ns.is_none()
        || command.completed_at_ns.is_none()
    {
        bail!("frr capture envelope command {key} lacks execution timing evidence")
    }
    if let Some(output) = command.output.as_ref() {
        if output.len() > 4 * 1024 * 1024 {
            bail!("frr interface capture exceeds helper bounds")
        }
        if command.output_bytes != Some(output.len())
            || command.output_sha256.as_deref() != Some(sha256_text(output).as_str())
        {
            bail!("frr capture envelope command {key} output digest/count mismatch")
        }
    } else if command.output_bytes.is_some() || command.output_sha256.is_some() {
        bail!("frr capture envelope command {key} has output metadata without output bytes")
    }
    if command.outcome == "complete" && command.output.is_none() {
        bail!("frr capture envelope command {key} is missing successful output")
    }
    Ok(())
}

fn validate_capture_envelope(raw: &RawCaptureEnvelope, address: &str) -> Result<()> {
    if raw.host != address
        || raw.port != 22
        || raw.transport != "verified_ssh_read_only_cli"
        || raw.acquisition_batch.atomic_snapshot
        || raw.acquisition_batch.deadline_seconds == 0
        || raw.acquisition_batch.deadline_seconds > CAPTURE_DEADLINE_SECS
    {
        bail!("frr interface capture envelope identity/contract mismatch")
    }
    for key in ["version", "interfaces", "arpnd", "mac", "vlan"] {
        validate_command_capture(raw, key)?;
    }
    let version = raw.command("version").expect("validated version slot");
    if version.outcome != "complete" || version.output.as_ref().is_none_or(|o| o.trim().is_empty())
    {
        bail!("mandatory FRR version/release identity capture did not complete")
    }
    Ok(())
}

fn capture_outcome_for_execution(command: &RawCommandCaptureEnvelope) -> CaptureOutcome {
    match command.outcome.as_str() {
        "complete" => CaptureOutcome::Complete,
        "timed_out" => CaptureOutcome::TimedOut,
        "unsupported" => CaptureOutcome::Unsupported,
        "truncated" => CaptureOutcome::Truncated,
        "not_attempted" => CaptureOutcome::Unavailable,
        _ => CaptureOutcome::Failed,
    }
}

fn execution_metadata(command: &RawCommandCaptureEnvelope) -> CommandExecutionMetadata {
    CommandExecutionMetadata {
        command_key: command.command_key.clone(),
        command: command.command.clone(),
        outcome: command.outcome.clone(),
        started_at: command.started_at.clone(),
        completed_at: command.completed_at.clone(),
        started_at_ns: command.started_at_ns,
        completed_at_ns: command.completed_at_ns,
        output_bytes: command.output_bytes,
        output_sha256: command.output_sha256.clone(),
        detail: command.detail.clone(),
    }
}

fn family_context<'a>(
    device_id: &'a str,
    collector_id: &'a str,
    capture_id: &'a str,
    observed_at: &'a str,
    session_epoch: u64,
    sequence: u64,
    namespace: &'a str,
    release_raw: &'a str,
    release_build: Option<&'a str>,
) -> ObservationContext<'a> {
    let mut context = ObservationContext::minimal(device_id, collector_id, capture_id, observed_at);
    context.session_epoch = session_epoch;
    context.sequence = sequence;
    context.network_namespace = Some(namespace);
    context.platform = Some("frr");
    context.release_raw = Some(release_raw);
    context.release_build = release_build;
    context
}

fn validate_family_profile_boundary(
    receipt: &crate::nos_features::SelectionReceipt,
    feature: &str,
    namespace: &str,
    release: &SoftwareRelease,
    expected_normalizer: &str,
    expected_parser_revision: &str,
    expected_mapping_revision: &str,
    required_contract: &str,
) -> Result<()> {
    let crate::nos_features::Selection::Selected(profile) = receipt.selection() else {
        bail!("feature {feature} has no executable selected NOS profile")
    };
    let errors = crate::nos_features::ProfileRegistry::validate_profile(profile);
    if !errors.is_empty() {
        bail!(
            "feature {feature} profile is invalid: {}",
            errors.join("; ")
        )
    }
    let status = crate::nos_features::ProfileRegistry::feature_status_in_namespace(
        profile, feature, namespace,
    );
    if status != crate::nos_features::FeatureStatus::Enabled {
        bail!("feature {feature} is not applicable in namespace {namespace}: {status:?}")
    }
    let closure = crate::nos_features::ProfileRegistry::dependency_closure(profile, feature);
    if let Err(errors) = crate::nos_features::ProfileRegistry::validate_feature_set_in_scope(
        profile, &closure, namespace,
    ) {
        bail!(
            "feature {feature} composition is invalid: {}",
            errors.join("; ")
        )
    }
    let flag = profile
        .features
        .get(feature)
        .expect("feature status checked");
    let normalizer = flag
        .normalizer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("feature {feature} has no normalizer binding"))?;
    if normalizer.id != expected_normalizer
        || normalizer.revision != expected_parser_revision
        || normalizer.mapping_revision.as_deref() != Some(expected_mapping_revision)
    {
        bail!("feature {feature} parser or mapping revision is not the registered implementation")
    }
    let contract_ok = flag.acquisition.iter().any(|contract| {
        contract.kind == crate::nos_features::AcquisitionKind::CliObservation
            && contract
                .contracts
                .iter()
                .any(|item| item == required_contract)
    });
    if !contract_ok {
        bail!("feature {feature} does not declare the required acquisition source contract")
    }
    if receipt.canonical_platform() != "frr" {
        bail!("feature {feature} selection receipt is bound to a different canonical platform")
    }
    let constraint = profile.release_constraint.as_ref().ok_or_else(|| {
        anyhow::anyhow!("feature {feature} profile has no exact release constraint")
    })?;
    if constraint.is_empty()
        || !constraint.matches(Some(release))
        || profile.verification != "release_qualified"
    {
        bail!(
            "feature {feature} actual release evidence does not satisfy the exact qualified artifact constraint"
        )
    }
    if !profile.hardware_constraints.is_empty() {
        // The narrow Linux/FRR profile currently has no hardware constraints.
        // If one is introduced, this collector must implement the complete
        // hardware binding rather than weakening it here.
        bail!("feature {feature} has hardware constraints this collector does not implement")
    }
    Ok(())
}

fn planned_capture_families(release_build: &str) -> Result<Vec<&'static str>> {
    let release = SoftwareRelease {
        raw: "10.6.1_git".into(),
        train: None,
        build: Some(release_build.to_string()),
        components: BTreeMap::new(),
    };
    let profiles = builtin_registry();
    let receipt = profiles.select_receipt(&ProfileQuery {
        platform: "frr".into(),
        version: release.raw.clone(),
        release: Some(release.clone()),
        hardware: Some(HardwareScope {
            namespace: Some("default".into()),
            ..Default::default()
        }),
    });
    let mut families = Vec::new();
    for (family, feature, normalizer, parser, mapping, contract) in [
        (
            "interfaces",
            FRR_INTERFACE_FEATURE,
            "interface_frr_json_normalizer",
            crate::frr_interface_json::FRR_JSON_PARSER_REVISION,
            crate::frr_interface_json::FRR_JSON_MAPPING_REVISION,
            FRR_JSON_ACQUISITION_CONTRACT,
        ),
        (
            "arpnd",
            FRR_ARPND_FEATURE,
            "arpnd_linux_ip_neigh_normalizer",
            crate::neighbor_normalizer::NEIGHBOR_PARSER_REVISION,
            crate::neighbor_normalizer::NEIGHBOR_MAPPING_REVISION,
            "linux_ip_neigh_show",
        ),
        (
            "mac",
            FRR_MAC_FEATURE,
            "mac_linux_bridge_fdb_normalizer",
            crate::neighbor_normalizer::NEIGHBOR_PARSER_REVISION,
            crate::neighbor_normalizer::NEIGHBOR_MAPPING_REVISION,
            "linux_bridge_fdb_show",
        ),
        (
            "vlan",
            FRR_VLAN_FEATURE,
            "vlan_linux_bridge_vlan_normalizer",
            crate::vlan_normalizer::VLAN_PARSER_REVISION,
            crate::vlan_normalizer::VLAN_MAPPING_REVISION,
            "linux_bridge_c_vlan_show",
        ),
    ] {
        if validate_family_profile_boundary(
            &receipt, feature, "default", &release, normalizer, parser, mapping, contract,
        )
        .is_ok()
        {
            families.push(family);
        }
    }
    if families.is_empty() {
        bail!("no approved FRR feature family is applicable for the qualified artifact")
    }
    Ok(families)
}

fn population_coverage_from_parts(
    capture_outcome: CaptureOutcome,
    positive_empty: bool,
    malformed: usize,
    omitted: usize,
    rejected: usize,
    refusal: bool,
) -> ScopedPopulationCoverage {
    if refusal
        || capture_outcome == CaptureOutcome::Unknown
        || capture_outcome == CaptureOutcome::Unavailable
    {
        ScopedPopulationCoverage::Unknown
    } else if capture_outcome == CaptureOutcome::Complete
        && malformed == 0
        && omitted == 0
        && rejected == 0
    {
        ScopedPopulationCoverage::Complete
    } else if positive_empty && capture_outcome == CaptureOutcome::Complete {
        ScopedPopulationCoverage::Complete
    } else {
        ScopedPopulationCoverage::Partial
    }
}

fn non_interface_family_outcome(
    capture_outcome: CaptureOutcome,
    positive_empty: bool,
    usable_entity_facts: usize,
    malformed: usize,
    omitted: usize,
    rejected: usize,
    refusal: Option<&str>,
) -> &'static str {
    if refusal.is_some() {
        return if capture_outcome.usable() {
            "invalid_input"
        } else {
            "capture_failed"
        };
    }
    if positive_empty
        && capture_outcome == CaptureOutcome::Complete
        && malformed == 0
        && omitted == 0
        && rejected == 0
    {
        return "genuine_complete_empty";
    }
    if usable_entity_facts == 0 {
        return if capture_outcome == CaptureOutcome::Complete {
            "no_usable_records"
        } else {
            "capture_failed"
        };
    }
    if capture_outcome != CaptureOutcome::Complete || malformed > 0 || omitted > 0 || rejected > 0 {
        "partial_usable"
    } else {
        "complete_usable"
    }
}

fn family_attempt_state(
    command: &RawCommandCaptureEnvelope,
    normalization_outcome: Option<&str>,
    capture_id: &str,
    session_epoch: u64,
    sequence: u64,
    facts_emitted: usize,
    facts_validated: usize,
    report: serde_json::Value,
) -> FamilyAcquisitionAttemptState {
    FamilyAcquisitionAttemptState {
        outcome: command.outcome.clone(),
        normalization_outcome: normalization_outcome.map(str::to_string),
        command: Some(command.command.clone()),
        started_at: command.started_at.clone(),
        completed_at: command.completed_at.clone(),
        started_at_ns: command.started_at_ns,
        completed_at_ns: command.completed_at_ns,
        output_bytes: command.output_bytes,
        output_sha256: command.output_sha256.clone(),
        capture_id: Some(capture_id.to_string()),
        session_epoch: Some(session_epoch),
        sequence: Some(sequence),
        detail: command.detail.clone(),
        facts_emitted,
        facts_validated,
        report: Some(report),
    }
}

fn coverage_json_with_retention(
    coverage: &ScopedCaptureCoverage,
    emitted_population_facts: usize,
    retained_population_facts: usize,
) -> Result<serde_json::Value> {
    let mut adjusted = coverage.clone();
    let omitted = emitted_population_facts.saturating_sub(retained_population_facts);
    if omitted > 0 {
        adjusted.omitted_records = adjusted.omitted_records.saturating_add(omitted);
        if adjusted.population_coverage == ScopedPopulationCoverage::Complete {
            adjusted.population_coverage = ScopedPopulationCoverage::Partial;
        }
    }
    Ok(serde_json::to_value(adjusted)?)
}

fn family_retained_state(
    command: &RawCommandCaptureEnvelope,
    outcome: &str,
    observed_at: &str,
    capture_id: &str,
    session_epoch: u64,
    sequence: u64,
    facts_emitted: usize,
    facts_validated: usize,
    facts_retained: usize,
    facts_omitted_by_retention: usize,
    positive_empty: bool,
    coverage: serde_json::Value,
    report: serde_json::Value,
    raw_within_retention: bool,
    raw_omitted_reason: Option<String>,
) -> Option<FamilyRetainedEvidenceState> {
    if !matches!(
        outcome,
        "complete_usable" | "partial_usable" | "genuine_complete_empty"
    ) {
        return None;
    }
    Some(FamilyRetainedEvidenceState {
        evidence_state: if outcome == "partial_usable" {
            "partial_current"
        } else {
            "current"
        }
        .into(),
        capture_id: capture_id.to_string(),
        session_epoch,
        sequence,
        observed_at: observed_at.to_string(),
        output_bytes: command.output_bytes,
        output_sha256: command.output_sha256.clone(),
        raw_output_retained: raw_within_retention,
        raw_output_omitted_reason: raw_omitted_reason,
        facts_emitted,
        facts_validated,
        facts_retained,
        facts_omitted_by_retention,
        retention_truncated: facts_omitted_by_retention > 0,
        positive_empty_population: positive_empty,
        coverage: Some(coverage),
        report: Some(report),
    })
}

struct AssembledFamily {
    feature: &'static str,
    fact_kind: &'static str,
    sequence: u64,
    state: StoredFeatureFamilyState,
    retained_facts: Vec<crate::typed_facts::TypedFact>,
}

fn cap_family_facts(
    facts: &[crate::typed_facts::TypedFact],
) -> (Vec<crate::typed_facts::TypedFact>, usize) {
    // Diagnostic status facts are useful normalization output, but retained
    // family evidence is the bounded entity population. In particular, a
    // genuine complete-empty population is represented by scoped coverage and
    // zero retained entity facts, not by a synthetic empty entity row.
    let population_facts: Vec<_> = facts
        .iter()
        .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
        .cloned()
        .collect();
    let retained: Vec<_> = population_facts
        .iter()
        .take(MAX_STORED_FACTS_PER_FAMILY)
        .cloned()
        .collect();
    let omitted = population_facts.len().saturating_sub(retained.len());
    (retained, omitted)
}

fn retained_coverage_bound(
    coverage: &ScopedCaptureCoverage,
    observation: &StoredFrrInterfaceShadowObservation,
    retained: &FamilyRetainedEvidenceState,
) -> Result<()> {
    if coverage.device_id != observation.device_id
        || coverage.collector_id != observation.collector_id
        || coverage.network_namespace.as_deref() != Some(observation.network_namespace.as_str())
        || coverage.capture_id != retained.capture_id
        || coverage.source_generation != Some(retained.session_epoch)
        || coverage.observation_time.as_deref() != Some(retained.observed_at.as_str())
    {
        bail!("native interface family coverage identity does not match retained evidence")
    }
    if let Some(digest) = retained.output_sha256.as_ref() {
        if !coverage
            .evidence_refs
            .iter()
            .any(|reference| reference.kind == "raw_output" && &reference.sha256 == digest)
        {
            bail!("native interface family coverage is not bound to the retained raw-output digest")
        }
    }
    if retained.retention_truncated
        && coverage.population_coverage == ScopedPopulationCoverage::Complete
    {
        bail!("retention-truncated family coverage cannot claim a complete retained population")
    }
    Ok(())
}

/// Inputs needed to assemble the production capture envelope into the same
/// shadow observation used by live SSH acquisition and hermetic replay tests.
/// This is the single production envelope-to-observation constructor; tests
/// must not hand-assemble a parallel multifamily observation shape.
pub struct CaptureEnvelopeAssemblyInput {
    pub device_id: String,
    pub device_address: String,
    pub collector_id: String,
    pub release_build: Option<String>,
    pub acquisition_started_at_ns: i64,
    pub acquisition_classification: String,
}

fn unsupported_family(
    feature: &'static str,
    fact_kind: &'static str,
    sequence: u64,
    command: &RawCommandCaptureEnvelope,
    capture_id: &str,
    session_epoch: u64,
    observed_at: &str,
    device_id: &str,
    collector_id: &str,
    namespace: &str,
    detail: String,
) -> AssembledFamily {
    let outcome = if command.outcome == "not_attempted" {
        "unsupported"
    } else {
        "unsupported"
    };
    let _coverage = ScopedCaptureCoverage {
        device_id: device_id.to_string(),
        scope: None,
        network_namespace: Some(namespace.to_string()),
        collector_id: collector_id.to_string(),
        source_kind: "cli_text".into(),
        capture_id: capture_id.to_string(),
        source_generation: Some(session_epoch),
        acquisition_scope: feature.to_string(),
        covered_fact_kind: fact_kind.to_string(),
        declared_scope: AcquisitionScopeContract::Unknown,
        capture_outcome: if command.outcome == "not_attempted" {
            CaptureOutcome::Unavailable
        } else {
            CaptureOutcome::Unsupported
        },
        population_coverage: ScopedPopulationCoverage::Unknown,
        positive_empty_population: false,
        observation_time: Some(observed_at.to_string()),
        valid_until: None,
        observed_interfaces: BTreeSet::new(),
        observed_forwarding_domains: BTreeSet::new(),
        observed_vlan_ids: BTreeSet::new(),
        records_decoded: 0,
        malformed_records: 0,
        omitted_records: 0,
        rejected_records: 0,
        evidence_refs: command
            .output_sha256
            .as_ref()
            .map(|sha256| {
                vec![crate::typed_facts::EvidenceRef {
                    kind: "raw_output".into(),
                    sha256: sha256.clone(),
                    locator: Some(command.command.clone()),
                }]
            })
            .unwrap_or_default(),
    };
    let report = serde_json::json!({
        "capture_outcome": command.outcome,
        "records_decoded": 0,
        "facts_emitted": 0,
        "refusal_reason": detail,
    });
    let attempt = family_attempt_state(
        command,
        Some(outcome),
        capture_id,
        session_epoch,
        sequence,
        0,
        0,
        report.clone(),
    );
    AssembledFamily {
        feature,
        fact_kind,
        sequence,
        state: StoredFeatureFamilyState {
            feature: feature.into(),
            fact_kind: fact_kind.into(),
            support_state: "unsupported".into(),
            latest_attempt: attempt,
            retained: None,
        },
        retained_facts: Vec::new(),
    }
}

/// Production envelope-to-observation constructor. Live SSH acquisition,
/// genuine capture replay and hermetic integration tests all use this exact
/// function so multifamily assembly cannot diverge into a test-only parallel
/// implementation.
pub fn observation_from_capture_envelope(
    raw: RawCaptureEnvelope,
    input: CaptureEnvelopeAssemblyInput,
) -> Result<StoredFrrInterfaceShadowObservation> {
    validate_capture_envelope(&raw, &input.device_address)?;
    let version_command = raw.command("version").expect("validated version").clone();
    let version_output = version_command
        .output
        .clone()
        .expect("validated version output");
    if version_output.len() > 4 * 1024 * 1024 {
        bail!("frr version capture exceeds helper bounds")
    }
    let release_raw = raw
        .release_raw
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .context("frr version evidence is unavailable")?
        .to_string();
    let namespace = "default";
    let release = SoftwareRelease {
        raw: release_raw.clone(),
        train: None,
        build: input.release_build.clone(),
        components: BTreeMap::new(),
    };
    let profiles = builtin_registry();
    let receipt = profiles.select_receipt(&ProfileQuery {
        platform: "frr".into(),
        version: release_raw.clone(),
        release: Some(release.clone()),
        hardware: Some(HardwareScope {
            namespace: Some(namespace.into()),
            ..Default::default()
        }),
    });
    let captured_at_ns = crate::graph::common::now_ns();
    let captured_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let capture_id = format!("frr-l2-capture-{}", raw.acquisition_batch.started_at_ns);
    // The numeric generation is a monotonic acquisition-ordering token. It is
    // deliberately not evidence that each command batch was a new transport
    // session generation; that limitation is recorded in the report below.
    let session_epoch = input.acquisition_started_at_ns as u64;
    let device_id = input.device_id;
    let collector_id = input.collector_id;

    let interface_command = raw
        .command("interfaces")
        .expect("validated interface slot")
        .clone();
    let neighbor_command = raw.command("arpnd").expect("validated arp slot").clone();
    let mac_command = raw.command("mac").expect("validated mac slot").clone();
    let vlan_command = raw.command("vlan").expect("validated vlan slot").clone();

    // Interface family: the existing qualified registry dispatch remains the
    // authority for this feature. Per-command completion time, not one batch
    // completion timestamp, is the observation time.
    let iface_output = interface_command.output.clone().unwrap_or_default();
    let iface_observed_at = interface_command
        .completed_at
        .clone()
        .unwrap_or_else(|| captured_at.clone());
    let iface_capture_outcome = capture_outcome_for_execution(&interface_command);
    let iface_capture =
        CommandCapture::new(&iface_output, iface_capture_outcome).captured_at(&iface_observed_at);
    let iface_context = family_context(
        &device_id,
        &collector_id,
        &capture_id,
        &iface_observed_at,
        session_epoch,
        1,
        namespace,
        &release_raw,
        input.release_build.as_deref(),
    );
    let interface_observation = FrrInterfaceJsonObservation {
        capture: iface_capture,
        context: iface_context,
        acquisition_contract: FRR_JSON_ACQUISITION_CONTRACT,
    };
    let registry = NormalizerRegistry::builtin();
    let run = registry
        .dispatch_input(
            &receipt,
            FRR_INTERFACE_FEATURE,
            InterfaceObservationInput::FrrJson(&interface_observation),
        )
        .map_err(|e| anyhow::anyhow!("frr interface shadow dispatch refused: {e:?}"))?;
    let (interface_outcome, _, _, _) = classify_interpretation(&run.output);
    let interface_entity_facts = run
        .output
        .facts
        .iter()
        .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
        .count();
    let interface_positive_empty = interface_outcome == "genuine_complete_empty";
    let interface_malformed = run.output.report.invalid_entities + run.output.report.unparsed_lines;
    let interface_omitted =
        run.output.report.entities_omitted + run.output.report.output_facts_omitted;
    let interface_refused = run.output.report.context_error.is_some();
    let interface_coverage = ScopedCaptureCoverage {
        device_id: device_id.clone(),
        scope: None,
        network_namespace: Some(namespace.into()),
        collector_id: collector_id.clone(),
        source_kind: "structured_cli_json".into(),
        capture_id: capture_id.clone(),
        source_generation: Some(session_epoch),
        acquisition_scope: "frr/show-interface-json".into(),
        covered_fact_kind: "interface.state".into(),
        declared_scope: AcquisitionScopeContract::CompleteScopedTable,
        capture_outcome: iface_capture_outcome,
        population_coverage: population_coverage_from_parts(
            iface_capture_outcome,
            interface_positive_empty,
            interface_malformed,
            interface_omitted,
            run.output.report.facts_rejected,
            interface_refused,
        ),
        positive_empty_population: interface_positive_empty,
        observation_time: Some(iface_observed_at.clone()),
        valid_until: None,
        observed_interfaces: run
            .output
            .facts
            .iter()
            .filter_map(|fact| fact.subject.entity.clone())
            .collect(),
        observed_forwarding_domains: BTreeSet::new(),
        observed_vlan_ids: BTreeSet::new(),
        records_decoded: run
            .output
            .report
            .records_decoded
            .unwrap_or(run.output.report.interfaces_observed),
        malformed_records: interface_malformed,
        omitted_records: interface_omitted,
        rejected_records: run.output.report.facts_rejected,
        evidence_refs: interface_command
            .output_sha256
            .as_ref()
            .map(|sha256| {
                vec![crate::typed_facts::EvidenceRef {
                    kind: "raw_output".into(),
                    sha256: sha256.clone(),
                    locator: Some("frr/show-interface-json".into()),
                }]
            })
            .unwrap_or_default(),
    };
    let (interface_retained_facts, interface_retention_omitted) =
        cap_family_facts(&run.output.facts);
    let interface_retained_population = interface_retained_facts
        .iter()
        .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
        .count();
    let interface_coverage_json = coverage_json_with_retention(
        &interface_coverage,
        interface_entity_facts,
        interface_retained_population,
    )?;
    let interface_report_json = serde_json::to_value(&run.output.report)?;
    let interface_raw_within = iface_output.len() <= MAX_STORED_RAW_BYTES;
    let interface_retained = family_retained_state(
        &interface_command,
        interface_outcome,
        &iface_observed_at,
        &capture_id,
        session_epoch,
        1,
        run.output.facts.len(),
        run.output.facts.len(),
        interface_retained_facts.len(),
        interface_retention_omitted,
        interface_positive_empty,
        interface_coverage_json,
        interface_report_json.clone(),
        interface_raw_within,
        (!interface_raw_within)
            .then_some("raw interface output exceeds 1 MiB shadow retention bound".into()),
    );
    let interface_family = AssembledFamily {
        feature: FRR_INTERFACE_FEATURE,
        fact_kind: "interface.state",
        sequence: 1,
        state: StoredFeatureFamilyState {
            feature: FRR_INTERFACE_FEATURE.into(),
            fact_kind: "interface.state".into(),
            support_state: "supported".into(),
            latest_attempt: family_attempt_state(
                &interface_command,
                Some(interface_outcome),
                &capture_id,
                session_epoch,
                1,
                run.output.facts.len(),
                run.output.facts.len(),
                interface_report_json,
            ),
            retained: interface_retained,
        },
        retained_facts: interface_retained_facts,
    };
    let interface_identities = interface_identity_evidence(&run.output.facts);

    // ARP/ND family. Applicability is independently enforced against the
    // selected NOS feature receipt before normalization dispatch.
    let arp_output = neighbor_command.output.clone().unwrap_or_default();
    let arp_observed_at = neighbor_command
        .completed_at
        .clone()
        .unwrap_or_else(|| captured_at.clone());
    let arp_capture_outcome = capture_outcome_for_execution(&neighbor_command);
    let arp_boundary = validate_family_profile_boundary(
        &receipt,
        FRR_ARPND_FEATURE,
        namespace,
        &release,
        "arpnd_linux_ip_neigh_normalizer",
        crate::neighbor_normalizer::NEIGHBOR_PARSER_REVISION,
        crate::neighbor_normalizer::NEIGHBOR_MAPPING_REVISION,
        "linux_ip_neigh_show",
    );
    let arp_family = if let Err(error) = arp_boundary {
        unsupported_family(
            FRR_ARPND_FEATURE,
            "arpnd.neighbor",
            2,
            &neighbor_command,
            &capture_id,
            session_epoch,
            &arp_observed_at,
            &device_id,
            &collector_id,
            namespace,
            error.to_string(),
        )
    } else {
        let arp_capture =
            CommandCapture::new(&arp_output, arp_capture_outcome).captured_at(&arp_observed_at);
        let arp_context = family_context(
            &device_id,
            &collector_id,
            &capture_id,
            &arp_observed_at,
            session_epoch,
            2,
            namespace,
            &release_raw,
            input.release_build.as_deref(),
        );
        let arp_run = normalize_arpnd(&ArpndObservation {
            profile: ArpndSourceProfile::LinuxIpNeighShow,
            capture: arp_capture,
            context: arp_context,
            interface_identities: Some(&interface_identities),
        });
        let arp_entities = arp_run
            .facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let arp_omitted =
            arp_run.report.input_records_omitted + arp_run.report.output_facts_omitted;
        let arp_outcome = non_interface_family_outcome(
            arp_capture_outcome,
            arp_run.coverage.positive_empty_population,
            arp_entities,
            arp_run.report.malformed_records,
            arp_omitted,
            arp_run.report.facts_rejected,
            arp_run.report.refusal_reason.as_deref(),
        );
        let (arp_retained_facts, arp_retention_omitted) = cap_family_facts(&arp_run.facts);
        let arp_retained_population = arp_retained_facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let arp_coverage_json =
            coverage_json_with_retention(&arp_run.coverage, arp_entities, arp_retained_population)?;
        let arp_report_json = serde_json::to_value(&arp_run.report)?;
        let arp_raw_within = arp_output.len() <= MAX_STORED_RAW_BYTES;
        let arp_retained = family_retained_state(
            &neighbor_command,
            arp_outcome,
            &arp_observed_at,
            &capture_id,
            session_epoch,
            2,
            arp_run.facts.len(),
            arp_run.facts.len(),
            arp_retained_facts.len(),
            arp_retention_omitted,
            arp_run.coverage.positive_empty_population,
            arp_coverage_json,
            arp_report_json.clone(),
            arp_raw_within,
            (!arp_raw_within)
                .then_some("raw neighbor output exceeds 1 MiB shadow retention bound".into()),
        );
        AssembledFamily {
            feature: FRR_ARPND_FEATURE,
            fact_kind: "arpnd.neighbor",
            sequence: 2,
            state: StoredFeatureFamilyState {
                feature: FRR_ARPND_FEATURE.into(),
                fact_kind: "arpnd.neighbor".into(),
                support_state: "supported".into(),
                latest_attempt: family_attempt_state(
                    &neighbor_command,
                    Some(arp_outcome),
                    &capture_id,
                    session_epoch,
                    2,
                    arp_run.facts.len(),
                    arp_run.facts.len(),
                    arp_report_json,
                ),
                retained: arp_retained,
            },
            retained_facts: arp_retained_facts,
        }
    };

    // MAC forwarding family.
    let mac_output = mac_command.output.clone().unwrap_or_default();
    let mac_observed_at = mac_command
        .completed_at
        .clone()
        .unwrap_or_else(|| captured_at.clone());
    let mac_capture_outcome = capture_outcome_for_execution(&mac_command);
    let mac_boundary = validate_family_profile_boundary(
        &receipt,
        FRR_MAC_FEATURE,
        namespace,
        &release,
        "mac_linux_bridge_fdb_normalizer",
        crate::neighbor_normalizer::NEIGHBOR_PARSER_REVISION,
        crate::neighbor_normalizer::NEIGHBOR_MAPPING_REVISION,
        "linux_bridge_fdb_show",
    );
    let mac_family = if let Err(error) = mac_boundary {
        unsupported_family(
            FRR_MAC_FEATURE,
            "mac.forwarding",
            3,
            &mac_command,
            &capture_id,
            session_epoch,
            &mac_observed_at,
            &device_id,
            &collector_id,
            namespace,
            error.to_string(),
        )
    } else {
        let mac_capture =
            CommandCapture::new(&mac_output, mac_capture_outcome).captured_at(&mac_observed_at);
        let mac_context = family_context(
            &device_id,
            &collector_id,
            &capture_id,
            &mac_observed_at,
            session_epoch,
            3,
            namespace,
            &release_raw,
            input.release_build.as_deref(),
        );
        let mac_run = normalize_mac(&MacObservation {
            profile: MacSourceProfile::LinuxBridgeFdbShow,
            capture: mac_capture,
            context: mac_context,
            interface_identities: Some(&interface_identities),
        });
        let mac_entities = mac_run
            .facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let mac_omitted =
            mac_run.report.input_records_omitted + mac_run.report.output_facts_omitted;
        let mac_outcome = non_interface_family_outcome(
            mac_capture_outcome,
            mac_run.coverage.positive_empty_population,
            mac_entities,
            mac_run.report.malformed_records,
            mac_omitted,
            mac_run.report.facts_rejected,
            mac_run.report.refusal_reason.as_deref(),
        );
        let (mac_retained_facts, mac_retention_omitted) = cap_family_facts(&mac_run.facts);
        let mac_retained_population = mac_retained_facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let mac_coverage_json =
            coverage_json_with_retention(&mac_run.coverage, mac_entities, mac_retained_population)?;
        let mac_report_json = serde_json::to_value(&mac_run.report)?;
        let mac_raw_within = mac_output.len() <= MAX_STORED_RAW_BYTES;
        let mac_retained = family_retained_state(
            &mac_command,
            mac_outcome,
            &mac_observed_at,
            &capture_id,
            session_epoch,
            3,
            mac_run.facts.len(),
            mac_run.facts.len(),
            mac_retained_facts.len(),
            mac_retention_omitted,
            mac_run.coverage.positive_empty_population,
            mac_coverage_json,
            mac_report_json.clone(),
            mac_raw_within,
            (!mac_raw_within)
                .then_some("raw MAC output exceeds 1 MiB shadow retention bound".into()),
        );
        AssembledFamily {
            feature: FRR_MAC_FEATURE,
            fact_kind: "mac.forwarding",
            sequence: 3,
            state: StoredFeatureFamilyState {
                feature: FRR_MAC_FEATURE.into(),
                fact_kind: "mac.forwarding".into(),
                support_state: "supported".into(),
                latest_attempt: family_attempt_state(
                    &mac_command,
                    Some(mac_outcome),
                    &capture_id,
                    session_epoch,
                    3,
                    mac_run.facts.len(),
                    mac_run.facts.len(),
                    mac_report_json,
                ),
                retained: mac_retained,
            },
            retained_facts: mac_retained_facts,
        }
    };

    // VLAN bridge-domain family.
    let vlan_output = vlan_command.output.clone().unwrap_or_default();
    let vlan_observed_at = vlan_command
        .completed_at
        .clone()
        .unwrap_or_else(|| captured_at.clone());
    let vlan_capture_outcome = capture_outcome_for_execution(&vlan_command);
    let vlan_boundary = validate_family_profile_boundary(
        &receipt,
        FRR_VLAN_FEATURE,
        namespace,
        &release,
        "vlan_linux_bridge_vlan_normalizer",
        crate::vlan_normalizer::VLAN_PARSER_REVISION,
        crate::vlan_normalizer::VLAN_MAPPING_REVISION,
        "linux_bridge_c_vlan_show",
    );
    let vlan_family = if let Err(error) = vlan_boundary {
        unsupported_family(
            FRR_VLAN_FEATURE,
            "vlan.bridge_domain",
            4,
            &vlan_command,
            &capture_id,
            session_epoch,
            &vlan_observed_at,
            &device_id,
            &collector_id,
            namespace,
            error.to_string(),
        )
    } else {
        let vlan_capture =
            CommandCapture::new(&vlan_output, vlan_capture_outcome).captured_at(&vlan_observed_at);
        let vlan_context = family_context(
            &device_id,
            &collector_id,
            &capture_id,
            &vlan_observed_at,
            session_epoch,
            4,
            namespace,
            &release_raw,
            input.release_build.as_deref(),
        );
        let vlan_run = normalize_vlan(&VlanObservation {
            profile: VlanSourceProfile::LinuxBridgeCVlanShow,
            capture: vlan_capture,
            context: vlan_context,
        });
        let vlan_entities = vlan_run
            .facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let vlan_omitted =
            vlan_run.report.input_records_omitted + vlan_run.report.output_facts_omitted;
        let vlan_outcome = non_interface_family_outcome(
            vlan_capture_outcome,
            vlan_run.coverage.positive_empty_population,
            vlan_entities,
            vlan_run.report.malformed_records,
            vlan_omitted,
            vlan_run.report.facts_rejected,
            vlan_run.report.refusal_reason.as_deref(),
        );
        let (vlan_retained_facts, vlan_retention_omitted) = cap_family_facts(&vlan_run.facts);
        let vlan_retained_population = vlan_retained_facts
            .iter()
            .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
            .count();
        let vlan_coverage_json = coverage_json_with_retention(
            &vlan_run.coverage,
            vlan_entities,
            vlan_retained_population,
        )?;
        let vlan_report_json = serde_json::to_value(&vlan_run.report)?;
        let vlan_raw_within = vlan_output.len() <= MAX_STORED_RAW_BYTES;
        let vlan_retained = family_retained_state(
            &vlan_command,
            vlan_outcome,
            &vlan_observed_at,
            &capture_id,
            session_epoch,
            4,
            vlan_run.facts.len(),
            vlan_run.facts.len(),
            vlan_retained_facts.len(),
            vlan_retention_omitted,
            vlan_run.coverage.positive_empty_population,
            vlan_coverage_json,
            vlan_report_json.clone(),
            vlan_raw_within,
            (!vlan_raw_within)
                .then_some("raw VLAN output exceeds 1 MiB shadow retention bound".into()),
        );
        AssembledFamily {
            feature: FRR_VLAN_FEATURE,
            fact_kind: "vlan.bridge_domain",
            sequence: 4,
            state: StoredFeatureFamilyState {
                feature: FRR_VLAN_FEATURE.into(),
                fact_kind: "vlan.bridge_domain".into(),
                support_state: "supported".into(),
                latest_attempt: family_attempt_state(
                    &vlan_command,
                    Some(vlan_outcome),
                    &capture_id,
                    session_epoch,
                    4,
                    vlan_run.facts.len(),
                    vlan_run.facts.len(),
                    vlan_report_json,
                ),
                retained: vlan_retained,
            },
            retained_facts: vlan_retained_facts,
        }
    };

    let mut families = vec![interface_family, arp_family, mac_family, vlan_family];
    for family in &mut families {
        if family.state.retained.is_none() {
            // Failed/unsupported family interpretation can emit bounded status
            // facts for attempt diagnostics, but those facts are not retained
            // family evidence and must not enter the usable aggregate union.
            family.retained_facts.clear();
        }
    }
    let mut typed_facts = Vec::new();
    for family in &families {
        typed_facts.extend(family.retained_facts.iter().cloned());
    }
    if !typed_facts.iter().all(|fact| fact.validate().is_ok()) {
        bail!("frr native normalizers emitted an invalid typed fact")
    }
    let usable_family_count = families
        .iter()
        .filter(|family| family.state.retained.is_some())
        .count();
    let fresh_complete_count = families
        .iter()
        .filter(|family| {
            matches!(
                family.state.latest_attempt.normalization_outcome.as_deref(),
                Some("complete_usable" | "genuine_complete_empty")
            )
        })
        .count();
    let _fresh_partial_count = families
        .iter()
        .filter(|family| {
            matches!(
                family.state.latest_attempt.normalization_outcome.as_deref(),
                Some("partial_usable")
            )
        })
        .count();
    let interpretation_outcome = if usable_family_count == 0 {
        "no_usable_records"
    } else if fresh_complete_count == 4 {
        "complete_usable"
    } else {
        "partial_usable"
    };
    let facts_emitted: usize = families
        .iter()
        .map(|family| family.state.latest_attempt.facts_emitted)
        .sum();
    let facts_validated: usize = families
        .iter()
        .map(|family| family.state.latest_attempt.facts_validated)
        .sum();
    let facts_retained = typed_facts.len();
    let fact_retention_truncated = families.iter().any(|family| {
        family
            .state
            .retained
            .as_ref()
            .is_some_and(|r| r.retention_truncated)
    });
    let mut facts = Vec::with_capacity(facts_retained.min(MAX_STORED_FACTS));
    for fact in typed_facts.iter().take(MAX_STORED_FACTS) {
        facts.push(serde_json::to_value(fact)?);
    }
    let diagnostics_retained = run.output.interfaces.len().min(MAX_STORED_DIAGNOSTICS);
    let mut interface_diagnostics = Vec::with_capacity(diagnostics_retained);
    for diagnostic in run.output.interfaces.iter().take(diagnostics_retained) {
        interface_diagnostics.push(serde_json::to_value(diagnostic)?);
    }
    let interface_sha = interface_command
        .output_sha256
        .clone()
        .unwrap_or_else(|| sha256_text(&iface_output));
    let neighbor_sha = neighbor_command
        .output_sha256
        .clone()
        .unwrap_or_else(|| sha256_text(&arp_output));
    let mac_sha = mac_command
        .output_sha256
        .clone()
        .unwrap_or_else(|| sha256_text(&mac_output));
    let vlan_sha = vlan_command
        .output_sha256
        .clone()
        .unwrap_or_else(|| sha256_text(&vlan_output));
    let raw_within_retention = interface_raw_within;
    let neighbor_within_retention = arp_output.len() <= MAX_STORED_RAW_BYTES;
    let mac_within_retention = mac_output.len() <= MAX_STORED_RAW_BYTES;
    let vlan_within_retention = vlan_output.len() <= MAX_STORED_RAW_BYTES;
    let family_states: Vec<_> = families.iter().map(|family| family.state.clone()).collect();
    let feature_family_report: Vec<_> = family_states
        .iter()
        .map(|family| {
            serde_json::json!({
                "feature": family.feature,
                "fact_kind": family.fact_kind,
                "support_state": family.support_state,
                "latest_attempt": family.latest_attempt,
                "retained": family.retained,
                "facts_emitted": family.latest_attempt.facts_emitted,
                "facts_retained": family.retained.as_ref().map(|r| r.facts_retained).unwrap_or(0),
                "facts_omitted_by_retention": family.retained.as_ref().map(|r| r.facts_omitted_by_retention).unwrap_or(0),
                "coverage": family.retained.as_ref().and_then(|r| r.coverage.clone()),
                "report": family.latest_attempt.report,
            })
        })
        .collect();
    let mut normalization_report = serde_json::to_value(&run.output.report)?;
    normalization_report["feature_families"] = serde_json::Value::Array(feature_family_report);
    normalization_report["acquisition_batch"] = serde_json::json!({
        "atomic_snapshot": false,
        "ordering_generation_semantics": "monotonic acquisition ordering only; not proof of a new transport session generation",
        "started_at": raw.acquisition_batch.started_at,
        "started_at_ns": raw.acquisition_batch.started_at_ns,
        "completed_at": raw.acquisition_batch.completed_at,
        "completed_at_ns": raw.acquisition_batch.completed_at_ns,
    });
    normalization_report["command_execution"] = serde_json::to_value(
        raw.commands
            .values()
            .map(execution_metadata)
            .collect::<Vec<_>>(),
    )?;
    let usable_entity_facts = typed_facts
        .iter()
        .filter(|fact| fact.subject.entity.is_some() && fact.payload.fields.is_some())
        .count();
    let status_facts = typed_facts
        .iter()
        .filter(|fact| fact.subject.entity.is_none() && fact.payload.state.is_some())
        .count();

    Ok(StoredFrrInterfaceShadowObservation {
        observation_id: observation_id(&device_id, namespace),
        device_address: input.device_address,
        device_id,
        collector_id,
        captured_at_ns,
        capture_started_at_ns: input.acquisition_started_at_ns,
        captured_at,
        transport: raw.transport,
        acquisition_contract: FRR_INTERFACE_CAPTURE_CONTRACT.into(),
        interface_command: interface_command.command.clone(),
        version_command: version_command.command.clone(),
        neighbor_command: Some(neighbor_command.command.clone()),
        mac_command: Some(mac_command.command.clone()),
        vlan_command: Some(vlan_command.command.clone()),
        host_key: raw.host_key,
        platform: "frr".into(),
        release_raw,
        release_build: input.release_build,
        network_namespace: namespace.into(),
        network_namespace_evidence:
            "fixed verified SSH session in device default network namespace; no namespace override"
                .into(),
        device_hostname_reported: raw.device_hostname,
        interface_output_sha256: interface_sha,
        interface_output_bytes: iface_output.len(),
        raw_interface_json: raw_within_retention.then_some(iface_output),
        raw_output_omitted_reason: (!raw_within_retention)
            .then_some("raw output exceeds 1 MiB shadow retention bound".into()),
        neighbor_output_sha256: Some(neighbor_sha),
        neighbor_output_bytes: Some(arp_output.len()),
        raw_neighbor_output: neighbor_within_retention.then_some(arp_output),
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
        facts_validated,
        facts_retained,
        fact_retention_truncated,
        facts,
        diagnostics_retained,
        diagnostic_retention_truncated: diagnostics_retained != run.output.interfaces.len(),
        interface_diagnostics,
        command_execution: raw.commands.values().map(execution_metadata).collect(),
        family_states,
        normalization_report,
        acquisition_classification: input.acquisition_classification,
        interpretation_outcome: interpretation_outcome.into(),
        usable_entity_facts,
        status_facts,
    })
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
    if target.software_build.as_deref() != Some(crate::nos_features::FRR_1061_STRUCTURED_ARTIFACT) {
        bail!(
            "frr interface shadow dispatch refused: exact qualified software build metadata is absent or does not match the supported artifact"
        )
    }
    let device_id = canonical_device_id(target);
    if admission.device_key() != device_id {
        bail!("native interface acquisition admission identity mismatch")
    }
    let release_build = target
        .software_build
        .clone()
        .expect("exact artifact checked");
    let families = planned_capture_families(&release_build)?;
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
        .arg("--families")
        .arg(families.join(","))
        .env("BONSAI_CAPTURE_PASSWORD", credentials.password.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.arg("--known-hosts").arg(known_hosts);
    let output = run_capture_helper_bounded(
        &mut cmd,
        std::time::Duration::from_secs(CAPTURE_DEADLINE_SECS),
    )
    .await?;
    if !output.status.success() {
        bail!(
            "frr interface capture failed (helper stderr {} bytes); verify credentials and pinned SSH host keys",
            output.stderr_bytes
        )
    }
    if output.stdout_bytes == 0 {
        bail!("frr interface capture helper produced no envelope")
    }
    let raw: RawCaptureEnvelope =
        serde_json::from_slice(&output.stdout).context("invalid frr interface capture envelope")?;
    let address_string = address.to_string();
    let collector_id = collector_id.to_string();
    tokio::task::spawn_blocking(move || {
        observation_from_capture_envelope(
            raw,
            CaptureEnvelopeAssemblyInput {
                device_id,
                device_address: address_string,
                collector_id,
                release_build: Some(release_build),
                acquisition_started_at_ns: started,
                acquisition_classification: "live_authorized_ssh_readonly".into(),
            },
        )
    })
    .await
    .context("frr interface normalization worker failed")?
}

fn expected_family_contract(fact_kind: &str) -> Option<(&'static str, &'static str, u64)> {
    match fact_kind {
        "interface.state" => Some((FRR_INTERFACE_FEATURE, "interface", 1)),
        "arpnd.neighbor" => Some((FRR_ARPND_FEATURE, "interface", 2)),
        "mac.forwarding" => Some((FRR_MAC_FEATURE, "interface", 3)),
        "vlan.bridge_domain" => Some((FRR_VLAN_FEATURE, "forwarding", 4)),
        _ => None,
    }
}

fn raw_output_metadata<'a>(
    observation: &'a StoredFrrInterfaceShadowObservation,
    fact_kind: &str,
) -> Result<(usize, &'a str, &'a Option<String>, &'a Option<String>)> {
    match fact_kind {
        "interface.state" => Ok((
            observation.interface_output_bytes,
            observation.interface_output_sha256.as_str(),
            &observation.raw_interface_json,
            &observation.raw_output_omitted_reason,
        )),
        "arpnd.neighbor" => Ok((
            observation
                .neighbor_output_bytes
                .context("missing neighbor output byte count")?,
            observation
                .neighbor_output_sha256
                .as_deref()
                .context("missing neighbor output digest")?,
            &observation.raw_neighbor_output,
            &observation.raw_neighbor_output_omitted_reason,
        )),
        "mac.forwarding" => Ok((
            observation
                .mac_output_bytes
                .context("missing MAC output byte count")?,
            observation
                .mac_output_sha256
                .as_deref()
                .context("missing MAC output digest")?,
            &observation.raw_mac_output,
            &observation.raw_mac_output_omitted_reason,
        )),
        "vlan.bridge_domain" => Ok((
            observation
                .vlan_output_bytes
                .context("missing VLAN output byte count")?,
            observation
                .vlan_output_sha256
                .as_deref()
                .context("missing VLAN output digest")?,
            &observation.raw_vlan_output,
            &observation.raw_vlan_output_omitted_reason,
        )),
        _ => bail!("unsupported native interface family fact kind"),
    }
}

/// Validate the stored multifamily observation using fact-kind-specific
/// semantic contracts. This intentionally does not reduce every family to
/// the interface namespace: interface/ARP/MAC facts are interface-namespace
/// operational facts, while bridge-domain VLAN facts are forwarding-plane
/// operational facts. Common canonical device, collector, scope, network
/// namespace, capture-generation and bounded-retention checks apply to all.
fn fact_json_text<'a>(fact: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut current = fact;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn fact_json_u64(fact: &serde_json::Value, path: &[&str]) -> Option<u64> {
    let mut current = fact;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_u64()
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
        bail!("native interface observation predates canonical source identity")
    }
    if observation.transport != "verified_ssh_read_only_cli"
        || observation.acquisition_contract != FRR_L2_CAPTURE_CONTRACT
        || observation.platform != "frr"
        || !observation.shadow_only
    {
        bail!("native interface observation acquisition/trust contract mismatch")
    }

    // Family lifecycle state is the authority for retained-evidence identity.
    // Require exactly one state for each registered feature family so an
    // unsupported kind cannot enter through the generic JSON report.
    let expected_families = [
        ("interface.state", FRR_INTERFACE_FEATURE, "interface", 1u64),
        ("arpnd.neighbor", FRR_ARPND_FEATURE, "interface", 2u64),
        ("mac.forwarding", FRR_MAC_FEATURE, "interface", 3u64),
        ("vlan.bridge_domain", FRR_VLAN_FEATURE, "forwarding", 4u64),
    ];
    if observation.family_states.len() != expected_families.len() {
        bail!(
            "native interface observation does not carry the complete registered family lifecycle state"
        )
    }
    let mut states_by_kind = BTreeMap::new();
    for (fact_kind, feature, namespace, sequence) in expected_families {
        let matches: Vec<_> = observation
            .family_states
            .iter()
            .filter(|state| state.fact_kind == fact_kind)
            .collect();
        if matches.len() != 1 {
            bail!(
                "native interface family lifecycle state is missing or duplicated for {fact_kind}"
            )
        }
        let state = matches[0];
        if state.feature != feature
            || !matches!(
                state.support_state.as_str(),
                "supported" | "unsupported" | "unknown"
            )
        {
            bail!("native interface family feature/support identity mismatch for {fact_kind}")
        }
        let attempt = &state.latest_attempt;
        if attempt.capture_id.as_deref() != Some(observation.capture_id.as_str())
            || attempt.session_epoch != Some(observation.session_epoch)
            || attempt.sequence != Some(sequence)
        {
            bail!(
                "native interface latest-attempt capture/generation identity mismatch for {fact_kind}"
            )
        }
        if attempt.started_at_ns.is_none()
            || attempt.completed_at_ns.is_none()
            || attempt.started_at_ns > attempt.completed_at_ns
        {
            bail!(
                "native interface family lacks coherent per-command acquisition timing for {fact_kind}"
            )
        }
        let _ = namespace;
        states_by_kind.insert(fact_kind, state);
    }

    // Per-command execution metadata must preserve the sequential acquisition
    // interval. It is evidence of timing, not a claim of atomic snapshotting.
    for key in ["version", "interfaces", "arpnd", "mac", "vlan"] {
        let Some(command) = observation
            .command_execution
            .iter()
            .find(|command| command.command_key == key)
        else {
            bail!("native interface observation lacks command execution evidence for {key}")
        };
        if command.command != expected_family_command(key)
            || command.started_at_ns.is_none()
            || command.completed_at_ns.is_none()
            || command.started_at_ns > command.completed_at_ns
        {
            bail!("native interface command execution evidence is inconsistent for {key}")
        }
    }

    let mut fact_counts: BTreeMap<String, usize> = BTreeMap::new();
    for fact in &observation.facts {
        // Check the storage-family contract against the serialized axes before
        // TypedFact deserialization. Deserialization intentionally performs
        // typed semantic validation; doing these scope checks first preserves a
        // specific storage-contract error instead of collapsing a wrong-family
        // fact into a generic undecodable/invalid fact.
        let fact_kind = fact_json_text(fact, &["fact_kind"])
            .context("native interface typed fact lacks a decodable fact kind")?;
        let Some((feature, expected_namespace, expected_sequence)) =
            expected_family_contract(fact_kind)
        else {
            bail!(
                "unsupported fact kind '{}' cannot enter the multifamily observation",
                fact_kind
            )
        };
        let state = states_by_kind
            .get(fact_kind)
            .copied()
            .expect("family state validated");
        if state.feature != feature {
            bail!("native interface family feature binding mismatch")
        }
        let retained = state
            .retained
            .as_ref()
            .context("typed fact exists without corresponding retained family evidence")?;
        let subject_device = fact_json_text(fact, &["subject", "device_id"])
            .context("native interface typed fact lacks subject device identity")?;
        let subject_namespace = fact_json_text(fact, &["subject", "namespace"])
            .context("native interface typed fact lacks subject namespace")?;
        let subject_plane = fact_json_text(fact, &["subject", "plane"]);
        let subject_network_namespace = fact_json_text(fact, &["subject", "network_namespace"]);
        let source_capture = fact_json_text(fact, &["source", "capture_id"])
            .context("native interface typed fact lacks source capture identity")?;
        let source_collector = fact_json_text(fact, &["source", "collector_id"])
            .context("native interface typed fact lacks source collector identity")?;
        let source_epoch = fact_json_u64(fact, &["source", "session_epoch"])
            .context("native interface typed fact lacks source session epoch")?;
        let source_sequence = fact_json_u64(fact, &["source", "sequence"])
            .context("native interface typed fact lacks source sequence")?;
        if subject_device != observation.device_id
            || subject_namespace != expected_namespace
            || subject_plane != Some("operational")
            || subject_network_namespace != Some(observation.network_namespace.as_str())
            || source_capture != retained.capture_id
            || source_epoch != retained.session_epoch
            || source_sequence != retained.sequence
            || source_sequence != expected_sequence
            || source_collector != observation.collector_id
        {
            bail!("native interface typed-fact subject/source identity mismatch")
        }
        let typed: crate::typed_facts::TypedFact = serde_json::from_value(fact.clone())
            .context("native interface typed fact is undecodable or semantically invalid")?;
        *fact_counts.entry(typed.fact_kind.clone()).or_insert(0) += 1;
    }

    // Validate retained per-family evidence, raw-output integrity and scoped
    // coverage against the exact family lifecycle record that authorizes the
    // facts. Facts may legitimately come from an older last-good family
    // capture; therefore they are bound to that family's retained capture
    // identity rather than flattened into the latest aggregate batch time.
    for (fact_kind, state) in states_by_kind {
        let Some(retained) = state.retained.as_ref() else {
            if fact_counts.get(fact_kind).copied().unwrap_or(0) != 0 {
                bail!("native interface family facts exist without retained lifecycle evidence")
            }
            continue;
        };
        if retained.capture_id.trim().is_empty()
            || retained.session_epoch == 0
            || retained.observed_at.trim().is_empty()
            || !matches!(
                retained.evidence_state.as_str(),
                "current" | "partial_current" | "stale_last_usable"
            )
        {
            bail!("native interface retained family lifecycle state is incomplete for {fact_kind}")
        }
        let (output_bytes, output_sha256, raw_output, raw_omitted_reason) =
            raw_output_metadata(observation, fact_kind)?;
        if retained.output_bytes != Some(output_bytes)
            || retained.output_sha256.as_deref() != Some(output_sha256)
        {
            bail!("native interface retained family raw-output identity mismatch for {fact_kind}")
        }
        match raw_output {
            Some(raw) => {
                if raw.len() != output_bytes || sha256_text(raw) != output_sha256 {
                    bail!("retained raw output digest/count mismatch for {fact_kind}")
                }
                if !retained.raw_output_retained {
                    bail!("retained raw-output availability flag mismatch for {fact_kind}")
                }
            }
            None => {
                if retained.raw_output_retained
                    || raw_omitted_reason.as_deref().is_none_or(str::is_empty)
                {
                    bail!("omitted raw output lacks explicit omission metadata for {fact_kind}")
                }
            }
        }
        let actual_count = fact_counts.get(fact_kind).copied().unwrap_or(0);
        if actual_count != retained.facts_retained {
            bail!("native interface retained family fact count mismatch for {fact_kind}")
        }
        if retained.positive_empty_population && actual_count != 0 {
            bail!("positive-empty family cannot contain populated retained facts for {fact_kind}")
        }
        if !retained.positive_empty_population
            && actual_count == 0
            && matches!(
                retained.evidence_state.as_str(),
                "current" | "partial_current"
            )
        {
            bail!(
                "non-empty retained family evidence is missing all retained facts for {fact_kind}"
            )
        }
        if (retained.facts_omitted_by_retention > 0) != retained.retention_truncated {
            bail!("native interface retention accounting is inconsistent for {fact_kind}")
        }
        let coverage_value = retained
            .coverage
            .as_ref()
            .context("retained family evidence lacks scoped capture coverage")?;
        let coverage: ScopedCaptureCoverage = serde_json::from_value(coverage_value.clone())
            .context("retained family scoped coverage is undecodable")?;
        let expected_scope = match fact_kind {
            "interface.state" => "frr/show-interface-json",
            "arpnd.neighbor" => "linux/ip-neigh-show",
            "mac.forwarding" => "linux/bridge-fdb-show",
            "vlan.bridge_domain" => "linux/bridge-c-vlan-show",
            _ => "",
        };
        if coverage.covered_fact_kind != fact_kind || coverage.acquisition_scope != expected_scope {
            bail!(
                "retained family coverage fact-kind/source-scope identity mismatch for {fact_kind}"
            )
        }
        retained_coverage_bound(&coverage, observation, retained)?;
        if coverage.records_decoded < coverage.malformed_records + coverage.rejected_records {
            bail!("retained family coverage accounting is internally inconsistent for {fact_kind}")
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
        family_states: observation.family_states.clone(),
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
        family_states: Vec::new(),
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
fn retained_replaces_prior(
    candidate: &FamilyRetainedEvidenceState,
    prior: Option<&FamilyRetainedEvidenceState>,
) -> bool {
    let Some(prior) = prior else { return true };
    let candidate_position = (candidate.session_epoch, candidate.sequence);
    let prior_position = (prior.session_epoch, prior.sequence);
    if candidate_position <= prior_position {
        return false;
    }
    match candidate.evidence_state.as_str() {
        // `current` includes both populated complete evidence and a genuine
        // complete-empty capture; positive empty is a valid replacement within
        // the same scoped population, not missing evidence.
        "current" => true,
        // Partial evidence is useful when no better evidence exists, but it
        // must not erase a more complete last-good population.
        "partial_current" => false,
        _ => false,
    }
}

fn merge_family_states(
    prior: &[StoredFeatureFamilyState],
    candidate: &[StoredFeatureFamilyState],
) -> Vec<StoredFeatureFamilyState> {
    let mut merged = Vec::new();
    for (feature, fact_kind) in [
        (FRR_INTERFACE_FEATURE, "interface.state"),
        (FRR_ARPND_FEATURE, "arpnd.neighbor"),
        (FRR_MAC_FEATURE, "mac.forwarding"),
        (FRR_VLAN_FEATURE, "vlan.bridge_domain"),
    ] {
        let Some(candidate_state) = candidate
            .iter()
            .find(|state| state.feature == feature && state.fact_kind == fact_kind)
        else {
            if let Some(prior_state) = prior
                .iter()
                .find(|state| state.feature == feature && state.fact_kind == fact_kind)
            {
                merged.push(prior_state.clone());
            }
            continue;
        };
        let prior_state = prior
            .iter()
            .find(|state| state.feature == feature && state.fact_kind == fact_kind);
        let mut state = candidate_state.clone();
        let replacement = match (
            candidate_state.retained.as_ref(),
            prior_state.and_then(|s| s.retained.as_ref()),
        ) {
            (Some(candidate_retained), prior_retained) => {
                retained_replaces_prior(candidate_retained, prior_retained)
            }
            (None, _) => false,
        };
        if replacement {
            // Fresh complete/genuine-empty evidence remains current.
        } else if let Some(prior_retained) = prior_state.and_then(|s| s.retained.clone()) {
            let mut retained = prior_retained;
            if retained.evidence_state != "partial_current" {
                retained.evidence_state = "stale_last_usable".into();
            }
            state.retained = Some(retained);
        } else {
            state.retained = candidate_state.retained.clone();
        }
        merged.push(state);
    }
    merged
}

fn typed_fact_values_by_kind(
    observation: &StoredFrrInterfaceShadowObservation,
) -> Result<BTreeMap<String, Vec<serde_json::Value>>> {
    let mut grouped: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for value in &observation.facts {
        let typed: crate::typed_facts::TypedFact = serde_json::from_value(value.clone())
            .context("native interface typed fact is undecodable during family merge")?;
        grouped
            .entry(typed.fact_kind.clone())
            .or_default()
            .push(value.clone());
    }
    Ok(grouped)
}

fn family_uses_candidate_evidence(
    merged: &StoredFeatureFamilyState,
    candidate: &StoredFrrInterfaceShadowObservation,
) -> bool {
    let Some(retained) = merged.retained.as_ref() else {
        return false;
    };
    candidate.family_states.iter().any(|state| {
        state.feature == merged.feature
            && state.fact_kind == merged.fact_kind
            && state.retained.as_ref().is_some_and(|candidate_retained| {
                candidate_retained.capture_id == retained.capture_id
                    && candidate_retained.session_epoch == retained.session_epoch
                    && candidate_retained.sequence == retained.sequence
            })
    })
}

fn rebuild_feature_family_report(
    family_states: &[StoredFeatureFamilyState],
) -> Vec<serde_json::Value> {
    family_states
        .iter()
        .map(|family| {
            serde_json::json!({
                "feature": family.feature,
                "fact_kind": family.fact_kind,
                "support_state": family.support_state,
                "latest_attempt": family.latest_attempt,
                "retained": family.retained,
                "facts_emitted": family.latest_attempt.facts_emitted,
                "facts_retained": family.retained.as_ref().map(|r| r.facts_retained).unwrap_or(0),
                "facts_omitted_by_retention": family.retained.as_ref().map(|r| r.facts_omitted_by_retention).unwrap_or(0),
                "coverage": family.retained.as_ref().and_then(|r| r.coverage.clone()),
                "report": family.latest_attempt.report,
            })
        })
        .collect()
}

/// Merge a new aggregate acquisition batch with prior per-family retained
/// evidence. The aggregate observation remains the single storage unit, but
/// family lifecycle semantics are independent: failed or partial fresh family
/// evidence cannot erase unrelated valid last-good evidence, while a genuine
/// complete-empty capture can replace an older populated family.
fn merge_multifamily_observation(
    prior: &StoredFrrInterfaceShadowObservation,
    candidate: &StoredFrrInterfaceShadowObservation,
) -> Result<StoredFrrInterfaceShadowObservation> {
    if prior.device_id != candidate.device_id
        || prior.network_namespace != candidate.network_namespace
        || prior.collector_id != candidate.collector_id
    {
        bail!("cannot merge native interface evidence across canonical device/source boundaries")
    }
    let mut merged = candidate.clone();
    merged.family_states = merge_family_states(&prior.family_states, &candidate.family_states);
    let prior_facts = typed_fact_values_by_kind(prior)?;
    let candidate_facts = typed_fact_values_by_kind(candidate)?;
    let mut facts = Vec::new();
    let mut facts_retained = 0usize;
    let mut retention_truncated = false;
    for fact_kind in [
        "interface.state",
        "arpnd.neighbor",
        "mac.forwarding",
        "vlan.bridge_domain",
    ] {
        let Some(state) = merged
            .family_states
            .iter()
            .find(|state| state.fact_kind == fact_kind)
        else {
            continue;
        };
        if state.retained.is_none() {
            continue;
        }
        let use_candidate = family_uses_candidate_evidence(state, candidate);
        let source = if use_candidate {
            &candidate_facts
        } else {
            &prior_facts
        };
        if let Some(values) = source.get(fact_kind) {
            facts.extend(values.iter().cloned());
        }
        if let Some(retained) = state.retained.as_ref() {
            facts_retained = facts_retained.saturating_add(retained.facts_retained);
            retention_truncated |= retained.retention_truncated;
        }
    }
    merged.facts = facts.into_iter().take(MAX_STORED_FACTS).collect();
    merged.facts_retained = merged.facts.len();
    merged.fact_retention_truncated = retention_truncated || facts_retained > MAX_STORED_FACTS;
    merged.usable_entity_facts = merged
        .facts
        .iter()
        .filter(|fact| {
            fact.get("subject").and_then(|s| s.get("entity")).is_some()
                && fact.get("payload").and_then(|p| p.get("fields")).is_some()
        })
        .count();
    merged.status_facts = merged
        .facts
        .iter()
        .filter(|fact| {
            fact.get("subject").and_then(|s| s.get("entity")).is_none()
                && fact.get("payload").and_then(|p| p.get("state")).is_some()
        })
        .count();

    // Family raw evidence must move with the selected family lifecycle. A
    // retained prior ARP population cannot be accompanied by the newer failed
    // command's digest merely because both live in one aggregate record.
    for state in &merged.family_states {
        if state.retained.is_none() || family_uses_candidate_evidence(state, candidate) {
            continue;
        }
        match state.fact_kind.as_str() {
            "interface.state" => {
                merged.interface_output_sha256 = prior.interface_output_sha256.clone();
                merged.interface_output_bytes = prior.interface_output_bytes;
                merged.raw_interface_json = prior.raw_interface_json.clone();
                merged.raw_output_omitted_reason = prior.raw_output_omitted_reason.clone();
            }
            "arpnd.neighbor" => {
                merged.neighbor_output_sha256 = prior.neighbor_output_sha256.clone();
                merged.neighbor_output_bytes = prior.neighbor_output_bytes;
                merged.raw_neighbor_output = prior.raw_neighbor_output.clone();
                merged.raw_neighbor_output_omitted_reason =
                    prior.raw_neighbor_output_omitted_reason.clone();
            }
            "mac.forwarding" => {
                merged.mac_output_sha256 = prior.mac_output_sha256.clone();
                merged.mac_output_bytes = prior.mac_output_bytes;
                merged.raw_mac_output = prior.raw_mac_output.clone();
                merged.raw_mac_output_omitted_reason = prior.raw_mac_output_omitted_reason.clone();
            }
            "vlan.bridge_domain" => {
                merged.vlan_output_sha256 = prior.vlan_output_sha256.clone();
                merged.vlan_output_bytes = prior.vlan_output_bytes;
                merged.raw_vlan_output = prior.raw_vlan_output.clone();
                merged.raw_vlan_output_omitted_reason =
                    prior.raw_vlan_output_omitted_reason.clone();
            }
            _ => {}
        }
    }

    let fresh_complete = merged.family_states.iter().all(|family| {
        matches!(
            family.latest_attempt.normalization_outcome.as_deref(),
            Some("complete_usable" | "genuine_complete_empty")
        )
    });
    let any_retained = merged
        .family_states
        .iter()
        .any(|family| family.retained.is_some());
    merged.interpretation_outcome = if !any_retained {
        "no_usable_records".into()
    } else if fresh_complete {
        "complete_usable".into()
    } else {
        "partial_usable".into()
    };
    if let Some(feature_families) = merged.normalization_report.get_mut("feature_families") {
        *feature_families =
            serde_json::Value::Array(rebuild_feature_family_report(&merged.family_states));
    } else {
        merged.normalization_report["feature_families"] =
            serde_json::Value::Array(rebuild_feature_family_report(&merged.family_states));
    }
    validate_stored_observation(&merged)?;
    Ok(merged)
}

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
        family_states: Vec::new(),
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
    if !attempt.family_states.is_empty() {
        state.family_states = merge_family_states(&state.family_states, &attempt.family_states);
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
    if let Some(observation) = observation.as_ref() {
        if observation.observation_id
            != observation_id(&observation.device_id, &observation.network_namespace)
        {
            bail!("native interface observation identity does not match its storage key")
        }
        ids.push(observation.observation_id.clone());
    }
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
            let mut observation_to_write: Option<StoredFrrInterfaceShadowObservation> = None;
            let mut observation_content: Option<String> = None;
            if let Some(observation) = observation.as_ref() {
                let existing_observation: Option<StoredFrrInterfaceShadowObservation> =
                    decode_enabled(
                        existing.get(&observation.observation_id).cloned().flatten(),
                        FRR_INTERFACE_OBSERVATION_CLASS,
                        "native interface shadow evidence",
                    )?;
                let merged = if let Some(existing) = existing_observation.as_ref() {
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
                    merge_multifamily_observation(existing, observation)?
                } else {
                    observation.clone()
                };
                validate_stored_observation(&merged)?;
                let content = serde_json::to_string(&merged)?;
                if content.len() > MAX_STORED_OBSERVATION_BYTES {
                    bail!(
                        "bounded native interface shadow observation exceeds {} bytes",
                        MAX_STORED_OBSERVATION_BYTES
                    )
                }
                observation_content = Some(content);
                observation_to_write = Some(merged);
            }

            let mut attempt = attempt.clone();
            if let Some(merged) = observation_to_write.as_ref() {
                attempt.family_states = merged.family_states.clone();
                attempt.interpretation_outcome = Some(merged.interpretation_outcome.clone());
                attempt.observation_id = is_usable_interpretation(&merged.interpretation_outcome)
                    .then_some(merged.observation_id.clone());
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
            if let (Some(observation), Some(content)) =
                (observation_to_write.as_ref(), observation_content)
            {
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

    fn test_command(
        key: &str,
        command: &str,
        output: &str,
        started_ns: i64,
    ) -> RawCommandCaptureEnvelope {
        RawCommandCaptureEnvelope {
            command_key: key.into(),
            command: command.into(),
            outcome: "complete".into(),
            started_at: Some("2026-11-27T00:00:00.000Z".into()),
            completed_at: Some("2026-11-27T00:00:01.000Z".into()),
            started_at_ns: Some(started_ns),
            completed_at_ns: Some(started_ns + 1_000_000_000),
            output: Some(output.into()),
            output_bytes: Some(output.len()),
            output_sha256: Some(sha256_text(output)),
            detail: None,
            stderr_bytes: None,
        }
    }

    fn valid_capture_envelope() -> RawCaptureEnvelope {
        let mut commands = BTreeMap::new();
        commands.insert(
            "version".into(),
            test_command("version", FRR_VERSION_COMMAND, "{}", 1_000),
        );
        commands.insert(
            "interfaces".into(),
            test_command("interfaces", FRR_INTERFACE_COMMAND, "{}", 2_000),
        );
        commands.insert(
            "arpnd".into(),
            test_command("arpnd", FRR_NEIGHBOR_COMMAND, "", 3_000),
        );
        commands.insert(
            "mac".into(),
            test_command("mac", FRR_MAC_COMMAND, "", 4_000),
        );
        commands.insert(
            "vlan".into(),
            test_command("vlan", FRR_VLAN_COMMAND, "port\tvlan ids\n", 5_000),
        );
        RawCaptureEnvelope {
            host: "192.0.2.1".into(),
            port: 22,
            transport: "verified_ssh_read_only_cli".into(),
            host_key: VerifiedHostKey {
                algorithm: "ssh-ed25519".into(),
                sha256: "SHA256:test".into(),
            },
            acquisition_batch: RawAcquisitionBatchEnvelope {
                started_at: "2026-11-27T00:00:00.000Z".into(),
                started_at_ns: 1_000,
                completed_at: "2026-11-27T00:00:06.000Z".into(),
                completed_at_ns: 6_000,
                deadline_seconds: 40,
                atomic_snapshot: false,
            },
            commands,
            release_raw: Some("10.6.1_git".into()),
            device_hostname: Some("frr2".into()),
        }
    }

    #[test]
    fn capture_envelope_requires_the_exact_multifamily_fixed_command_contract() {
        let raw = valid_capture_envelope();
        validate_capture_envelope(&raw, "192.0.2.1").expect("exact fixed-command envelope");

        let mut wrong_command = valid_capture_envelope();
        wrong_command
            .commands
            .get_mut("vlan")
            .expect("vlan command")
            .command = "bridge vlan show".into();
        let err = validate_capture_envelope(&wrong_command, "192.0.2.1")
            .expect_err("substitute VLAN command must be refused");
        assert!(err.to_string().contains("identity/contract mismatch"));

        let err = validate_capture_envelope(&raw, "192.0.2.2")
            .expect_err("envelope for another host must be refused");
        assert!(err.to_string().contains("identity/contract mismatch"));
    }

    #[test]
    fn capture_envelope_missing_a_family_output_is_not_positive_empty_evidence() {
        let mut missing_slot = valid_capture_envelope();
        missing_slot.commands.remove("arpnd");
        let err = validate_capture_envelope(&missing_slot, "192.0.2.1")
            .expect_err("missing neighbor command slot must be explicit");
        assert!(err.to_string().contains("missing command slot arpnd"));

        let mut missing_output = valid_capture_envelope();
        let arp = missing_output.commands.get_mut("arpnd").expect("arp slot");
        arp.output = None;
        arp.output_bytes = None;
        arp.output_sha256 = None;
        let err = validate_capture_envelope(&missing_output, "192.0.2.1")
            .expect_err("successful command without bytes cannot become positive-empty evidence");
        assert!(err.to_string().contains("missing successful output"));
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

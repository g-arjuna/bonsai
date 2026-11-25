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
    interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext},
    normalizer_registry::{InterfaceObservationInput, NormalizerRegistry},
    nos_features::{HardwareScope, ProfileQuery, SoftwareRelease, builtin_registry},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
};
use tokio::{io::AsyncReadExt, sync::Semaphore};

pub const FRR_INTERFACE_COMMAND: &str = "vtysh -c 'show interface json'";
pub const FRR_VERSION_COMMAND: &str = "vtysh -c 'show version json'";
pub const FRR_INTERFACE_FEATURE: &str = "interfaces.frr_json";
pub const FRR_INTERFACE_CAPTURE_CONTRACT: &str = FRR_JSON_ACQUISITION_CONTRACT;
const CAPTURE_DEADLINE_SECS: u64 = 45;
const MAX_CONCURRENT_CAPTURES: usize = 2;
const MAX_STORED_FACTS: usize = 256;
const MAX_STORED_DIAGNOSTICS: usize = 128;
const MAX_STORED_RAW_BYTES: usize = 1024 * 1024;
const MAX_STORED_OBSERVATION_BYTES: usize = 2 * 1024 * 1024;
const MAX_HELPER_STDOUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_HELPER_STDERR_BYTES: usize = 64 * 1024;

fn capture_permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_CAPTURES)))
}

fn device_capture_permits() -> &'static std::sync::Mutex<BTreeMap<String, Arc<Semaphore>>> {
    static PERMITS: OnceLock<std::sync::Mutex<BTreeMap<String, Arc<Semaphore>>>> = OnceLock::new();
    PERMITS.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
}

struct BoundedChildOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

async fn read_bounded_stream<R>(mut reader: R, max_bytes: usize) -> std::io::Result<(Vec<u8>, u64)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    // Drain the stream completely so a verbose child cannot deadlock on a
    // full pipe, while retaining only max_bytes+1 bytes as proof of overflow.
    // The absolute child deadline bounds the total draining work.
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
    }
    Ok((retained, total))
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
    let stdout_task = tokio::spawn(read_bounded_stream(stdout, MAX_HELPER_STDOUT_BYTES));
    let stderr_task = tokio::spawn(read_bounded_stream(stderr, MAX_HELPER_STDERR_BYTES));
    let status = match tokio::time::timeout(deadline, child.wait()).await {
        Ok(status) => status.context("frr interface capture helper wait failed")?,
        Err(_) => {
            child
                .kill()
                .await
                .context("frr interface capture helper timeout kill failed")?;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            bail!("frr interface capture deadline exceeded; child terminated and reaped")
        }
    };
    let (stdout, stdout_bytes) = stdout_task
        .await
        .context("frr interface capture stdout drain task failed")??;
    let (_stderr, stderr_bytes) = stderr_task
        .await
        .context("frr interface capture stderr drain task failed")??;
    if stdout_bytes > MAX_HELPER_STDOUT_BYTES as u64 {
        bail!("frr interface capture helper stdout exceeds bounded retention limit")
    }
    if stderr_bytes > MAX_HELPER_STDERR_BYTES as u64 {
        bail!("frr interface capture helper stderr exceeds bounded retention limit")
    }
    Ok(BoundedChildOutput {
        status,
        stdout,
        stdout_bytes,
        stderr_bytes,
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
    /// Canonical inventory identity used in typed-fact subjects. For imported
    /// inventory this prefers external identity/hostname over a raw address.
    pub device_id: String,
    pub captured_at_ns: i64,
    pub capture_started_at_ns: i64,
    pub captured_at: String,
    pub transport: String,
    pub acquisition_contract: String,
    pub interface_command: String,
    pub version_command: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredFrrInterfaceAcquisitionState {
    pub device_address: String,
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
}

pub fn observation_id(address: &str) -> String {
    format!(
        "native-frr-interface-{}",
        hex::encode(Sha256::digest(address.as_bytes()))
    )
}

fn acquisition_state_id(address: &str) -> String {
    format!(
        "native-frr-interface-acquisition-{}",
        hex::encode(Sha256::digest(address.as_bytes()))
    )
}

pub fn canonical_device_id(target: &TargetConfig) -> String {
    target
        .external_id
        .as_deref()
        .or(target.hostname.as_deref())
        .unwrap_or_else(|| target.primary_ip())
        .to_string()
}

#[derive(Deserialize)]
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
    release_raw: Option<String>,
    device_hostname: Option<String>,
}

/// Acquire, select, dispatch and validate one bounded shadow observation.
/// The target must be inventory-registered before this function is called;
/// this function itself only enforces the narrow platform/credential shape.
pub async fn collect_normalize_frr_interfaces(
    target: &TargetConfig,
    credentials: &ResolvedCredential,
    known_hosts: Option<&Path>,
    collector_id: &str,
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
    let _device_permit = {
        let mut devices = device_capture_permits()
            .lock()
            .map_err(|_| anyhow::anyhow!("frr interface device admission lock poisoned"))?;
        devices
            .entry(address.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("frr_interface_admission_overloaded"))?
    };
    let _permit = capture_permits()
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("frr_interface_admission_overloaded"))?;
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
    if raw.host != address
        || raw.port != 22
        || raw.transport != "verified_ssh_read_only_cli"
        || raw.interface_command != FRR_INTERFACE_COMMAND
        || raw.version_command != FRR_VERSION_COMMAND
    {
        bail!("frr interface capture envelope identity/contract mismatch")
    }
    if output.stdout_bytes == 0 {
        bail!("frr interface capture helper produced no envelope")
    }
    if raw.interface_output.len() > 4 * 1024 * 1024 || raw.version_output.len() > 4 * 1024 * 1024 {
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
    let device_id = canonical_device_id(target);
    let mut context =
        ObservationContext::minimal(&device_id, collector_id, &capture_id, &captured_at);
    context.session_epoch = session_epoch;
    context.sequence = 1;
    context.network_namespace = Some(namespace);
    context.platform = Some("frr");
    context.release_raw = Some(&release_raw);
    context.release_build = release_build.as_deref();
    let capture = CommandCapture::new(&raw.interface_output, CaptureOutcome::Complete)
        .captured_at(&captured_at);
    let observation = FrrInterfaceJsonObservation {
        capture,
        context,
        acquisition_contract: FRR_JSON_ACQUISITION_CONTRACT,
    };
    let registry = NormalizerRegistry::builtin();
    let run = registry
        .dispatch_input(
            &receipt,
            FRR_INTERFACE_FEATURE,
            InterfaceObservationInput::FrrJson(&observation),
        )
        .map_err(|e| anyhow::anyhow!("frr interface shadow dispatch refused: {e:?}"))?;
    if !run.output.facts.iter().all(|fact| fact.validate().is_ok()) {
        bail!("frr interface normalizer emitted an invalid typed fact")
    }
    let facts_emitted = run.output.facts.len();
    let facts_retained = facts_emitted.min(MAX_STORED_FACTS);
    let mut facts = Vec::with_capacity(facts_retained);
    for fact in run.output.facts.iter().take(facts_retained) {
        facts.push(serde_json::to_value(fact)?);
    }
    let diagnostics_retained = run.output.interfaces.len().min(MAX_STORED_DIAGNOSTICS);
    let mut interface_diagnostics = Vec::with_capacity(diagnostics_retained);
    for diagnostic in run.output.interfaces.iter().take(diagnostics_retained) {
        interface_diagnostics.push(serde_json::to_value(diagnostic)?);
    }
    let interface_sha = hex::encode(Sha256::digest(raw.interface_output.as_bytes()));
    let raw_within_retention = raw.interface_output.len() <= MAX_STORED_RAW_BYTES;
    Ok(StoredFrrInterfaceShadowObservation {
        observation_id: observation_id(address),
        device_address: address.to_string(),
        device_id: canonical_device_id(target),
        captured_at_ns,
        capture_started_at_ns: started,
        captured_at,
        transport: raw.transport,
        acquisition_contract: FRR_INTERFACE_CAPTURE_CONTRACT.into(),
        interface_command: raw.interface_command,
        version_command: raw.version_command,
        host_key: raw.host_key,
        platform: "frr".into(),
        release_raw,
        release_build,
        network_namespace: namespace.into(),
        network_namespace_evidence:
            "fixed verified SSH session in device default network namespace; no namespace override"
                .into(),
        device_hostname_reported: raw.device_hostname,
        interface_output_sha256: interface_sha,
        interface_output_bytes: raw.interface_output.len(),
        raw_interface_json: raw_within_retention.then_some(raw.interface_output),
        raw_output_omitted_reason: (!raw_within_retention)
            .then_some("raw output exceeds 1 MiB shadow retention bound".into()),
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
        fact_retention_truncated: facts_retained != facts_emitted,
        facts,
        diagnostics_retained,
        diagnostic_retention_truncated: diagnostics_retained != run.output.interfaces.len(),
        interface_diagnostics,
        normalization_report: serde_json::to_value(&run.output.report)?,
        acquisition_classification: "live_authorized_ssh_readonly".into(),
    })
}

pub async fn acquisition_state(
    store: &GraphStore,
    address: &str,
) -> Result<Option<StoredFrrInterfaceAcquisitionState>> {
    let id = acquisition_state_id(address);
    let Some(item) = store
        .list_config_items(Some("native_interface_acquisition_state".into()))
        .await?
        .into_iter()
        .find(|item| item.id == id && item.enabled)
    else {
        return Ok(None);
    };
    let state: StoredFrrInterfaceAcquisitionState = serde_json::from_str(&item.content_json)
        .context("existing native interface acquisition state is undecodable")?;
    if state.device_address != address {
        bail!("native interface acquisition state identity mismatch")
    }
    Ok(Some(state))
}

async fn persist_acquisition_state(
    store: &GraphStore,
    state: &StoredFrrInterfaceAcquisitionState,
) -> Result<()> {
    store
        .upsert_config_item(ConfigItemRecord {
            id: acquisition_state_id(&state.device_address),
            config_class: "native_interface_acquisition_state".into(),
            vendor: "frr".into(),
            name: state.device_address.clone(),
            version: "1".into(),
            content_json: serde_json::to_string(state)?,
            enabled: true,
            created_by: "native-frr-interface-collector".into(),
        })
        .await
}

pub async fn record_acquisition_success(
    store: &GraphStore,
    observation: &StoredFrrInterfaceShadowObservation,
) -> Result<()> {
    let now = crate::graph::common::now_ns();
    let state = StoredFrrInterfaceAcquisitionState {
        device_address: observation.device_address.clone(),
        updated_at_ns: now,
        last_attempt_started_at_ns: observation.capture_started_at_ns,
        last_attempt_finished_at_ns: observation.captured_at_ns,
        last_attempt_outcome: "success".into(),
        last_failure_class: None,
        last_failure_detail: None,
        consecutive_failures: 0,
        last_successful_observation_id: Some(observation.observation_id.clone()),
        last_successful_captured_at_ns: Some(observation.captured_at_ns),
        last_successful_capture_started_at_ns: Some(observation.capture_started_at_ns),
    };
    persist_acquisition_state(store, &state).await
}

pub async fn record_acquisition_failure(
    store: &GraphStore,
    target: &TargetConfig,
    started_at_ns: i64,
    failure_class: &str,
    failure_detail: &str,
) -> Result<()> {
    let address = target.primary_ip().to_string();
    let prior = acquisition_state(store, &address).await?;
    let now = crate::graph::common::now_ns();
    let state = StoredFrrInterfaceAcquisitionState {
        device_address: address,
        updated_at_ns: now,
        last_attempt_started_at_ns: started_at_ns,
        last_attempt_finished_at_ns: now,
        last_attempt_outcome: "failed".into(),
        last_failure_class: Some(failure_class.into()),
        last_failure_detail: Some(failure_detail.into()),
        consecutive_failures: prior
            .as_ref()
            .map(|p| {
                if p.last_attempt_outcome == "failed" {
                    p.consecutive_failures.saturating_add(1)
                } else {
                    1
                }
            })
            .unwrap_or(1),
        last_successful_observation_id: prior
            .as_ref()
            .and_then(|p| p.last_successful_observation_id.clone()),
        last_successful_captured_at_ns: prior
            .as_ref()
            .and_then(|p| p.last_successful_captured_at_ns),
        last_successful_capture_started_at_ns: prior
            .as_ref()
            .and_then(|p| p.last_successful_capture_started_at_ns),
    };
    persist_acquisition_state(store, &state).await
}

pub async fn persist_observation(
    store: &GraphStore,
    observation: &StoredFrrInterfaceShadowObservation,
) -> Result<String> {
    let id = observation.observation_id.clone();
    let content_json = serde_json::to_string(observation)?;
    if content_json.len() > MAX_STORED_OBSERVATION_BYTES {
        bail!(
            "bounded native interface shadow observation exceeds {} bytes",
            MAX_STORED_OBSERVATION_BYTES
        )
    }
    if let Some(existing_item) = store
        .list_config_items(Some("native_interface_shadow_observation".into()))
        .await?
        .into_iter()
        .find(|item| item.id == id && item.enabled)
    {
        let existing: StoredFrrInterfaceShadowObservation = serde_json::from_str(
            &existing_item.content_json,
        )
        .context("existing native interface shadow evidence is undecodable; refusing overwrite")?;
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
    store
        .upsert_config_item(ConfigItemRecord {
            id: id.clone(),
            config_class: "native_interface_shadow_observation".into(),
            vendor: "frr".into(),
            name: observation.device_address.clone(),
            version: "1".into(),
            content_json,
            enabled: true,
            created_by: "native-frr-interface-collector".into(),
        })
        .await?;
    Ok(id)
}

pub async fn latest_observation(
    store: &GraphStore,
    address: &str,
) -> Result<StoredFrrInterfaceShadowObservation> {
    let id = observation_id(address);
    let item = store
        .list_config_items(Some("native_interface_shadow_observation".into()))
        .await?
        .into_iter()
        .find(|i| i.id == id && i.enabled)
        .context("native interface shadow evidence unavailable")?;
    let observation: StoredFrrInterfaceShadowObservation =
        serde_json::from_str(&item.content_json)?;
    if observation.device_address != address {
        bail!("native interface observation identity mismatch")
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroize::Zeroizing;

    const FRR_1061_LAB_ARTIFACT: &str = "quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b";

    #[test]
    fn fixed_commands_match_the_approved_contract() {
        assert_eq!(FRR_INTERFACE_COMMAND, "vtysh -c 'show interface json'");
        assert_eq!(
            FRR_INTERFACE_CAPTURE_CONTRACT,
            "frr_vtysh_show_interface_json"
        );
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
            let err = collect_normalize_frr_interfaces(
                &target,
                &credential,
                Some(Path::new("/dev/null")),
                "bn02404-test",
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
        let observation = collect_normalize_frr_interfaces(
            &target,
            &credential,
            Some(Path::new(&known_hosts)),
            "bn02404-live-test",
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
        assert_eq!(observation.facts_emitted, 4);
        assert_eq!(observation.facts_validated, 4);
        assert_eq!(observation.facts_retained, 4);
        assert!(!observation.fact_retention_truncated);
        assert_eq!(observation.normalization_report["entities_accepted"], 4);
        assert_eq!(
            observation.normalization_report["expected_interface_total"],
            4
        );
        assert!(observation.raw_interface_json.is_some());
        std::fs::write(&out_path, serde_json::to_vec_pretty(&observation).unwrap()).unwrap();
    }
}

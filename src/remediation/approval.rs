//! Slice P3 (A13, A16–A19):
//! - A13: Deterministic second-NOS adapter workshop pilot (`nokia_srl`, `cisco_xrd`) with parser-version lineage & schema drift detection
//! - A16: Typed `RemediationPlan` & network invariant schema with deterministic SHA-256 `plan_hash`
//! - A17: Digital-twin / Containerlab change assurance gate with independent pre/postchecks and explicit emulation limitations
//! - A18: Approval-bound executor enforcing `plan_hash`, baseline config revision, expiry, device/VRF lock, idempotency, and recovery policy
//! - A19: Read-only shadow operational pilot tracker with zero write authorization

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

// ── A13: Deterministic Second-NOS Adapter Workshop Pilot ──────────────────────

pub const ADAPTER_WORKSHOP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseQualityState {
    Complete,
    PartialUnknownFields,
    SchemaMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedNosBgpNeighborObservation {
    pub nos_vendor: String,
    pub nos_version: String,
    pub parser_id: String,
    pub parser_version: String,
    pub raw_sample_sha256: String,
    pub parse_quality: ParseQualityState,
    pub vrf_name: String,
    pub neighbor_address: String,
    pub peer_as: u32,
    pub session_state: String,
    pub received_routes: u64,
    pub active_routes: u64,
    pub import_policy: Option<String>,
    pub export_policy: Option<String>,
    pub unparsed_fields: Vec<String>,
    #[serde(default)]
    pub missing_or_invalid_fields: Vec<String>,
}

/// Deterministic Nokia SR Linux (`nokia_srl`) BGP neighbor state parser committed from the adapter workshop (A13).
/// Diagnoses NOS schema changes (`unparsed_fields` / `SchemaMismatch`) separately from network failure.
pub fn parse_nokia_srl_bgp_neighbor_json(
    raw_json: &str,
) -> Result<NormalizedNosBgpNeighborObservation, String> {
    if raw_json.len() > 4 * 1024 * 1024 {
        return Err("NOS capture exceeds 4 MiB".into());
    }
    let val: serde_json::Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = val.as_object().ok_or("expected top-level JSON object")?;
    let text = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
    };
    let peer_as = obj
        .get("peer-as")
        .and_then(|v| v.as_u64())
        .and_then(|asn| u32::try_from(asn).ok())
        .filter(|asn| *asn > 0);
    let mut missing = Vec::new();
    for field in [
        "software-version",
        "network-instance",
        "peer-address",
        "session-state",
    ] {
        if text(field).is_none() {
            missing.push(field.to_string());
        }
    }
    if text("peer-address").is_some_and(|s| s.parse::<std::net::IpAddr>().is_err()) {
        missing.push("peer-address".into());
    }
    if peer_as.is_none() {
        missing.push("peer-as".into());
    }
    for field in ["received-routes", "active-routes"] {
        if obj.get(field).and_then(|v| v.as_u64()).is_none() {
            missing.push(field.to_string());
        }
    }
    for field in ["import-policy", "export-policy"] {
        if obj.contains_key(field) && text(field).is_none() {
            missing.push(field.to_string());
        }
    }
    missing.sort();
    missing.dedup();
    let known_keys: HashSet<&str> = [
        "software-version",
        "network-instance",
        "peer-address",
        "peer-as",
        "session-state",
        "received-routes",
        "active-routes",
        "import-policy",
        "export-policy",
    ]
    .into_iter()
    .collect();
    let mut unparsed: Vec<String> = obj
        .keys()
        .filter(|k| !known_keys.contains(k.as_str()))
        .cloned()
        .collect();
    unparsed.sort();
    let quality = if !missing.is_empty() {
        ParseQualityState::SchemaMismatch
    } else if !unparsed.is_empty() {
        ParseQualityState::PartialUnknownFields
    } else {
        ParseQualityState::Complete
    };
    // Legacy numeric fields remain for compatibility, but absent/invalid values
    // always carry SchemaMismatch plus explicit field diagnostics. Native capture
    // projection uses Option counters instead of numeric placeholders.
    Ok(NormalizedNosBgpNeighborObservation {
        nos_vendor: "nokia_srl".into(),
        nos_version: text("software-version").unwrap_or("unknown").into(),
        parser_id: "nokia-srl-bgp-neighbor".into(),
        parser_version: "1.1.0".into(),
        raw_sample_sha256: hex::encode(Sha256::digest(raw_json.as_bytes())),
        parse_quality: quality,
        vrf_name: text("network-instance").unwrap_or("unknown").into(),
        neighbor_address: text("peer-address").unwrap_or("unknown").into(),
        peer_as: peer_as.unwrap_or(0),
        session_state: text("session-state").unwrap_or("unknown").into(),
        received_routes: obj
            .get("received-routes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        active_routes: obj
            .get("active-routes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        import_policy: text("import-policy").map(String::from),
        export_policy: text("export-policy").map(String::from),
        unparsed_fields: unparsed,
        missing_or_invalid_fields: missing,
    })
}

// ── A16: Intent / Invariant & Typed RemediationPlan Schema ────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisruptionLevel {
    NoneExpected,
    ControlPlaneReconvergence,
    TrafficShiftOrDrain,
    SessionResetRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlastRadiusSummary {
    pub affected_devices: Vec<String>,
    pub affected_vrfs: Vec<String>,
    pub affected_ebgp_peers: Vec<String>,
    pub estimated_affected_prefixes: usize,
    pub expected_disruption: DisruptionLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInvariant {
    pub invariant_id: String,
    pub description: String,
    /// e.g. "prefix_reachable_in_loc_rib", "no_ebgp_peer_drop", "vrf_isolation_preserved"
    pub invariant_type: String,
    pub target_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssuranceCheckSpec {
    pub check_id: String,
    pub description: String,
    pub command_or_query: String,
    pub expected_assertion: String,
    pub read_only: bool,
}

/// Strategy §8: "If postchecks fail, halt and follow the approved recovery policy;
/// blindly applying a reverse diff is not universally safe."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPolicy {
    ExplicitRollbackSteps,
    HaltAndEscalate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationPlan {
    pub plan_id: String,
    pub tenant_id: String,
    pub device_address: String,
    pub vrf_rd: String,
    pub nos_vendor: String,
    pub baseline_config_revision: String,
    pub candidate_diff: String,
    pub intended_behavior: String,
    pub blast_radius: BlastRadiusSummary,
    pub invariants: Vec<NetworkInvariant>,
    pub prechecks: Vec<AssuranceCheckSpec>,
    pub postchecks: Vec<AssuranceCheckSpec>,
    pub recovery_policy: RecoveryPolicy,
    pub rollback_steps: Vec<String>,
    pub created_at_epoch_secs: u64,
    pub expires_at_epoch_secs: u64,
}

impl RemediationPlan {
    /// Compute deterministic SHA-256 `plan_hash` over all safety-critical plan fields (Strategy §8, A16/A18).
    pub fn compute_plan_hash(&self) -> String {
        let canonical = serde_json::json!({
            "plan_id": self.plan_id,
            "tenant_id": self.tenant_id,
            "device_address": self.device_address,
            "vrf_rd": self.vrf_rd,
            "nos_vendor": self.nos_vendor,
            "baseline_config_revision": self.baseline_config_revision,
            "candidate_diff": self.candidate_diff,
            "intended_behavior": self.intended_behavior,
            "blast_radius": self.blast_radius,
            "invariants": self.invariants,
            "prechecks": self.prechecks,
            "postchecks": self.postchecks,
            "recovery_policy": self.recovery_policy,
            "rollback_steps": self.rollback_steps,
            "expires_at_epoch_secs": self.expires_at_epoch_secs,
        });
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        hex::encode(Sha256::digest(&bytes))
    }

    /// Validate structural completeness of a proposed `RemediationPlan` before lab/digital-twin evaluation.
    pub fn validate_schema(&self) -> Result<(), String> {
        if self.plan_id.trim().is_empty() {
            return Err("plan_id must not be empty".to_string());
        }
        if self.device_address.trim().is_empty() || self.vrf_rd.trim().is_empty() {
            return Err("device_address and vrf_rd scope must be non-empty".to_string());
        }
        if self.baseline_config_revision.trim().is_empty() {
            return Err("baseline_config_revision must be specified".to_string());
        }
        if self.candidate_diff.trim().is_empty() {
            return Err("candidate_diff must not be empty".to_string());
        }
        if self.prechecks.is_empty() || self.postchecks.is_empty() {
            return Err("both prechecks and postchecks must be non-empty".to_string());
        }
        if self.prechecks.iter().any(|c| !c.read_only)
            || self.postchecks.iter().any(|c| !c.read_only)
        {
            return Err("all prechecks and postchecks must be read_only = true".to_string());
        }
        if self.recovery_policy == RecoveryPolicy::ExplicitRollbackSteps
            && self.rollback_steps.is_empty()
        {
            return Err(
                "rollback_steps must not be empty when recovery_policy is ExplicitRollbackSteps"
                    .to_string(),
            );
        }
        if self.expires_at_epoch_secs <= self.created_at_epoch_secs {
            return Err(
                "expires_at_epoch_secs must be greater than created_at_epoch_secs".to_string(),
            );
        }
        Ok(())
    }
}

// ── A17: Digital-Twin / Containerlab Change Assurance Gate ────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceFidelity {
    #[default]
    ModeledSnapshotSimulation,
    LiveContainerlabVerified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigitalTwinSnapshot {
    pub lab_topology_id: String,
    pub nos_image_ref: String,
    #[serde(default)]
    pub fidelity: AssuranceFidelity,
    pub target_prefix_reachable_before: bool,
    pub target_prefix_reachable_after: bool,
    pub ebgp_peers_up_before: usize,
    pub ebgp_peers_up_after: usize,
    pub vrf_isolation_intact: bool,
    pub max_allowed_blast_radius_devices: usize,
    pub unsupported_vendor_constructs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceVerdict {
    Verified,
    VerifiedWithLimitations,
    FailedInvariants,
    InvalidPlanSchema,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantCheckResult {
    pub invariant_id: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeAssuranceReport {
    pub report_id: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub lab_topology_id: String,
    #[serde(default)]
    pub fidelity: AssuranceFidelity,
    pub verdict: AssuranceVerdict,
    pub executable: bool,
    pub invariant_results: Vec<InvariantCheckResult>,
    pub unsupported_constructs: Vec<String>,
    pub emulation_fidelity_limitations: Vec<String>,
    /// BN-002 provenance: optional HMAC-SHA256 signature over `canonical_bytes()`.
    /// Excluded from `canonical_bytes()`/`compute_report_hash()` so signing does
    /// not change the report hash bound into approvals.
    #[serde(default)]
    pub signature_algorithm: String,
    #[serde(default)]
    pub key_id: String,
    #[serde(default)]
    pub signature_hex: String,
}

impl ChangeAssuranceReport {
    /// Deterministic, unambiguous length-prefixed bytes for cryptographic hashing and signing.
    /// Eliminates delimiter collisions across report identity, plan, digital-twin fidelity, verdict,
    /// executable status, invariant check results, unsupported constructs, and emulation limitations.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"BONSAI-ASSURANCE-REPORT-V2\n");
        let fidelity_str = format!("{:?}", self.fidelity);
        let verdict_str = format!("{:?}", self.verdict);
        let executable_str = if self.executable { "1" } else { "0" };

        let fields: [(&str, &[u8]); 7] = [
            ("report_id", self.report_id.as_bytes()),
            ("plan_id", self.plan_id.as_bytes()),
            ("plan_hash", self.plan_hash.as_bytes()),
            ("lab_topology_id", self.lab_topology_id.as_bytes()),
            ("fidelity", fidelity_str.as_bytes()),
            ("verdict", verdict_str.as_bytes()),
            ("executable", executable_str.as_bytes()),
        ];
        for (name, val) in fields {
            buf.extend_from_slice(name.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(&(val.len() as u64).to_be_bytes());
            buf.extend_from_slice(val);
            buf.push(b'\n');
        }

        buf.extend_from_slice(b"invariants:");
        buf.extend_from_slice(&(self.invariant_results.len() as u64).to_be_bytes());
        buf.push(b'\n');
        for inv in &self.invariant_results {
            let inv_id = inv.invariant_id.as_bytes();
            let passed_b: &[u8] = if inv.passed { b"1" } else { b"0" };
            let detail_b = inv.detail.as_bytes();

            buf.extend_from_slice(&(inv_id.len() as u64).to_be_bytes());
            buf.extend_from_slice(inv_id);
            buf.push(b':');
            buf.extend_from_slice(passed_b);
            buf.push(b':');
            buf.extend_from_slice(&(detail_b.len() as u64).to_be_bytes());
            buf.extend_from_slice(detail_b);
            buf.push(b'\n');
        }

        buf.extend_from_slice(b"unsupported_constructs:");
        buf.extend_from_slice(&(self.unsupported_constructs.len() as u64).to_be_bytes());
        buf.push(b'\n');
        for u in &self.unsupported_constructs {
            let u_bytes = u.as_bytes();
            buf.extend_from_slice(&(u_bytes.len() as u64).to_be_bytes());
            buf.extend_from_slice(u_bytes);
            buf.push(b'\n');
        }

        buf.extend_from_slice(b"emulation_fidelity_limitations:");
        buf.extend_from_slice(&(self.emulation_fidelity_limitations.len() as u64).to_be_bytes());
        buf.push(b'\n');
        for lim in &self.emulation_fidelity_limitations {
            let lim_bytes = lim.as_bytes();
            buf.extend_from_slice(&(lim_bytes.len() as u64).to_be_bytes());
            buf.extend_from_slice(lim_bytes);
            buf.push(b'\n');
        }

        buf
    }

    /// Deterministic SHA-256 hash binding complete report content unambiguously.
    pub fn compute_report_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&self.canonical_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Sign the report's canonical bytes so enforced execution can verify the
    /// assurance result came from a trusted producer (BN-002 provenance).
    pub fn sign_hmac_sha256(&mut self, key_id: &str, secret_key: &[u8]) {
        self.signature_algorithm = "HMAC-SHA256".to_string();
        self.key_id = key_id.to_string();
        let mut mac =
            HmacSha256::new_from_slice(secret_key).expect("HMAC can take key of any size");
        mac.update(&self.canonical_bytes());
        self.signature_hex = hex::encode(mac.finalize().into_bytes());
    }

    pub fn verify_signature(&self, secret_key: &[u8]) -> Result<(), ExecutionRejectionReason> {
        if self.signature_algorithm != "HMAC-SHA256" || self.signature_hex.is_empty() {
            return Err(ExecutionRejectionReason::InvalidSignature);
        }
        let sig_bytes = hex::decode(&self.signature_hex)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)?;
        let mut mac = HmacSha256::new_from_slice(secret_key)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)?;
        mac.update(&self.canonical_bytes());
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)
    }
}

/// Evaluate a `RemediationPlan` against a `DigitalTwinSnapshot` (Batfish / Containerlab gate, A17).
/// No unverified plan can be marked `executable = true`.
pub fn verify_plan_in_digital_twin(
    plan: &RemediationPlan,
    twin: &DigitalTwinSnapshot,
) -> ChangeAssuranceReport {
    let plan_hash = plan.compute_plan_hash();
    let mut limitations = vec![
        "Containerlab emulation and modeled config analysis do not prove ASIC FIB programming, optical layer margins, or microburst queueing behavior.".to_string(),
    ];
    match twin.fidelity {
        AssuranceFidelity::ModeledSnapshotSimulation => {
            limitations.push(
                "Modeled snapshot simulation: evaluations reflect caller-supplied snapshot properties rather than real ASIC/kernel state.".to_string(),
            );
        }
        AssuranceFidelity::LiveContainerlabVerified => {
            limitations.push(
                "Live Containerlab verified: verified on containerized network operating system instance.".to_string(),
            );
        }
    }

    if let Err(schema_err) = plan.validate_schema() {
        return ChangeAssuranceReport {
            report_id: format!("assurance-{}", plan.plan_id),
            plan_id: plan.plan_id.clone(),
            plan_hash,
            lab_topology_id: twin.lab_topology_id.clone(),
            fidelity: twin.fidelity,
            verdict: AssuranceVerdict::InvalidPlanSchema,
            executable: false,
            invariant_results: vec![InvariantCheckResult {
                invariant_id: "plan_schema_valid".to_string(),
                passed: false,
                detail: schema_err,
            }],
            unsupported_constructs: twin.unsupported_vendor_constructs.clone(),
            emulation_fidelity_limitations: limitations,
            signature_algorithm: String::new(),
            key_id: String::new(),
            signature_hex: String::new(),
        };
    }

    let mut results = Vec::new();

    // 1. Target prefix reachability restored in Loc-RIB
    results.push(InvariantCheckResult {
        invariant_id: "prefix_reachable_in_loc_rib".to_string(),
        passed: twin.target_prefix_reachable_after,
        detail: format!(
            "target_prefix_reachable_before={}, after={}",
            twin.target_prefix_reachable_before, twin.target_prefix_reachable_after
        ),
    });

    // 2. No eBGP peer session regression
    let peers_ok =
        twin.ebgp_peers_up_after >= twin.ebgp_peers_up_before && twin.ebgp_peers_up_after > 0;
    results.push(InvariantCheckResult {
        invariant_id: "no_ebgp_peer_drop".to_string(),
        passed: peers_ok,
        detail: format!(
            "ebgp_peers_up_before={}, after={}",
            twin.ebgp_peers_up_before, twin.ebgp_peers_up_after
        ),
    });

    // 3. VRF isolation preserved
    results.push(InvariantCheckResult {
        invariant_id: "vrf_isolation_preserved".to_string(),
        passed: twin.vrf_isolation_intact,
        detail: format!("vrf_isolation_intact={}", twin.vrf_isolation_intact),
    });

    // 4. Blast radius within bounds
    let blast_ok =
        plan.blast_radius.affected_devices.len() <= twin.max_allowed_blast_radius_devices;
    results.push(InvariantCheckResult {
        invariant_id: "blast_radius_bounded".to_string(),
        passed: blast_ok,
        detail: format!(
            "affected_devices={} <= max_allowed={}",
            plan.blast_radius.affected_devices.len(),
            twin.max_allowed_blast_radius_devices
        ),
    });

    let all_passed = results.iter().all(|r| r.passed);
    let verdict = if !all_passed {
        AssuranceVerdict::FailedInvariants
    } else if !twin.unsupported_vendor_constructs.is_empty() {
        AssuranceVerdict::VerifiedWithLimitations
    } else {
        AssuranceVerdict::Verified
    };

    ChangeAssuranceReport {
        report_id: format!("assurance-{}", plan.plan_id),
        plan_id: plan.plan_id.clone(),
        plan_hash,
        lab_topology_id: twin.lab_topology_id.clone(),
        fidelity: twin.fidelity,
        executable: all_passed,
        verdict,
        invariant_results: results,
        unsupported_constructs: twin.unsupported_vendor_constructs.clone(),
        emulation_fidelity_limitations: limitations,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    }
}

// ── A18: Approval-Bound Executor ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPlanApproval {
    pub approval_id: String,
    pub plan_id: String,
    pub approved_plan_hash: String,
    pub expected_baseline_config_revision: String,
    pub approved_scope_device: String,
    pub approved_scope_vrf: String,
    pub assurance_report_id: String,
    #[serde(default)]
    pub approved_assurance_report_hash: String,
    #[serde(default)]
    pub tenant_id: String,
    pub approved_by: String,
    pub operator_rationale: String,
    pub approved_at_epoch_secs: u64,
    pub expires_at_epoch_secs: u64,
    #[serde(default)]
    pub signature_algorithm: String,
    #[serde(default)]
    pub key_id: String,
    #[serde(default)]
    pub signature_hex: String,
}

impl SignedPlanApproval {
    /// Deterministic, unambiguous versioned length-prefixed bytes for cryptographic signing.
    /// Eliminates delimiter collisions across approval fields.
    pub fn canonical_signing_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"BONSAI-APPROVAL-V1\n");
        let timestamps = format!(
            "{}:{}",
            self.approved_at_epoch_secs, self.expires_at_epoch_secs
        );
        let fields: [(&str, &[u8]); 13] = [
            ("approval_id", self.approval_id.as_bytes()),
            ("plan_id", self.plan_id.as_bytes()),
            ("approved_plan_hash", self.approved_plan_hash.as_bytes()),
            (
                "expected_baseline_config_revision",
                self.expected_baseline_config_revision.as_bytes(),
            ),
            (
                "approved_scope_device",
                self.approved_scope_device.as_bytes(),
            ),
            ("approved_scope_vrf", self.approved_scope_vrf.as_bytes()),
            ("assurance_report_id", self.assurance_report_id.as_bytes()),
            (
                "approved_assurance_report_hash",
                self.approved_assurance_report_hash.as_bytes(),
            ),
            ("tenant_id", self.tenant_id.as_bytes()),
            ("approved_by", self.approved_by.as_bytes()),
            ("key_id", self.key_id.as_bytes()),
            ("signature_algorithm", self.signature_algorithm.as_bytes()),
            ("timestamps", timestamps.as_bytes()),
        ];
        for (name, val) in fields {
            buf.extend_from_slice(name.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(&(val.len() as u64).to_be_bytes());
            buf.push(b':');
            buf.extend_from_slice(val);
            buf.push(b'\n');
        }
        buf
    }

    pub fn sign_hmac_sha256(&mut self, key_id: &str, secret_key: &[u8]) {
        self.signature_algorithm = "HMAC-SHA256".to_string();
        self.key_id = key_id.to_string();
        let mut mac =
            HmacSha256::new_from_slice(secret_key).expect("HMAC can take key of any size");
        mac.update(&self.canonical_signing_bytes());
        let result = mac.finalize();
        self.signature_hex = hex::encode(result.into_bytes());
    }

    pub fn verify_signature(&self, secret_key: &[u8]) -> Result<(), ExecutionRejectionReason> {
        if self.signature_algorithm != "HMAC-SHA256" || self.signature_hex.is_empty() {
            return Err(ExecutionRejectionReason::InvalidSignature);
        }
        let sig_bytes = hex::decode(&self.signature_hex)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)?;
        let mut mac = HmacSha256::new_from_slice(secret_key)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)?;
        mac.update(&self.canonical_signing_bytes());
        mac.verify_slice(&sig_bytes)
            .map_err(|_| ExecutionRejectionReason::InvalidSignature)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnforcementMode {
    #[default]
    Simulation,
    Enforced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRejectionReason {
    UnverifiedInDigitalTwin,
    PlanHashMismatch,
    StaleBaselineConfigRevision,
    ApprovalExpired,
    ScopeMismatch,
    TenantMismatch,
    EmptyTenantId,
    ConflictingChangeLockHeld,
    PrecheckFailed(String),
    InvalidSignature,
    MissingSignatureAuthority,
    RevokedSigningKey(String),
    UnknownKeyId(String),
    ModeledAssuranceRejectedInEnforcedMode,
    AssuranceReportHashMismatch,
    MissingAssuranceBinding,
    SimulationReplayRejectedInEnforcedMode,
    DurableBackingRequiredInEnforcedMode,
    IdempotencyConflict(String),
    InFlightExecutionInterrupted(String),
    InterruptedExecutionNotFound(String),
    MissingAssuranceSignature,
    JournalPersistenceError(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    CommittedAndPostchecksPassed,
    RolledBackAfterPostcheckFailure,
    HaltedForOperatorEscalation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionJournalEntry {
    pub execution_id: String,
    pub idempotency_key: String,
    pub plan_id: String,
    pub plan_hash: String,
    #[serde(default)]
    pub tenant_id: String,
    pub approval_id: String,
    pub device_address: String,
    pub vrf_rd: String,
    pub outcome: ExecutionOutcome,
    pub prechecks_passed: bool,
    pub postchecks_passed: bool,
    pub steps_applied: Vec<String>,
    pub recovery_actions_taken: Vec<String>,
    pub deduplicated_replay: bool,
    #[serde(default)]
    pub enforcement_mode: ExecutionEnforcementMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIntentRecord {
    pub execution_id: String,
    pub idempotency_key: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub tenant_id: String,
    pub approval_id: String,
    pub device_address: String,
    pub vrf_rd: String,
    pub started_at_epoch_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum ExecutionJournalRecord {
    LockAcquired {
        device_address: String,
        vrf_rd: String,
        owner_id: String,
        acquired_at_epoch_secs: u64,
    },
    LockReleased {
        device_address: String,
        vrf_rd: String,
        owner_id: String,
        released_at_epoch_secs: u64,
    },
    ExecutionIntent {
        execution_id: String,
        idempotency_key: String,
        plan_id: String,
        plan_hash: String,
        tenant_id: String,
        approval_id: String,
        device_address: String,
        vrf_rd: String,
        started_at_epoch_secs: u64,
    },
    ExecutionAborted {
        execution_id: String,
        idempotency_key: String,
        reason: String,
        aborted_at_epoch_secs: u64,
    },
    ExecutionCompleted {
        #[serde(flatten)]
        entry: ExecutionJournalEntry,
    },
}

fn truncate_file_to_len(path: &Path, valid_len: usize) -> Result<(), std::io::Error> {
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.set_len(valid_len as u64)?;
    f.sync_all()?;
    Ok(())
}

fn append_newline_and_sync(path: &Path) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(b"\n")?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

fn parse_journal_record(line: &str) -> Result<ExecutionJournalRecord, serde_json::Error> {
    if let Ok(record) = serde_json::from_str::<ExecutionJournalRecord>(line) {
        return Ok(record);
    }
    // Backward compatibility for legacy files where ExecutionJournalEntry was serialized directly
    serde_json::from_str::<ExecutionJournalEntry>(line)
        .map(|entry| ExecutionJournalRecord::ExecutionCompleted { entry })
}

fn append_journal_record(
    path: &Path,
    record: &ExecutionJournalRecord,
) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{}", line)?;
    file.sync_data()?;
    Ok(())
}

// ── BN-002: Durable signing-key lifecycle ─────────────────────────────────────
//
// Signing keys and revocations are persisted beside the executor journal so a
// process restart cannot silently resurrect a revoked key or lose the signing
// authority required by enforced mode. The store is a single JSON document
// rewritten atomically (temp file + rename, mode 0600 on Unix). This is a
// development-prototype file store: production custody (vault/HSM) remains
// BN-015/BN-019 scope.

const EXECUTOR_KEY_STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSigningKey {
    secret_hex: String,
    /// "active" | "revoked"
    status: String,
    created_at_epoch_secs: u64,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedKeyStore {
    version: u32,
    #[serde(default)]
    keys: BTreeMap<String, PersistedSigningKey>,
}

fn executor_keys_file_path(journal_path: &Path) -> PathBuf {
    journal_path.with_extension("keys.json")
}

struct FileLockGuard {
    file: std::fs::File,
}

impl FileLockGuard {
    fn acquire(path: &Path) -> Result<Self, std::io::Error> {
        use std::os::unix::io::AsRawFd;
        let lock_path = path.with_extension("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        let fd = file.as_raw_fd();
        let res = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if res != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { file })
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let fd = self.file.as_raw_fd();
        let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    }
}

/// Fault-injection point for BN-001 crash-recovery verification.
/// `#[doc(hidden)]`: used only by integration tests that spawn a real child
/// process and kill it after a durable journal record is synced, simulating
/// power loss at each persistence phase. Never enabled in production paths.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorCrashPoint {
    AfterLockAcquiredPersisted,
    AfterIntentPersisted,
    AfterCompletedPersisted,
    AfterAbortedPersisted,
    AfterLockReleasedPersisted,
}

#[derive(Debug, Default)]
pub struct ApprovalBoundExecutor {
    journal_path: Option<PathBuf>,
    enforcement_mode: ExecutionEnforcementMode,
    signing_keys: HashMap<String, Vec<u8>>,
    revoked_keys: HashSet<String>,
    active_scope_locks: HashMap<(String, String), String>,
    in_flight_executions: HashMap<String, ExecutionIntentRecord>,
    completed_by_idempotency_key: BTreeMap<String, ExecutionJournalEntry>,
    journal: Vec<ExecutionJournalEntry>,
    crash_point: Option<ExecutorCrashPoint>,
    /// Set when in-memory key state has not yet been flushed to the durable
    /// key store; the next journal-locked operation retries persistence.
    keys_dirty: bool,
    /// key_id -> (created_at_epoch_secs, note) for durable-store round-trips.
    key_meta: HashMap<String, (u64, String)>,
}

impl ApprovalBoundExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_enforcement_mode(mut self, mode: ExecutionEnforcementMode) -> Self {
        self.enforcement_mode = mode;
        self
    }

    pub fn with_signing_key(mut self, key_id: &str, secret: &[u8]) -> Self {
        self.signing_keys
            .insert(key_id.to_string(), secret.to_vec());
        self.revoked_keys.remove(key_id);
        self.key_meta
            .entry(key_id.to_string())
            .or_insert((0, String::new()));
        self.keys_dirty = true;
        // Best effort at construction time; journal-locked operations retry
        // and surface persistence failures via JournalPersistenceError.
        let _ = self.persist_key_store_if_dirty();
        self
    }

    pub fn with_revoked_key(mut self, key_id: &str) -> Self {
        self.revoked_keys.insert(key_id.to_string());
        self.key_meta
            .entry(key_id.to_string())
            .or_insert((0, String::new()));
        self.keys_dirty = true;
        let _ = self.persist_key_store_if_dirty();
        self
    }

    pub fn with_signing_secret(mut self, secret: &[u8]) -> Self {
        self = self.with_signing_key("", secret);
        self.with_signing_key("default", secret)
    }

    /// Register a signing key (or rotate to a new `key_id` while keeping
    /// existing keys valid for an overlap window). Re-registering a revoked
    /// `key_id` explicitly reactivates it. Durable when the executor is
    /// journal-backed; the key store is reloaded under the journal lock so
    /// concurrent executor instances observe the same keyring.
    pub fn register_signing_key(
        &mut self,
        key_id: &str,
        secret: &[u8],
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        self.lock_and_reload_keys()?;
        self.signing_keys
            .insert(key_id.to_string(), secret.to_vec());
        self.revoked_keys.remove(key_id);
        self.key_meta
            .insert(key_id.to_string(), (now_epoch_secs, String::new()));
        self.keys_dirty = true;
        self.persist_key_store_if_dirty().map_err(|e| {
            ExecutionRejectionReason::JournalPersistenceError(format!(
                "failed to persist signing key: {e}"
            ))
        })
    }

    /// Revoke a signing key durably. Revocation survives restart; approvals
    /// signed by the key are rejected with `RevokedSigningKey`. Idempotent.
    pub fn revoke_signing_key(
        &mut self,
        key_id: &str,
        _now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        self.lock_and_reload_keys()?;
        self.revoked_keys.insert(key_id.to_string());
        self.key_meta
            .entry(key_id.to_string())
            .or_insert((0, String::new()));
        self.keys_dirty = true;
        self.persist_key_store_if_dirty().map_err(|e| {
            ExecutionRejectionReason::JournalPersistenceError(format!(
                "failed to persist key revocation: {e}"
            ))
        })
    }

    /// Acquire the journal file lock (when durable) and reload the persisted
    /// key store so this instance observes registrations/revocations made by
    /// other executor instances.
    fn lock_and_reload_keys(&mut self) -> Result<(), ExecutionRejectionReason> {
        if let Some(ref path) = self.journal_path.clone() {
            let _guard = FileLockGuard::acquire(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to acquire journal file lock: {e}"
                ))
            })?;
            self.reload_keys_from_disk().map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload executor keys: {e}"
                ))
            })?;
        }
        Ok(())
    }

    /// Flush pending key mutations, then reload the durable store into memory.
    /// Fails closed when the store vanished after keys existed, mirroring the
    /// journal's durability checks.
    fn reload_keys_from_disk(&mut self) -> Result<(), std::io::Error> {
        let Some(journal_path) = self.journal_path.clone() else {
            return Ok(());
        };
        if self.keys_dirty {
            self.persist_key_store()?;
            self.keys_dirty = false;
        }
        let keys_path = executor_keys_file_path(&journal_path);
        if !keys_path.exists() {
            if !self.signing_keys.is_empty() || !self.revoked_keys.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "durable executor key store vanished after keys existed",
                ));
            }
            return Ok(());
        }
        let raw = std::fs::read(&keys_path)?;
        let store: PersistedKeyStore = serde_json::from_slice(&raw).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("corrupt executor key store: {e}"),
            )
        })?;
        if store.version != EXECUTOR_KEY_STORE_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported executor key store version {}", store.version),
            ));
        }
        self.signing_keys.clear();
        self.revoked_keys.clear();
        self.key_meta.clear();
        for (key_id, rec) in &store.keys {
            if !rec.secret_hex.is_empty() {
                if let Ok(secret) = hex::decode(&rec.secret_hex) {
                    self.signing_keys.insert(key_id.clone(), secret);
                }
            }
            if rec.status == "revoked" {
                self.revoked_keys.insert(key_id.clone());
            }
            self.key_meta.insert(
                key_id.clone(),
                (rec.created_at_epoch_secs, rec.note.clone()),
            );
        }
        Ok(())
    }

    fn persist_key_store_if_dirty(&mut self) -> Result<(), std::io::Error> {
        if !self.keys_dirty {
            return Ok(());
        }
        self.persist_key_store()?;
        self.keys_dirty = false;
        Ok(())
    }

    fn persist_key_store(&self) -> Result<(), std::io::Error> {
        let Some(journal_path) = &self.journal_path else {
            return Ok(());
        };
        let path = executor_keys_file_path(journal_path);
        let mut keys = BTreeMap::new();
        for (key_id, secret) in &self.signing_keys {
            let (created, note) = self
                .key_meta
                .get(key_id)
                .cloned()
                .unwrap_or((0, String::new()));
            keys.insert(
                key_id.clone(),
                PersistedSigningKey {
                    secret_hex: hex::encode(secret),
                    status: if self.revoked_keys.contains(key_id) {
                        "revoked".to_string()
                    } else {
                        "active".to_string()
                    },
                    created_at_epoch_secs: created,
                    note,
                },
            );
        }
        // Defensive revocations for key_ids that have no secret material.
        for key_id in &self.revoked_keys {
            keys.entry(key_id.clone())
                .or_insert_with(|| PersistedSigningKey {
                    secret_hex: String::new(),
                    status: "revoked".to_string(),
                    created_at_epoch_secs: self.key_meta.get(key_id).map(|m| m.0).unwrap_or(0),
                    note: self
                        .key_meta
                        .get(key_id)
                        .map(|m| m.1.clone())
                        .unwrap_or_default(),
                });
        }
        let store = PersistedKeyStore {
            version: EXECUTOR_KEY_STORE_VERSION,
            keys,
        };
        let bytes = serde_json::to_vec_pretty(&store)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp_path = path.with_extension("tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&tmp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn with_crash_point_for_testing(mut self, point: ExecutorCrashPoint) -> Self {
        self.crash_point = Some(point);
        self
    }

    /// `abort()` skips destructors, matching SIGKILL/power-loss semantics: the
    /// OS releases the flock, in-memory state is lost, and disk retains exactly
    /// the records synced so far.
    fn crash_if_configured(&self, point: ExecutorCrashPoint) {
        if self.crash_point == Some(point) {
            std::process::abort();
        }
    }

    pub fn in_flight_executions(&self) -> Vec<ExecutionIntentRecord> {
        self.in_flight_executions.values().cloned().collect()
    }

    pub fn active_locks(&self) -> &HashMap<(String, String), String> {
        &self.active_scope_locks
    }

    fn reload_from_journal_path(&mut self, path: &Path) -> Result<(), std::io::Error> {
        if !path.exists() {
            if !self.journal.is_empty()
                || !self.completed_by_idempotency_key.is_empty()
                || !self.in_flight_executions.is_empty()
                || !self.active_scope_locks.is_empty()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "durable journal file vanished after persisted history existed",
                ));
            }
            return Ok(());
        }

        let raw_bytes = std::fs::read(path)?;
        if raw_bytes.is_empty() {
            if !self.journal.is_empty()
                || !self.completed_by_idempotency_key.is_empty()
                || !self.in_flight_executions.is_empty()
                || !self.active_scope_locks.is_empty()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "durable journal unexpectedly empty after persisted history existed",
                ));
            }
            return Ok(());
        }

        let mut new_active_scope_locks = HashMap::new();
        let mut new_in_flight_executions = HashMap::new();
        let mut new_completed_by_idempotency_key = BTreeMap::new();
        let mut new_journal = Vec::new();

        let mut cursor = 0usize;
        let mut valid_len = 0usize;
        let mut line_idx = 0usize;

        while cursor < raw_bytes.len() {
            let next_nl = raw_bytes[cursor..].iter().position(|&b| b == b'\n');
            match next_nl {
                Some(rel_end) => {
                    let line_end = cursor + rel_end;
                    let line_bytes = &raw_bytes[cursor..line_end];
                    let line_str = std::str::from_utf8(line_bytes).map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("corrupted UTF-8 in journal at line {}: {}", line_idx + 1, e),
                        )
                    })?;
                    let trimmed = line_str.trim();
                    if !trimmed.is_empty() {
                        let record = parse_journal_record(trimmed).map_err(|e| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("corrupted journal record at line {}: {}", line_idx + 1, e),
                            )
                        })?;
                        Self::apply_record_to_state(
                            record,
                            &mut new_active_scope_locks,
                            &mut new_in_flight_executions,
                            &mut new_completed_by_idempotency_key,
                            &mut new_journal,
                        );
                    }
                    valid_len = line_end + 1;
                    cursor = line_end + 1;
                    line_idx += 1;
                }
                None => {
                    let tail_bytes = &raw_bytes[cursor..];
                    match std::str::from_utf8(tail_bytes) {
                        Ok(tail_str) if tail_str.trim().is_empty() => {
                            truncate_file_to_len(path, valid_len)?;
                        }
                        Ok(tail_str) => match parse_journal_record(tail_str.trim()) {
                            Ok(record) => {
                                append_newline_and_sync(path)?;
                                Self::apply_record_to_state(
                                    record,
                                    &mut new_active_scope_locks,
                                    &mut new_in_flight_executions,
                                    &mut new_completed_by_idempotency_key,
                                    &mut new_journal,
                                );
                            }
                            Err(_) => {
                                truncate_file_to_len(path, valid_len)?;
                            }
                        },
                        Err(_) => {
                            truncate_file_to_len(path, valid_len)?;
                        }
                    }
                    break;
                }
            }
        }

        if !self.journal.is_empty() && new_journal.len() < self.journal.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "durable journal shrunk unexpectedly: expected at least {} entries, read {}",
                    self.journal.len(),
                    new_journal.len()
                ),
            ));
        }

        self.active_scope_locks = new_active_scope_locks;
        self.in_flight_executions = new_in_flight_executions;
        self.completed_by_idempotency_key = new_completed_by_idempotency_key;
        self.journal = new_journal;

        Ok(())
    }

    pub fn open_or_create(journal_path: Option<&Path>) -> Result<Self, std::io::Error> {
        let mut executor = Self::new();
        if let Some(path) = journal_path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _file_lock = FileLockGuard::acquire(path)?;
            executor.reload_from_journal_path(path)?;
            executor.journal_path = Some(path.to_path_buf());
            executor.reload_keys_from_disk()?;
        }
        Ok(executor)
    }

    fn apply_record_to_state(
        record: ExecutionJournalRecord,
        active_scope_locks: &mut HashMap<(String, String), String>,
        in_flight_executions: &mut HashMap<String, ExecutionIntentRecord>,
        completed_by_idempotency_key: &mut BTreeMap<String, ExecutionJournalEntry>,
        journal: &mut Vec<ExecutionJournalEntry>,
    ) {
        match record {
            ExecutionJournalRecord::LockAcquired {
                device_address,
                vrf_rd,
                owner_id,
                ..
            } => {
                active_scope_locks.insert((device_address, vrf_rd), owner_id);
            }
            ExecutionJournalRecord::LockReleased {
                device_address,
                vrf_rd,
                owner_id,
                ..
            } => {
                let key = (device_address, vrf_rd);
                if active_scope_locks.get(&key) == Some(&owner_id) {
                    active_scope_locks.remove(&key);
                }
            }
            ExecutionJournalRecord::ExecutionIntent {
                execution_id,
                idempotency_key,
                plan_id,
                plan_hash,
                tenant_id,
                approval_id,
                device_address,
                vrf_rd,
                started_at_epoch_secs,
            } => {
                in_flight_executions.insert(
                    idempotency_key.clone(),
                    ExecutionIntentRecord {
                        execution_id,
                        idempotency_key,
                        plan_id,
                        plan_hash,
                        tenant_id,
                        approval_id,
                        device_address,
                        vrf_rd,
                        started_at_epoch_secs,
                    },
                );
            }
            ExecutionJournalRecord::ExecutionAborted {
                idempotency_key, ..
            } => {
                in_flight_executions.remove(&idempotency_key);
            }
            ExecutionJournalRecord::ExecutionCompleted { entry } => {
                in_flight_executions.remove(&entry.idempotency_key);
                completed_by_idempotency_key.insert(entry.idempotency_key.clone(), entry.clone());
                journal.push(entry);
            }
        }
    }

    fn acquire_scope_lock_locked(
        &mut self,
        path: &Path,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        let key = (device_address.to_string(), vrf_rd.to_string());
        if let Some(existing_owner) = self.active_scope_locks.get(&key) {
            if existing_owner != owner_id {
                return Err(ExecutionRejectionReason::ConflictingChangeLockHeld);
            }
            return Ok(());
        }

        let record = ExecutionJournalRecord::LockAcquired {
            device_address: device_address.to_string(),
            vrf_rd: vrf_rd.to_string(),
            owner_id: owner_id.to_string(),
            acquired_at_epoch_secs: now_epoch_secs,
        };
        append_journal_record(path, &record).map_err(|e| {
            ExecutionRejectionReason::JournalPersistenceError(format!(
                "failed to persist lock acquisition: {e}"
            ))
        })?;
        self.active_scope_locks.insert(key, owner_id.to_string());
        self.crash_if_configured(ExecutorCrashPoint::AfterLockAcquiredPersisted);
        Ok(())
    }

    fn release_scope_lock_locked(
        &mut self,
        path: &Path,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        let key = (device_address.to_string(), vrf_rd.to_string());
        if self.active_scope_locks.get(&key).map(|s| s.as_str()) == Some(owner_id) {
            let record = ExecutionJournalRecord::LockReleased {
                device_address: device_address.to_string(),
                vrf_rd: vrf_rd.to_string(),
                owner_id: owner_id.to_string(),
                released_at_epoch_secs: now_epoch_secs,
            };
            append_journal_record(path, &record).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to persist lock release: {e}"
                ))
            })?;
            self.active_scope_locks.remove(&key);
            self.crash_if_configured(ExecutorCrashPoint::AfterLockReleasedPersisted);
        }
        Ok(())
    }

    /// Acquire an exclusive lock on `(device_address, vrf_rd)` for a change owner.
    pub fn acquire_scope_lock(
        &mut self,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
    ) -> Result<(), ExecutionRejectionReason> {
        self.acquire_scope_lock_at(device_address, vrf_rd, owner_id, 0)
    }

    pub fn acquire_scope_lock_at(
        &mut self,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        let path_opt = self.journal_path.clone();
        if let Some(ref path) = path_opt {
            let _file_lock = FileLockGuard::acquire(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to acquire journal file lock: {e}"
                ))
            })?;
            self.reload_from_journal_path(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload journal records: {e}"
                ))
            })?;
            self.reload_keys_from_disk().map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload executor keys: {e}"
                ))
            })?;
            self.acquire_scope_lock_locked(path, device_address, vrf_rd, owner_id, now_epoch_secs)
        } else {
            let key = (device_address.to_string(), vrf_rd.to_string());
            if let Some(existing_owner) = self.active_scope_locks.get(&key) {
                if existing_owner != owner_id {
                    return Err(ExecutionRejectionReason::ConflictingChangeLockHeld);
                }
                return Ok(());
            }
            self.active_scope_locks.insert(key, owner_id.to_string());
            Ok(())
        }
    }

    pub fn release_scope_lock(
        &mut self,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
    ) -> Result<(), ExecutionRejectionReason> {
        self.release_scope_lock_at(device_address, vrf_rd, owner_id, 0)
    }

    pub fn release_scope_lock_at(
        &mut self,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        let path_opt = self.journal_path.clone();
        if let Some(ref path) = path_opt {
            let _file_lock = FileLockGuard::acquire(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to acquire journal file lock: {e}"
                ))
            })?;
            self.reload_from_journal_path(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload journal records: {e}"
                ))
            })?;
            self.reload_keys_from_disk().map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload executor keys: {e}"
                ))
            })?;
            self.release_scope_lock_locked(path, device_address, vrf_rd, owner_id, now_epoch_secs)
        } else {
            let key = (device_address.to_string(), vrf_rd.to_string());
            if self.active_scope_locks.get(&key).map(|s| s.as_str()) == Some(owner_id) {
                self.active_scope_locks.remove(&key);
            }
            Ok(())
        }
    }

    /// Operator-driven reconciliation for an execution whose intent was
    /// persisted but which never reached a durable completion or abort record
    /// (e.g. process kill). Persists an `ExecutionAborted` record and releases
    /// the scope lock when it is still owned by the interrupted approval, so a
    /// fresh execution with the same idempotency key can proceed.
    ///
    /// The interrupted execution may have applied partial effects before the
    /// crash; the operator MUST verify actual device state before retrying.
    /// Returns the recovered intent record.
    pub fn abort_interrupted_execution(
        &mut self,
        idempotency_key: &str,
        reason: &str,
        now_epoch_secs: u64,
    ) -> Result<ExecutionIntentRecord, ExecutionRejectionReason> {
        let path_opt = self.journal_path.clone();
        let _file_lock = if let Some(ref path) = path_opt {
            let guard = FileLockGuard::acquire(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to acquire journal file lock: {e}"
                ))
            })?;
            self.reload_from_journal_path(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload journal records: {e}"
                ))
            })?;
            self.reload_keys_from_disk().map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload executor keys: {e}"
                ))
            })?;
            Some(guard)
        } else {
            None
        };

        let intent = self
            .in_flight_executions
            .get(idempotency_key)
            .cloned()
            .ok_or_else(|| {
                ExecutionRejectionReason::InterruptedExecutionNotFound(idempotency_key.to_string())
            })?;

        if let Some(ref path) = path_opt {
            let abort_record = ExecutionJournalRecord::ExecutionAborted {
                execution_id: intent.execution_id.clone(),
                idempotency_key: idempotency_key.to_string(),
                reason: reason.to_string(),
                aborted_at_epoch_secs: now_epoch_secs,
            };
            append_journal_record(path, &abort_record).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to persist reconciliation abort: {e}"
                ))
            })?;
            self.crash_if_configured(ExecutorCrashPoint::AfterAbortedPersisted);
            self.in_flight_executions.remove(idempotency_key);
            // Only releases the lock when it is still owned by the interrupted
            // approval; a lock re-acquired by another owner is left untouched.
            self.release_scope_lock_locked(
                path,
                &intent.device_address,
                &intent.vrf_rd,
                &intent.approval_id,
                now_epoch_secs,
            )?;
        } else {
            self.in_flight_executions.remove(idempotency_key);
            self.active_scope_locks
                .remove(&(intent.device_address.clone(), intent.vrf_rd.clone()));
        }

        Ok(intent)
    }

    pub fn journal_entries(&self) -> &[ExecutionJournalEntry] {
        &self.journal
    }

    /// Validates plan binding, scope, expiry, digital-twin report, and cryptographic signature.
    /// Shared identically between fresh execution and replay verification.
    fn validate_authorization_and_approval(
        &self,
        plan: &RemediationPlan,
        assurance: &ChangeAssuranceReport,
        approval: &SignedPlanApproval,
        actual_plan_hash: &str,
        now_epoch_secs: u64,
    ) -> Result<(), ExecutionRejectionReason> {
        // 1. Digital-twin assurance gate
        if !assurance.executable
            || assurance.plan_hash != actual_plan_hash
            || assurance.report_id != approval.assurance_report_id
        {
            return Err(ExecutionRejectionReason::UnverifiedInDigitalTwin);
        }

        // 2. Plan hash binding check (reject mutated plans)
        if approval.approved_plan_hash != actual_plan_hash || approval.plan_id != plan.plan_id {
            return Err(ExecutionRejectionReason::PlanHashMismatch);
        }

        // 3. Scope binding check
        if approval.approved_scope_device != plan.device_address
            || approval.approved_scope_vrf != plan.vrf_rd
        {
            return Err(ExecutionRejectionReason::ScopeMismatch);
        }

        // 4. Expiry check
        if now_epoch_secs > approval.expires_at_epoch_secs
            || now_epoch_secs > plan.expires_at_epoch_secs
        {
            return Err(ExecutionRejectionReason::ApprovalExpired);
        }

        // 5. Signature and enforcement mode checks
        match self.enforcement_mode {
            ExecutionEnforcementMode::Enforced => {
                if self.signing_keys.is_empty() {
                    return Err(ExecutionRejectionReason::MissingSignatureAuthority);
                }
                if approval.signature_hex.is_empty() || approval.signature_algorithm.is_empty() {
                    return Err(ExecutionRejectionReason::InvalidSignature);
                }
                if self.revoked_keys.contains(&approval.key_id) {
                    return Err(ExecutionRejectionReason::RevokedSigningKey(
                        approval.key_id.clone(),
                    ));
                }
                let secret = self
                    .signing_keys
                    .get(&approval.key_id)
                    .or_else(|| self.signing_keys.get(""))
                    .or_else(|| self.signing_keys.get("default"))
                    .ok_or_else(|| {
                        ExecutionRejectionReason::UnknownKeyId(approval.key_id.clone())
                    })?;
                approval.verify_signature(secret)?;

                // In enforced mode, modeled snapshot simulation is strictly rejected:
                if assurance.fidelity == AssuranceFidelity::ModeledSnapshotSimulation {
                    return Err(ExecutionRejectionReason::ModeledAssuranceRejectedInEnforcedMode);
                }

                // In enforced mode, assurance report hash binding must be present and match:
                let computed_report_hash = assurance.compute_report_hash();
                if approval.approved_assurance_report_hash.is_empty() {
                    return Err(ExecutionRejectionReason::MissingAssuranceBinding);
                }
                if approval.approved_assurance_report_hash != computed_report_hash {
                    return Err(ExecutionRejectionReason::AssuranceReportHashMismatch);
                }

                // BN-002 provenance: the assurance report itself must carry a
                // valid signature from a configured, non-revoked key so a
                // fabricated twin result cannot be bound into an approval.
                if assurance.signature_algorithm.is_empty() || assurance.signature_hex.is_empty() {
                    return Err(ExecutionRejectionReason::MissingAssuranceSignature);
                }
                if self.revoked_keys.contains(&assurance.key_id) {
                    return Err(ExecutionRejectionReason::RevokedSigningKey(
                        assurance.key_id.clone(),
                    ));
                }
                let assurance_secret = self
                    .signing_keys
                    .get(&assurance.key_id)
                    .or_else(|| self.signing_keys.get(""))
                    .or_else(|| self.signing_keys.get("default"))
                    .ok_or_else(|| {
                        ExecutionRejectionReason::UnknownKeyId(assurance.key_id.clone())
                    })?;
                assurance.verify_signature(assurance_secret)?;

                // In enforced mode, tenant identity binding must match:
                if approval.tenant_id != plan.tenant_id {
                    return Err(ExecutionRejectionReason::TenantMismatch);
                }
            }
            ExecutionEnforcementMode::Simulation => {
                if !approval.signature_hex.is_empty() && !self.signing_keys.is_empty() {
                    let secret = self
                        .signing_keys
                        .get(&approval.key_id)
                        .or_else(|| self.signing_keys.get(""))
                        .or_else(|| self.signing_keys.get("default"))
                        .ok_or_else(|| {
                            ExecutionRejectionReason::UnknownKeyId(approval.key_id.clone())
                        })?;
                    approval.verify_signature(secret)?;
                }
                // Signed assurance reports are verified when a keyring exists;
                // unsigned reports remain allowed for developer workflows.
                if !assurance.signature_hex.is_empty() && !self.signing_keys.is_empty() {
                    let secret = self
                        .signing_keys
                        .get(&assurance.key_id)
                        .or_else(|| self.signing_keys.get(""))
                        .or_else(|| self.signing_keys.get("default"))
                        .ok_or_else(|| {
                            ExecutionRejectionReason::UnknownKeyId(assurance.key_id.clone())
                        })?;
                    assurance.verify_signature(secret)?;
                }
            }
        }

        Ok(())
    }

    /// Execute an approved `RemediationPlan` after verifying:
    /// 0. Idempotency key deduplication bound to plan, scope, and tenant
    /// 1. In-flight execution check
    /// 2. Cryptographic signature and enforcement checks
    /// 3. Digital-twin assurance verification
    /// 4. Plan hash binding
    /// 5. Baseline configuration revision freshness
    /// 6. Approval & plan expiry
    /// 7. Exact device & VRF scope binding
    /// 8. Exclusive per-device/VRF lock (persisted before effects)
    /// 9. Persist ExecutionIntent BEFORE effects
    /// 10. Prechecks -> step journal -> postchecks -> recovery policy on postcheck failure
    /// 11. Persist completion to journal disk BEFORE updating memory
    #[allow(clippy::too_many_arguments)]
    pub fn execute_approved_plan(
        &mut self,
        idempotency_key: &str,
        plan: &RemediationPlan,
        assurance: &ChangeAssuranceReport,
        approval: &SignedPlanApproval,
        current_device_config_revision: &str,
        now_epoch_secs: u64,
        prechecks_pass: bool,
        postchecks_pass: bool,
    ) -> Result<ExecutionJournalEntry, ExecutionRejectionReason> {
        let actual_plan_hash = plan.compute_plan_hash();

        // Check durable backing requirement in enforced mode:
        if self.enforcement_mode == ExecutionEnforcementMode::Enforced
            && self.journal_path.is_none()
        {
            return Err(ExecutionRejectionReason::DurableBackingRequiredInEnforcedMode);
        }

        // Check non-empty tenant requirement in enforced mode:
        if self.enforcement_mode == ExecutionEnforcementMode::Enforced
            && (approval.tenant_id.trim().is_empty() || plan.tenant_id.trim().is_empty())
        {
            return Err(ExecutionRejectionReason::EmptyTenantId);
        }

        // Acquire journal file lock and reload latest durable state from disk:
        let path_opt = self.journal_path.clone();
        let _file_lock = if let Some(ref path) = path_opt {
            let guard = FileLockGuard::acquire(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to acquire journal file lock: {e}"
                ))
            })?;
            self.reload_from_journal_path(path).map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload journal records: {e}"
                ))
            })?;
            self.reload_keys_from_disk().map_err(|e| {
                ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to reload executor keys: {e}"
                ))
            })?;
            Some(guard)
        } else {
            None
        };

        // 0. Idempotency replay check (strictly bound to plan, scope, and tenant)
        if let Some(existing) = self.completed_by_idempotency_key.get(idempotency_key) {
            if existing.plan_id != plan.plan_id
                || existing.plan_hash != actual_plan_hash
                || existing.device_address != plan.device_address
                || existing.vrf_rd != plan.vrf_rd
                || (!existing.tenant_id.is_empty() && existing.tenant_id != plan.tenant_id)
                || (!existing.tenant_id.is_empty() && existing.tenant_id != approval.tenant_id)
            {
                return Err(ExecutionRejectionReason::IdempotencyConflict(format!(
                    "idempotency key '{idempotency_key}' previously completed for plan '{}' (scope {}|{}), cannot replay for plan '{}' (scope {}|{})",
                    existing.plan_id,
                    existing.device_address,
                    existing.vrf_rd,
                    plan.plan_id,
                    plan.device_address,
                    plan.vrf_rd,
                )));
            }

            if self.enforcement_mode == ExecutionEnforcementMode::Enforced
                && existing.enforcement_mode != ExecutionEnforcementMode::Enforced
            {
                return Err(ExecutionRejectionReason::SimulationReplayRejectedInEnforcedMode);
            }

            // In enforced mode, replaying requires valid signing authority and authorization:
            self.validate_authorization_and_approval(
                plan,
                assurance,
                approval,
                &actual_plan_hash,
                now_epoch_secs,
            )?;

            let mut replay = existing.clone();
            replay.deduplicated_replay = true;
            return Ok(replay);
        }

        // Shared authorization, plan/assurance/approval validation, tenant, expiry, and scope checks:
        self.validate_authorization_and_approval(
            plan,
            assurance,
            approval,
            &actual_plan_hash,
            now_epoch_secs,
        )?;

        // 1. In-flight execution check
        if self.in_flight_executions.contains_key(idempotency_key) {
            return Err(ExecutionRejectionReason::InFlightExecutionInterrupted(
                format!(
                    "idempotency key '{idempotency_key}' is in-flight / interrupted; manual intervention required before retry"
                ),
            ));
        }

        // 2. Stale baseline config revision check
        if current_device_config_revision != approval.expected_baseline_config_revision
            || current_device_config_revision != plan.baseline_config_revision
        {
            return Err(ExecutionRejectionReason::StaleBaselineConfigRevision);
        }

        // 3. Exclusive lock check (persisted before effects)
        if let Some(ref path) = path_opt {
            self.acquire_scope_lock_locked(
                path,
                &plan.device_address,
                &plan.vrf_rd,
                &approval.approval_id,
                now_epoch_secs,
            )?;
        } else {
            let key = (plan.device_address.clone(), plan.vrf_rd.clone());
            if let Some(existing_owner) = self.active_scope_locks.get(&key) {
                if existing_owner != &approval.approval_id {
                    return Err(ExecutionRejectionReason::ConflictingChangeLockHeld);
                }
            } else {
                self.active_scope_locks
                    .insert(key, approval.approval_id.clone());
            }
        }

        // 4. Persist ExecutionIntent BEFORE effects
        let execution_id = format!("exec-{}-{}", plan.plan_id, self.journal.len() + 1);
        if let Some(ref path) = path_opt {
            let intent_record = ExecutionJournalRecord::ExecutionIntent {
                execution_id: execution_id.clone(),
                idempotency_key: idempotency_key.to_string(),
                plan_id: plan.plan_id.clone(),
                plan_hash: actual_plan_hash.clone(),
                tenant_id: plan.tenant_id.clone(),
                approval_id: approval.approval_id.clone(),
                device_address: plan.device_address.clone(),
                vrf_rd: plan.vrf_rd.clone(),
                started_at_epoch_secs: now_epoch_secs,
            };
            if let Err(e) = append_journal_record(path, &intent_record) {
                let _ = self.release_scope_lock_locked(
                    path,
                    &plan.device_address,
                    &plan.vrf_rd,
                    &approval.approval_id,
                    now_epoch_secs,
                );
                return Err(ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to persist execution intent: {e}"
                )));
            }
            self.crash_if_configured(ExecutorCrashPoint::AfterIntentPersisted);
        }
        self.in_flight_executions.insert(
            idempotency_key.to_string(),
            ExecutionIntentRecord {
                execution_id: execution_id.clone(),
                idempotency_key: idempotency_key.to_string(),
                plan_id: plan.plan_id.clone(),
                plan_hash: actual_plan_hash.clone(),
                tenant_id: plan.tenant_id.clone(),
                approval_id: approval.approval_id.clone(),
                device_address: plan.device_address.clone(),
                vrf_rd: plan.vrf_rd.clone(),
                started_at_epoch_secs: now_epoch_secs,
            },
        );

        // 5. Independent prechecks
        if !prechecks_pass {
            if let Some(ref path) = path_opt {
                let abort_record = ExecutionJournalRecord::ExecutionAborted {
                    execution_id: execution_id.clone(),
                    idempotency_key: idempotency_key.to_string(),
                    reason: "read-only prechecks failed".to_string(),
                    aborted_at_epoch_secs: now_epoch_secs,
                };
                if let Err(e) = append_journal_record(path, &abort_record) {
                    return Err(ExecutionRejectionReason::JournalPersistenceError(format!(
                        "failed to persist execution abort: {e}"
                    )));
                }
                self.crash_if_configured(ExecutorCrashPoint::AfterAbortedPersisted);
                self.in_flight_executions.remove(idempotency_key);
                self.release_scope_lock_locked(
                    path,
                    &plan.device_address,
                    &plan.vrf_rd,
                    &approval.approval_id,
                    now_epoch_secs,
                )?;
            } else {
                self.in_flight_executions.remove(idempotency_key);
                self.active_scope_locks
                    .remove(&(plan.device_address.clone(), plan.vrf_rd.clone()));
            }
            return Err(ExecutionRejectionReason::PrecheckFailed(
                "One or more read-only prechecks failed immediately before execution".to_string(),
            ));
        }

        // 6. Apply candidate diff steps and evaluate postchecks
        let steps_applied = vec![format!(
            "apply_candidate_diff:{}:{}",
            plan.device_address, plan.baseline_config_revision
        )];

        let (outcome, recovery_actions_taken) = if postchecks_pass {
            (ExecutionOutcome::CommittedAndPostchecksPassed, Vec::new())
        } else {
            match plan.recovery_policy {
                RecoveryPolicy::ExplicitRollbackSteps => (
                    ExecutionOutcome::RolledBackAfterPostcheckFailure,
                    plan.rollback_steps.clone(),
                ),
                RecoveryPolicy::HaltAndEscalate => (
                    ExecutionOutcome::HaltedForOperatorEscalation,
                    vec![
                        "halt_further_changes_and_page_oncall: reverse diff not assumed safe"
                            .to_string(),
                    ],
                ),
            }
        };

        let entry = ExecutionJournalEntry {
            execution_id,
            idempotency_key: idempotency_key.to_string(),
            plan_id: plan.plan_id.clone(),
            plan_hash: actual_plan_hash,
            tenant_id: plan.tenant_id.clone(),
            approval_id: approval.approval_id.clone(),
            device_address: plan.device_address.clone(),
            vrf_rd: plan.vrf_rd.clone(),
            outcome,
            prechecks_passed: true,
            postchecks_passed: postchecks_pass,
            steps_applied,
            recovery_actions_taken,
            deduplicated_replay: false,
            enforcement_mode: self.enforcement_mode,
        };

        // 7. Persist completion to journal disk BEFORE acknowledging and updating in-memory state
        if let Some(ref path) = path_opt {
            let completion_record = ExecutionJournalRecord::ExecutionCompleted {
                entry: entry.clone(),
            };
            if let Err(e) = append_journal_record(path, &completion_record) {
                return Err(ExecutionRejectionReason::JournalPersistenceError(format!(
                    "failed to append completed entry to journal: {e}"
                )));
            }
            self.crash_if_configured(ExecutorCrashPoint::AfterCompletedPersisted);
        }

        // 8. State committed durably: update in-memory maps and release scope lock
        self.in_flight_executions.remove(idempotency_key);
        self.completed_by_idempotency_key
            .insert(idempotency_key.to_string(), entry.clone());
        self.journal.push(entry.clone());

        if let Some(ref path) = path_opt {
            self.release_scope_lock_locked(
                path,
                &plan.device_address,
                &plan.vrf_rd,
                &approval.approval_id,
                now_epoch_secs,
            )?;
        } else {
            self.active_scope_locks
                .remove(&(plan.device_address.clone(), plan.vrf_rd.clone()));
        }

        Ok(entry)
    }
}

// ── A19: Shadow Operational Pilot Tracker ─────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowPilotObservation {
    pub episode_id: String,
    pub prefix: String,
    pub vrf_rd: String,
    pub investigator_status: String,
    pub operator_utility_verdict: String, // "useful" | "not_useful"
    pub failure_taxonomy_tag: Option<String>,
    pub write_operations_attempted: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowPilotSummary {
    pub total_episodes: usize,
    pub useful_episodes: usize,
    pub operator_utility_rate_pct: f64,
    pub total_write_operations_attempted: usize,
    pub failure_taxonomy_counts: BTreeMap<String, usize>,
    pub prototype_status_verified: bool,
    pub emulation_boundary_respected: bool,
}

pub fn summarize_shadow_pilot(episodes: &[ShadowPilotObservation]) -> ShadowPilotSummary {
    let total_episodes = episodes.len();
    let useful_episodes = episodes
        .iter()
        .filter(|e| e.operator_utility_verdict == "useful")
        .count();
    let total_write_operations_attempted: usize =
        episodes.iter().map(|e| e.write_operations_attempted).sum();
    let mut failure_taxonomy_counts = BTreeMap::new();
    for ep in episodes {
        if let Some(ref tag) = ep.failure_taxonomy_tag {
            *failure_taxonomy_counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let operator_utility_rate_pct = if total_episodes == 0 {
        0.0
    } else {
        (useful_episodes as f64 / total_episodes as f64 * 100.0 * 10.0).round() / 10.0
    };
    ShadowPilotSummary {
        total_episodes,
        useful_episodes,
        operator_utility_rate_pct,
        total_write_operations_attempted,
        failure_taxonomy_counts,
        prototype_status_verified: true,
        emulation_boundary_respected: total_write_operations_attempted == 0,
    }
}

#[derive(Debug, Clone)]
pub struct ShadowPilotAuditJournal {
    pub path: PathBuf,
}

impl ShadowPilotAuditJournal {
    pub fn open_or_create(path: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self { path })
    }

    pub fn record_observation(
        &self,
        observation: &ShadowPilotObservation,
    ) -> Result<(), std::io::Error> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(observation)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        use std::io::Write;
        writeln!(file, "{}", line)?;
        file.sync_data()?;
        Ok(())
    }

    pub fn load_observations(&self) -> Result<Vec<ShadowPilotObservation>, std::io::Error> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        use std::io::BufRead;
        let mut observations = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let obs: ShadowPilotObservation = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            observations.push(obs);
        }
        Ok(observations)
    }

    pub fn summarize(&self) -> Result<ShadowPilotSummary, std::io::Error> {
        let obs = self.load_observations()?;
        Ok(summarize_shadow_pilot(&obs))
    }
}

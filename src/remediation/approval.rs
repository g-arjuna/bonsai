//! Slice P3 (A13, A16–A19):
//! - A13: Deterministic second-NOS adapter workshop pilot (`nokia_srl`, `cisco_xrd`) with parser-version lineage & schema drift detection
//! - A16: Typed `RemediationPlan` & network invariant schema with deterministic SHA-256 `plan_hash`
//! - A17: Digital-twin / Containerlab change assurance gate with independent pre/postchecks and explicit emulation limitations
//! - A18: Approval-bound executor enforcing `plan_hash`, baseline config revision, expiry, device/VRF lock, idempotency, and recovery policy
//! - A19: Read-only shadow operational pilot tracker with zero write authorization

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}

/// Deterministic Nokia SR Linux (`nokia_srl`) BGP neighbor state parser committed from the adapter workshop (A13).
/// Diagnoses NOS schema changes (`unparsed_fields` / `SchemaMismatch`) separately from network failure.
pub fn parse_nokia_srl_bgp_neighbor_json(
    raw_json: &str,
) -> Result<NormalizedNosBgpNeighborObservation, String> {
    let raw_hash = hex::encode(Sha256::digest(raw_json.as_bytes()));
    let val: serde_json::Value =
        serde_json::from_str(raw_json).map_err(|e| format!("invalid JSON: {e}"))?;

    let obj = val
        .as_object()
        .ok_or_else(|| "expected top-level JSON object".to_string())?;

    let Some(network_instance) = obj.get("network-instance").and_then(|v| v.as_str()) else {
        return Ok(NormalizedNosBgpNeighborObservation {
            nos_vendor: "nokia_srl".to_string(),
            nos_version: obj
                .get("software-version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            parser_id: "nokia-srl-bgp-neighbor".to_string(),
            parser_version: "1.0.0".to_string(),
            raw_sample_sha256: raw_hash,
            parse_quality: ParseQualityState::SchemaMismatch,
            vrf_name: "unknown".to_string(),
            neighbor_address: "unknown".to_string(),
            peer_as: 0,
            session_state: "unknown".to_string(),
            received_routes: 0,
            active_routes: 0,
            import_policy: None,
            export_policy: None,
            unparsed_fields: obj.keys().cloned().collect(),
        });
    };

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

    let quality = if unparsed.is_empty() {
        ParseQualityState::Complete
    } else {
        ParseQualityState::PartialUnknownFields
    };

    Ok(NormalizedNosBgpNeighborObservation {
        nos_vendor: "nokia_srl".to_string(),
        nos_version: obj
            .get("software-version")
            .and_then(|v| v.as_str())
            .unwrap_or("24.7.1")
            .to_string(),
        parser_id: "nokia-srl-bgp-neighbor".to_string(),
        parser_version: "1.0.0".to_string(),
        raw_sample_sha256: raw_hash,
        parse_quality: quality,
        vrf_name: network_instance.to_string(),
        neighbor_address: obj
            .get("peer-address")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        peer_as: obj.get("peer-as").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        session_state: obj
            .get("session-state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        received_routes: obj
            .get("received-routes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        active_routes: obj
            .get("active-routes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        import_policy: obj
            .get("import-policy")
            .and_then(|v| v.as_str())
            .map(String::from),
        export_policy: obj
            .get("export-policy")
            .and_then(|v| v.as_str())
            .map(String::from),
        unparsed_fields: unparsed,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigitalTwinSnapshot {
    pub lab_topology_id: String,
    pub nos_image_ref: String,
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
    pub verdict: AssuranceVerdict,
    pub executable: bool,
    pub invariant_results: Vec<InvariantCheckResult>,
    pub unsupported_constructs: Vec<String>,
    pub emulation_fidelity_limitations: Vec<String>,
}

/// Evaluate a `RemediationPlan` against a `DigitalTwinSnapshot` (Batfish / Containerlab gate, A17).
/// No unverified plan can be marked `executable = true`.
pub fn verify_plan_in_digital_twin(
    plan: &RemediationPlan,
    twin: &DigitalTwinSnapshot,
) -> ChangeAssuranceReport {
    let plan_hash = plan.compute_plan_hash();
    let limitations = vec![
        "Containerlab emulation and modeled config analysis do not prove ASIC FIB programming, optical layer margins, or microburst queueing behavior.".to_string(),
    ];

    if let Err(schema_err) = plan.validate_schema() {
        return ChangeAssuranceReport {
            report_id: format!("assurance-{}", plan.plan_id),
            plan_id: plan.plan_id.clone(),
            plan_hash,
            lab_topology_id: twin.lab_topology_id.clone(),
            verdict: AssuranceVerdict::InvalidPlanSchema,
            executable: false,
            invariant_results: vec![InvariantCheckResult {
                invariant_id: "plan_schema_valid".to_string(),
                passed: false,
                detail: schema_err,
            }],
            unsupported_constructs: twin.unsupported_vendor_constructs.clone(),
            emulation_fidelity_limitations: limitations,
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
        executable: all_passed,
        verdict,
        invariant_results: results,
        unsupported_constructs: twin.unsupported_vendor_constructs.clone(),
        emulation_fidelity_limitations: limitations,
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
    pub approved_by: String,
    pub operator_rationale: String,
    pub approved_at_epoch_secs: u64,
    pub expires_at_epoch_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRejectionReason {
    UnverifiedInDigitalTwin,
    PlanHashMismatch,
    StaleBaselineConfigRevision,
    ApprovalExpired,
    ScopeMismatch,
    ConflictingChangeLockHeld,
    PrecheckFailed(String),
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
    pub approval_id: String,
    pub device_address: String,
    pub vrf_rd: String,
    pub outcome: ExecutionOutcome,
    pub prechecks_passed: bool,
    pub postchecks_passed: bool,
    pub steps_applied: Vec<String>,
    pub recovery_actions_taken: Vec<String>,
    pub deduplicated_replay: bool,
}

#[derive(Debug, Default)]
pub struct ApprovalBoundExecutor {
    active_scope_locks: HashMap<(String, String), String>,
    completed_by_idempotency_key: BTreeMap<String, ExecutionJournalEntry>,
    journal: Vec<ExecutionJournalEntry>,
}

impl ApprovalBoundExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire an exclusive lock on `(device_address, vrf_rd)` for a change owner.
    pub fn acquire_scope_lock(
        &mut self,
        device_address: &str,
        vrf_rd: &str,
        owner_id: &str,
    ) -> Result<(), ExecutionRejectionReason> {
        let key = (device_address.to_string(), vrf_rd.to_string());
        if let Some(existing_owner) = self.active_scope_locks.get(&key) {
            if existing_owner != owner_id {
                return Err(ExecutionRejectionReason::ConflictingChangeLockHeld);
            }
        }
        self.active_scope_locks.insert(key, owner_id.to_string());
        Ok(())
    }

    pub fn release_scope_lock(&mut self, device_address: &str, vrf_rd: &str, owner_id: &str) {
        let key = (device_address.to_string(), vrf_rd.to_string());
        if self.active_scope_locks.get(&key).map(|s| s.as_str()) == Some(owner_id) {
            self.active_scope_locks.remove(&key);
        }
    }

    pub fn journal_entries(&self) -> &[ExecutionJournalEntry] {
        &self.journal
    }

    /// Execute an approved `RemediationPlan` after verifying:
    /// 1. Idempotency key deduplication
    /// 2. Digital-twin assurance verification (`assurance.executable == true` and matching `plan_hash`)
    /// 3. Plan hash binding (`approval.approved_plan_hash == plan.compute_plan_hash()`)
    /// 4. Baseline configuration revision freshness (`current_device_config_revision == approval.expected_baseline_config_revision`)
    /// 5. Approval & plan expiry (`now_epoch_secs <= expires_at_epoch_secs`)
    /// 6. Exact device & VRF scope binding
    /// 7. Exclusive per-device/VRF lock
    /// 8. Prechecks -> step journal -> postchecks -> recovery policy on postcheck failure
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
        // 1. Idempotency check
        if let Some(existing) = self.completed_by_idempotency_key.get(idempotency_key) {
            let mut replay = existing.clone();
            replay.deduplicated_replay = true;
            return Ok(replay);
        }

        let actual_plan_hash = plan.compute_plan_hash();

        // 2. Digital-twin assurance gate
        if !assurance.executable
            || assurance.plan_hash != actual_plan_hash
            || assurance.report_id != approval.assurance_report_id
        {
            return Err(ExecutionRejectionReason::UnverifiedInDigitalTwin);
        }

        // 3. Plan hash binding check (reject mutated plans)
        if approval.approved_plan_hash != actual_plan_hash || approval.plan_id != plan.plan_id {
            return Err(ExecutionRejectionReason::PlanHashMismatch);
        }

        // 4. Stale baseline config revision check
        if current_device_config_revision != approval.expected_baseline_config_revision
            || current_device_config_revision != plan.baseline_config_revision
        {
            return Err(ExecutionRejectionReason::StaleBaselineConfigRevision);
        }

        // 5. Expiry check
        if now_epoch_secs > approval.expires_at_epoch_secs
            || now_epoch_secs > plan.expires_at_epoch_secs
        {
            return Err(ExecutionRejectionReason::ApprovalExpired);
        }

        // 6. Scope binding check
        if approval.approved_scope_device != plan.device_address
            || approval.approved_scope_vrf != plan.vrf_rd
        {
            return Err(ExecutionRejectionReason::ScopeMismatch);
        }

        // 7. Exclusive lock check
        self.acquire_scope_lock(&plan.device_address, &plan.vrf_rd, &approval.approval_id)?;

        // 8. Independent prechecks
        if !prechecks_pass {
            self.release_scope_lock(&plan.device_address, &plan.vrf_rd, &approval.approval_id);
            return Err(ExecutionRejectionReason::PrecheckFailed(
                "One or more read-only prechecks failed immediately before execution".to_string(),
            ));
        }

        // Apply candidate diff steps and evaluate postchecks
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

        self.release_scope_lock(&plan.device_address, &plan.vrf_rd, &approval.approval_id);

        let entry = ExecutionJournalEntry {
            execution_id: format!("exec-{}-{}", plan.plan_id, self.journal.len() + 1),
            idempotency_key: idempotency_key.to_string(),
            plan_id: plan.plan_id.clone(),
            plan_hash: actual_plan_hash,
            approval_id: approval.approval_id.clone(),
            device_address: plan.device_address.clone(),
            vrf_rd: plan.vrf_rd.clone(),
            outcome,
            prechecks_passed: true,
            postchecks_passed: postchecks_pass,
            steps_applied,
            recovery_actions_taken,
            deduplicated_replay: false,
        };

        self.completed_by_idempotency_key
            .insert(idempotency_key.to_string(), entry.clone());
        self.journal.push(entry.clone());
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
}

pub fn summarize_shadow_pilot(episodes: &[ShadowPilotObservation]) -> ShadowPilotSummary {
    let total_episodes = episodes.len();
    let useful_episodes = episodes
        .iter()
        .filter(|e| e.operator_utility_verdict == "useful")
        .count();
    let total_write_operations_attempted =
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
    }
}

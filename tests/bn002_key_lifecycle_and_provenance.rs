//! BN-002: operational signing-key lifecycle and authenticated assurance
//! provenance for the approval-bound executor.
//!
//! Covers: durable key registration/rotation/revocation surviving restart,
//! cross-instance keyring visibility, fail-closed key-store corruption, and
//! signed assurance reports required in enforced mode.

use std::path::{Path, PathBuf};

use bonsai::remediation::{
    ApprovalBoundExecutor, AssuranceCheckSpec, AssuranceFidelity, BlastRadiusSummary,
    ChangeAssuranceReport, DigitalTwinSnapshot, DisruptionLevel, ExecutionEnforcementMode,
    ExecutionOutcome, ExecutionRejectionReason, NetworkInvariant, RecoveryPolicy, RemediationPlan,
    SignedPlanApproval, verify_plan_in_digital_twin,
};

const DEVICE: &str = "192.0.2.10";
const VRF: &str = "0:0";
const REV: &str = "rev-2026-10-12-001";
const NOW: u64 = 1_760_000_200;
const KEY_SECRET: &[u8] = b"bn002-test-signing-secret-key-32bytes!";

fn sample_plan() -> RemediationPlan {
    RemediationPlan {
        plan_id: "plan-bn002-keys".to_string(),
        tenant_id: "default".to_string(),
        device_address: DEVICE.to_string(),
        vrf_rd: VRF.to_string(),
        nos_vendor: "frr".to_string(),
        baseline_config_revision: REV.to_string(),
        candidate_diff: "- route-map RM-IN deny 10\n+ route-map RM-IN permit 10".to_string(),
        intended_behavior: "Permit 203.0.113.0/24 into LocRib".to_string(),
        blast_radius: BlastRadiusSummary {
            affected_devices: vec![DEVICE.to_string()],
            affected_vrfs: vec![VRF.to_string()],
            affected_ebgp_peers: vec![],
            estimated_affected_prefixes: 1,
            expected_disruption: DisruptionLevel::ControlPlaneReconvergence,
        },
        invariants: vec![NetworkInvariant {
            invariant_id: "prefix_reachable_in_loc_rib".to_string(),
            description: "prefix present in LocRib".to_string(),
            invariant_type: "loc_rib_presence".to_string(),
            target_scope: format!("{DEVICE}|{VRF}|203.0.113.0/24"),
        }],
        prechecks: vec![AssuranceCheckSpec {
            check_id: "pre-1".to_string(),
            description: "precheck".to_string(),
            command_or_query: "read-only query".to_string(),
            expected_assertion: "ok".to_string(),
            read_only: true,
        }],
        postchecks: vec![AssuranceCheckSpec {
            check_id: "post-1".to_string(),
            description: "postcheck".to_string(),
            command_or_query: "read-only query".to_string(),
            expected_assertion: "ok".to_string(),
            read_only: true,
        }],
        recovery_policy: RecoveryPolicy::ExplicitRollbackSteps,
        rollback_steps: vec!["restore previous route-map".to_string()],
        created_at_epoch_secs: 1_760_000_000,
        expires_at_epoch_secs: 1_760_003_600,
    }
}

fn sample_assurance(plan: &RemediationPlan) -> ChangeAssuranceReport {
    let twin = DigitalTwinSnapshot {
        lab_topology_id: "clab-bn002-twin".to_string(),
        nos_image_ref: "quay.io/frrouting/frr:9.1.0".to_string(),
        fidelity: AssuranceFidelity::LiveContainerlabVerified,
        target_prefix_reachable_before: false,
        target_prefix_reachable_after: true,
        ebgp_peers_up_before: 1,
        ebgp_peers_up_after: 1,
        vrf_isolation_intact: true,
        max_allowed_blast_radius_devices: 2,
        unsupported_vendor_constructs: vec![],
    };
    verify_plan_in_digital_twin(plan, &twin)
}

fn signed_approval(
    plan: &RemediationPlan,
    assurance: &ChangeAssuranceReport,
    approval_id: &str,
    key_id: &str,
) -> SignedPlanApproval {
    let mut approval = SignedPlanApproval {
        approval_id: approval_id.to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        approved_assurance_report_hash: assurance.compute_report_hash(),
        tenant_id: plan.tenant_id.clone(),
        approved_by: "netops-lead".to_string(),
        operator_rationale: "bn002 verification".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    };
    approval.sign_hmac_sha256(key_id, KEY_SECRET);
    approval
}

fn journal_and_keys_paths(dir: &Path) -> (PathBuf, PathBuf) {
    let journal = dir.join("executor_journal.jsonl");
    let keys = journal.with_extension("keys.json");
    (journal, keys)
}

fn enforced_executor(path: &Path) -> ApprovalBoundExecutor {
    ApprovalBoundExecutor::open_or_create(Some(path))
        .expect("open executor")
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
}

#[test]
fn bn002_registered_keys_and_revocations_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, keys) = journal_and_keys_paths(dir.path());

    // Register a key through the durable API, then drop the instance.
    {
        let mut executor = enforced_executor(&journal);
        executor
            .register_signing_key("key-rotate-1", KEY_SECRET, 1_760_000_100)
            .expect("register key");
    }
    assert!(
        keys.exists(),
        "key store must be persisted beside the journal"
    );

    // A fresh instance observes the registered key and executes enforced plans.
    let plan = sample_plan();
    let mut assurance = sample_assurance(&plan);
    assurance.sign_hmac_sha256("key-rotate-1", KEY_SECRET);
    let approval = signed_approval(&plan, &assurance, "appr-k1", "key-rotate-1");

    let mut executor = enforced_executor(&journal);
    let entry = executor
        .execute_approved_plan(
            "idem-k1", &plan, &assurance, &approval, REV, NOW, true, true,
        )
        .expect("enforced execution with restarted keyring must succeed");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );

    // Revoke through a second instance; a third instance must still see it.
    {
        let mut revoker = enforced_executor(&journal);
        revoker
            .revoke_signing_key("key-rotate-1", 1_760_000_400)
            .expect("revoke key");
    }
    let mut restarted = enforced_executor(&journal);
    // Replay of idem-k1 dedups before auth; use a fresh idempotency key for
    // the revoked-key check.
    let approval2 = signed_approval(&plan, &assurance, "appr-k2", "key-rotate-1");
    let err = restarted.execute_approved_plan(
        "idem-k2",
        &plan,
        &assurance,
        &approval2,
        REV,
        1_760_000_500,
        true,
        true,
    );
    assert_eq!(
        err,
        Err(ExecutionRejectionReason::RevokedSigningKey(
            "key-rotate-1".to_string()
        ))
    );
}

#[test]
fn bn002_key_rotation_overlap_and_reactivation() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, _keys) = journal_and_keys_paths(dir.path());

    let mut executor = enforced_executor(&journal);
    executor
        .register_signing_key("key-old", KEY_SECRET, 1_760_000_100)
        .unwrap();
    // Rotation: register the new key while the old stays valid (overlap).
    executor
        .register_signing_key("key-new", KEY_SECRET, 1_760_000_200)
        .unwrap();

    let plan = sample_plan();
    let mut assurance = sample_assurance(&plan);
    assurance.sign_hmac_sha256("key-new", KEY_SECRET);

    // Old-key approval still verifies during the overlap window.
    let old_approval = signed_approval(&plan, &assurance, "appr-old", "key-old");
    let entry = executor
        .execute_approved_plan(
            "idem-old",
            &plan,
            &assurance,
            &old_approval,
            REV,
            NOW,
            true,
            true,
        )
        .expect("old key must remain valid during rotation overlap");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );

    // New-key approval also verifies.
    let new_approval = signed_approval(&plan, &assurance, "appr-new", "key-new");
    executor
        .execute_approved_plan(
            "idem-new",
            &plan,
            &assurance,
            &new_approval,
            REV,
            NOW,
            true,
            true,
        )
        .expect("new key must verify during overlap");

    // Close the overlap: revoke the old key durably.
    executor
        .revoke_signing_key("key-old", 1_760_000_300)
        .unwrap();
    let plan2 = {
        let mut p = sample_plan();
        p.plan_id = "plan-bn002-keys-2".to_string();
        p
    };
    let mut assurance2 = sample_assurance(&plan2);
    assurance2.sign_hmac_sha256("key-new", KEY_SECRET);
    let old_approval2 = signed_approval(&plan2, &assurance2, "appr-old-2", "key-old");
    let err = executor.execute_approved_plan(
        "idem-old-2",
        &plan2,
        &assurance2,
        &old_approval2,
        REV,
        1_760_000_400,
        true,
        true,
    );
    assert_eq!(
        err,
        Err(ExecutionRejectionReason::RevokedSigningKey(
            "key-old".to_string()
        ))
    );

    // Restart: revocation and the surviving new key both persist.
    drop(executor);
    let mut restarted = enforced_executor(&journal);
    let new_approval2 = signed_approval(&plan2, &assurance2, "appr-new-2", "key-new");
    restarted
        .execute_approved_plan(
            "idem-new-2",
            &plan2,
            &assurance2,
            &new_approval2,
            REV,
            1_760_000_500,
            true,
            true,
        )
        .expect("new key must survive restart after old key revoked");
}

#[test]
fn bn002_corrupt_or_vanished_key_store_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, keys) = journal_and_keys_paths(dir.path());

    {
        let mut executor = enforced_executor(&journal);
        executor
            .register_signing_key("key-x", KEY_SECRET, 1_760_000_100)
            .unwrap();
    }
    assert!(keys.exists());

    // Corrupt the key store: open must fail closed.
    std::fs::write(&keys, b"{not-json").unwrap();
    assert!(
        ApprovalBoundExecutor::open_or_create(Some(&journal)).is_err(),
        "corrupt key store must fail closed"
    );

    // Restore a valid store, then delete it while the registering instance is
    // still alive: its next operation must fail closed rather than silently
    // continuing with an in-memory keyring that no longer matches disk.
    std::fs::remove_file(&keys).unwrap();
    let mut executor = enforced_executor(&journal);
    executor
        .register_signing_key("key-x", KEY_SECRET, 1_760_000_100)
        .unwrap();
    std::fs::remove_file(&keys).unwrap();
    let plan = sample_plan();
    let mut assurance = sample_assurance(&plan);
    assurance.sign_hmac_sha256("key-x", KEY_SECRET);
    let approval = signed_approval(&plan, &assurance, "appr-x", "key-x");
    let err = executor
        .execute_approved_plan("idem-x", &plan, &assurance, &approval, REV, NOW, true, true);
    assert!(
        matches!(
            err,
            Err(ExecutionRejectionReason::JournalPersistenceError(_))
        ),
        "vanished key store must surface as JournalPersistenceError, got {err:?}"
    );
}

#[test]
fn bn002_enforced_mode_requires_signed_assurance_report() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, _keys) = journal_and_keys_paths(dir.path());

    let plan = sample_plan();
    let mut executor = enforced_executor(&journal);
    executor
        .register_signing_key("key-a", KEY_SECRET, 1_760_000_100)
        .unwrap();

    // 1. Unsigned assurance report rejected in enforced mode.
    let unsigned_assurance = sample_assurance(&plan);
    let approval = signed_approval(&plan, &unsigned_assurance, "appr-u", "key-a");
    let err = executor.execute_approved_plan(
        "idem-unsigned",
        &plan,
        &unsigned_assurance,
        &approval,
        REV,
        NOW,
        true,
        true,
    );
    assert_eq!(
        err,
        Err(ExecutionRejectionReason::MissingAssuranceSignature)
    );

    // 2. Report signed by an unknown key rejected.
    let mut unknown_signed = sample_assurance(&plan);
    unknown_signed.sign_hmac_sha256("key-unknown", KEY_SECRET);
    let approval_u = signed_approval(&plan, &unknown_signed, "appr-uu", "key-a");
    let err = executor.execute_approved_plan(
        "idem-unknown",
        &plan,
        &unknown_signed,
        &approval_u,
        REV,
        NOW,
        true,
        true,
    );
    assert_eq!(
        err,
        Err(ExecutionRejectionReason::UnknownKeyId(
            "key-unknown".to_string()
        ))
    );

    // 3. Tampered report content invalidates the signature. lab_topology_id is
    // not part of the assurance binding gate but IS part of canonical signing
    // bytes, so a valid-looking executable report with altered content must
    // fail signature verification.
    let mut tampered = sample_assurance(&plan);
    tampered.sign_hmac_sha256("key-a", KEY_SECRET);
    tampered.lab_topology_id = "clab-tampered".to_string();
    let approval_t = signed_approval(&plan, &tampered, "appr-t", "key-a");
    let err = executor.execute_approved_plan(
        "idem-tampered",
        &plan,
        &tampered,
        &approval_t,
        REV,
        NOW,
        true,
        true,
    );
    assert_eq!(err, Err(ExecutionRejectionReason::InvalidSignature));

    // 4. Properly signed report executes in enforced mode.
    let mut good = sample_assurance(&plan);
    good.sign_hmac_sha256("key-a", KEY_SECRET);
    let approval_g = signed_approval(&plan, &good, "appr-g", "key-a");
    let entry = executor
        .execute_approved_plan("idem-good", &plan, &good, &approval_g, REV, NOW, true, true)
        .expect("signed assurance report must satisfy provenance in enforced mode");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
}

#[test]
fn bn002_revoked_assurance_signing_key_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, _keys) = journal_and_keys_paths(dir.path());

    let plan = sample_plan();
    let mut executor = enforced_executor(&journal);
    executor
        .register_signing_key("key-r", KEY_SECRET, 1_760_000_100)
        .unwrap();
    executor.revoke_signing_key("key-r", 1_760_000_200).unwrap();

    let mut assurance = sample_assurance(&plan);
    assurance.sign_hmac_sha256("key-r", KEY_SECRET);
    let approval = signed_approval(&plan, &assurance, "appr-r", "key-r");
    let err = executor
        .execute_approved_plan("idem-r", &plan, &assurance, &approval, REV, NOW, true, true);
    // The approval key itself is revoked, so approval verification fires first.
    assert_eq!(
        err,
        Err(ExecutionRejectionReason::RevokedSigningKey(
            "key-r".to_string()
        ))
    );
}

#[test]
fn bn002_unsigned_assurance_still_allowed_in_simulation_mode() {
    let mut executor = ApprovalBoundExecutor::new();
    let plan = sample_plan();
    let assurance = sample_assurance(&plan); // unsigned
    let mut approval = SignedPlanApproval {
        approval_id: "appr-sim".to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        approved_assurance_report_hash: assurance.compute_report_hash(),
        tenant_id: plan.tenant_id.clone(),
        approved_by: "dev".to_string(),
        operator_rationale: "simulation".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    };
    approval.sign_hmac_sha256("key-sim", KEY_SECRET);
    executor = executor.with_signing_key("key-sim", KEY_SECRET);

    let entry = executor
        .execute_approved_plan(
            "idem-sim", &plan, &assurance, &approval, REV, NOW, true, true,
        )
        .expect("simulation mode permits unsigned assurance reports");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
}

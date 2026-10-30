use bonsai::remediation::{
    ApprovalBoundExecutor, AssuranceCheckSpec, AssuranceFidelity, BlastRadiusSummary,
    DigitalTwinSnapshot, DisruptionLevel, NetworkInvariant, RecoveryPolicy, RemediationPlan,
    SignedPlanApproval, verify_plan_in_digital_twin,
};
fn sample_remediation_plan() -> RemediationPlan {
    RemediationPlan {
        plan_id: "plan-fix-rm-deny-203-0-113".to_string(),
        tenant_id: "default".to_string(),
        device_address: "192.0.2.10".to_string(),
        vrf_rd: "0:0".to_string(),
        nos_vendor: "frr".to_string(),
        baseline_config_revision: "rev-2026-10-12-001".to_string(),
        candidate_diff: "- route-map RM-IN deny 10\n+ route-map RM-IN permit 10".to_string(),
        intended_behavior: "Permit 203.0.113.0/24 in AdjRibInPostPolicy and install into LocRib"
            .to_string(),
        blast_radius: BlastRadiusSummary {
            affected_devices: vec!["192.0.2.10".to_string()],
            affected_vrfs: vec!["0:0".to_string()],
            affected_ebgp_peers: vec!["198.51.100.1".to_string()],
            estimated_affected_prefixes: 1,
            expected_disruption: DisruptionLevel::ControlPlaneReconvergence,
        },
        invariants: vec![
            NetworkInvariant {
                invariant_id: "prefix_reachable_in_loc_rib".to_string(),
                description: "203.0.113.0/24 must be present in LocRib after policy change"
                    .to_string(),
                invariant_type: "loc_rib_presence".to_string(),
                target_scope: "192.0.2.10|0:0|203.0.113.0/24".to_string(),
            },
            NetworkInvariant {
                invariant_id: "no_ebgp_peer_drop".to_string(),
                description: "All existing eBGP sessions remain Established".to_string(),
                invariant_type: "peer_session_stability".to_string(),
                target_scope: "192.0.2.10".to_string(),
            },
        ],
        prechecks: vec![AssuranceCheckSpec {
            check_id: "pre-1".to_string(),
            description: "Verify 203.0.113.0/24 is present in AdjRibInPrePolicy".to_string(),
            command_or_query: "get_prefix_journey(prefix=203.0.113.0/24, view=AdjRibInPrePolicy)"
                .to_string(),
            expected_assertion: "pre_policy_route_count >= 1".to_string(),
            read_only: true,
        }],
        postchecks: vec![AssuranceCheckSpec {
            check_id: "post-1".to_string(),
            description: "Verify 203.0.113.0/24 is installed in LocRib".to_string(),
            command_or_query: "get_prefix_journey(prefix=203.0.113.0/24, view=LocRib)".to_string(),
            expected_assertion: "loc_rib_route_count >= 1".to_string(),
            read_only: true,
        }],
        recovery_policy: RecoveryPolicy::ExplicitRollbackSteps,
        rollback_steps: vec![
            "configure terminal -> route-map RM-IN deny 10 -> bgp soft-reconfiguration inbound"
                .to_string(),
        ],
        created_at_epoch_secs: 1_760_000_000,
        expires_at_epoch_secs: 1_760_003_600,
    }
}

fn sample_healthy_twin() -> DigitalTwinSnapshot {
    DigitalTwinSnapshot {
        lab_topology_id: "clab-bonsai-frr-srl-twin".to_string(),
        nos_image_ref: "quay.io/frrouting/frr:9.1.0".to_string(),
        fidelity: AssuranceFidelity::LiveContainerlabVerified,
        target_prefix_reachable_before: false,
        target_prefix_reachable_after: true,
        ebgp_peers_up_before: 2,
        ebgp_peers_up_after: 2,
        vrf_isolation_intact: true,
        max_allowed_blast_radius_devices: 2,
        unsupported_vendor_constructs: vec![],
    }
}

fn sample_probe_approval(plan: &RemediationPlan) -> SignedPlanApproval {
    SignedPlanApproval {
        approval_id: "probe-approval-001".to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: format!("assurance-{}", plan.plan_id),
        approved_assurance_report_hash: String::new(),
        tenant_id: plan.tenant_id.clone(),
        approved_by: "probe-reviewer".to_string(),
        operator_rationale: "Automated regression verification".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    }
}

#[test]
fn corruption_added_after_completion_is_ignored_during_replay() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let plan = sample_remediation_plan();
    let assurance = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let approval = sample_probe_approval(&plan);
    executor
        .execute_approved_plan(
            "same",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{{broken-json}}").unwrap();
    file.sync_all().unwrap();
    assert!(ApprovalBoundExecutor::open_or_create(Some(&path)).is_err());
    assert!(
        executor
            .execute_approved_plan(
                "same",
                &plan,
                &assurance,
                &approval,
                "rev-2026-10-12-001",
                1_760_000_200,
                true,
                true
            )
            .unwrap()
            .deduplicated_replay
    );
}
#[test]
fn report_identity_delimiters_create_identical_hashes() {
    let plan = sample_remediation_plan();
    let mut a = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let mut b = a.clone();
    a.report_id = "a:b".into();
    a.plan_id = "c".into();
    b.report_id = "a".into();
    b.plan_id = "b:c".into();
    assert_ne!(a.report_id, b.report_id);
    assert_eq!(a.compute_report_hash(), b.compute_report_hash());
}
#[test]
fn fidelity_limitations_are_not_bound_by_report_hash() {
    let plan = sample_remediation_plan();
    let a = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let mut b = a.clone();
    b.emulation_fidelity_limitations = vec!["Independent lab evidence unavailable".into()];
    assert_ne!(
        a.emulation_fidelity_limitations,
        b.emulation_fidelity_limitations
    );
    assert_eq!(a.compute_report_hash(), b.compute_report_hash());
}
#[test]
fn empty_durable_journal_still_replays_from_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let plan = sample_remediation_plan();
    let assurance = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let approval = sample_probe_approval(&plan);
    executor
        .execute_approved_plan(
            "same",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap();
    std::fs::write(&path, b"").unwrap();
    assert!(
        ApprovalBoundExecutor::open_or_create(Some(&path))
            .unwrap()
            .journal_entries()
            .is_empty()
    );
    assert!(
        executor
            .execute_approved_plan(
                "same",
                &plan,
                &assurance,
                &approval,
                "rev-2026-10-12-001",
                1_760_000_200,
                true,
                true
            )
            .unwrap()
            .deduplicated_replay
    );
}

#[test]
fn process_scope_worker() {
    let Ok(path) = std::env::var("BONSAI_AUDIT_CHILD_JOURNAL") else {
        return;
    };
    let path = std::path::Path::new(&path);
    let mut child = ApprovalBoundExecutor::open_or_create(Some(path)).unwrap();
    std::fs::write(path.with_extension("ready"), b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.with_extension("go").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(matches!(
        child.acquire_scope_lock("device", "vrf", "child"),
        Err(bonsai::remediation::ExecutionRejectionReason::ConflictingChangeLockHeld)
    ));
}

#[test]
fn second_process_with_stale_snapshot_observes_new_scope_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut parent = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_scope_worker", "--nocapture"])
        .env("BONSAI_AUDIT_CHILD_JOURNAL", &path)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.with_extension("ready").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    parent
        .acquire_scope_lock("device", "vrf", "parent")
        .unwrap();
    std::fs::write(path.with_extension("go"), b"go").unwrap();
    assert!(child.wait().unwrap().success());
}

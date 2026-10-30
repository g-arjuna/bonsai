use bonsai::remediation::{
    ApprovalBoundExecutor, AssuranceCheckSpec, AssuranceFidelity, BlastRadiusSummary,
    ChangeAssuranceReport, DigitalTwinSnapshot, DisruptionLevel, ExecutionEnforcementMode,
    NetworkInvariant, RecoveryPolicy, RemediationPlan, SignedPlanApproval,
    verify_plan_in_digital_twin,
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

fn approval_for(plan: &RemediationPlan, assurance: &ChangeAssuranceReport) -> SignedPlanApproval {
    SignedPlanApproval {
        approval_id: "probe".into(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        tenant_id: plan.tenant_id.clone(),
        approved_by: "probe".into(),
        operator_rationale: "diagnostic".into(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    }
}

#[test]
fn concurrent_executors_can_acquire_same_persisted_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut a = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let mut b = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    a.acquire_scope_lock("device", "vrf", "owner-a").unwrap();
    assert!(b.acquire_scope_lock("device", "vrf", "owner-b").is_ok());
}
#[test]
fn completed_replay_precedes_enforced_signature_authority_check() {
    let plan = sample_remediation_plan();
    let assurance = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let approval = approval_for(&plan, &assurance);
    let mut executor = ApprovalBoundExecutor::new();
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
    executor = executor.with_enforcement_mode(ExecutionEnforcementMode::Enforced);
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
fn changing_unsigned_assurance_fidelity_allows_enforced_execution() {
    let plan = sample_remediation_plan();
    let mut twin = sample_healthy_twin();
    twin.fidelity = AssuranceFidelity::ModeledSnapshotSimulation;
    let mut assurance = verify_plan_in_digital_twin(&plan, &twin);
    let mut approval = approval_for(&plan, &assurance);
    approval.sign_hmac_sha256("key", b"secret");
    let mut executor = ApprovalBoundExecutor::new()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
        .with_signing_key("key", b"secret");
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
            .is_err()
    );
    assurance.fidelity = AssuranceFidelity::LiveContainerlabVerified;
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
            .is_ok()
    );
}

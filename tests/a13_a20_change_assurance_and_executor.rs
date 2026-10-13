//! Integration tests for Priority 3 (P3: A13, A16–A20):
//! - A13: Deterministic second-NOS adapter workshop pilot (`nokia_srl`) separating parser schema drift from network failure
//! - A16: Typed `RemediationPlan` & network invariant schema with deterministic SHA-256 `plan_hash`
//! - A17: Digital-twin / Containerlab change assurance gate (`verify_plan_in_digital_twin` + MCP `evaluate_change_plan`)
//! - A18: Approval-bound executor enforcing `plan_hash`, baseline config revision, expiry, device/VRF lock, idempotency, and recovery policy
//! - A19–A20: Shadow operational pilot tracker with zero write operations

use std::sync::Arc;

use bonsai::graph::GraphStore;
use bonsai::mcp_server::call_tool_with_store_and_routing;
use bonsai::remediation::{
    ApprovalBoundExecutor, AssuranceCheckSpec, AssuranceFidelity, AssuranceVerdict,
    BlastRadiusSummary, DigitalTwinSnapshot, DisruptionLevel, ExecutionOutcome,
    ExecutionRejectionReason, NetworkInvariant, ParseQualityState, RecoveryPolicy, RemediationPlan,
    ShadowPilotAuditJournal, ShadowPilotObservation, SignedPlanApproval,
    parse_nokia_srl_bgp_neighbor_json, summarize_shadow_pilot, verify_plan_in_digital_twin,
};
use serde_json::json;
use uuid::Uuid;

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

#[test]
fn a13_nokia_srl_adapter_separates_nos_schema_drift_from_network_failure() {
    // 1. Canonical Nokia SR Linux 24.7.1 BGP neighbor JSON
    let raw_v24 = r#"{
        "software-version": "v24.7.1",
        "network-instance": "default",
        "peer-address": "198.51.100.1",
        "peer-as": 65001,
        "session-state": "established",
        "received-routes": 120,
        "active-routes": 118,
        "import-policy": "IMPORT-EBGP",
        "export-policy": "EXPORT-EBGP"
    }"#;
    let obs = parse_nokia_srl_bgp_neighbor_json(raw_v24).unwrap();
    assert_eq!(obs.nos_vendor, "nokia_srl");
    assert_eq!(obs.parse_quality, ParseQualityState::Complete);
    assert_eq!(obs.peer_as, 65001);
    assert_eq!(obs.received_routes, 120);
    assert!(obs.unparsed_fields.is_empty());

    // 2. Newer NOS release adds unknown telemetry fields: diagnosed as PartialUnknownFields, NOT network failure
    let raw_v25_drift = r#"{
        "software-version": "v25.3.1",
        "network-instance": "vrf-blue",
        "peer-address": "198.51.100.2",
        "peer-as": 65002,
        "session-state": "established",
        "received-routes": 42,
        "active-routes": 42,
        "multipath-ecmp-hash-seed": 99123,
        "post-quantum-tcp-ao-state": "enabled"
    }"#;
    let obs_drift = parse_nokia_srl_bgp_neighbor_json(raw_v25_drift).unwrap();
    assert_eq!(
        obs_drift.parse_quality,
        ParseQualityState::PartialUnknownFields
    );
    assert_eq!(obs_drift.session_state, "established");
    assert_eq!(
        obs_drift.unparsed_fields,
        vec![
            "multipath-ecmp-hash-seed".to_string(),
            "post-quantum-tcp-ao-state".to_string()
        ]
    );

    // 3. Incompatible schema missing network-instance -> SchemaMismatch
    let raw_mismatch = r#"{"unexpected-root": true}"#;
    let obs_mismatch = parse_nokia_srl_bgp_neighbor_json(raw_mismatch).unwrap();
    assert_eq!(
        obs_mismatch.parse_quality,
        ParseQualityState::SchemaMismatch
    );
}

#[tokio::test]
async fn a16_a17_digital_twin_assurance_gate_and_mcp_evaluate_change_plan() {
    let plan = sample_remediation_plan();
    let twin_ok = sample_healthy_twin();

    let report = verify_plan_in_digital_twin(&plan, &twin_ok);
    assert_eq!(report.verdict, AssuranceVerdict::Verified);
    assert!(report.executable);
    assert_eq!(report.plan_hash, plan.compute_plan_hash());
    assert!(
        report
            .emulation_fidelity_limitations
            .iter()
            .any(|m| m.contains("ASIC FIB programming")),
        "Digital-twin report must explicitly state emulation fidelity limitations"
    );

    // If the candidate change drops an eBGP peer in the digital twin, executable MUST be false.
    let mut twin_peer_drop = sample_healthy_twin();
    twin_peer_drop.ebgp_peers_up_after = 1;
    let failed_report = verify_plan_in_digital_twin(&plan, &twin_peer_drop);
    assert_eq!(failed_report.verdict, AssuranceVerdict::FailedInvariants);
    assert!(!failed_report.executable);

    // Verify MCP `evaluate_change_plan` tool end to end.
    let db_dir = std::env::temp_dir().join(format!("bonsai-a17-mcp-{}", Uuid::new_v4()));
    let store = Arc::new(GraphStore::open(db_dir.to_str().unwrap(), 256 * 1024 * 1024).unwrap());
    let mcp_res = call_tool_with_store_and_routing(
        &store,
        None,
        "evaluate_change_plan",
        &json!({
            "plan": plan,
            "digital_twin_snapshot": twin_ok,
        }),
    )
    .await
    .expect("evaluate_change_plan MCP tool succeeded");
    assert_eq!(mcp_res["verdict"], "verified");
    assert_eq!(mcp_res["executable"], true);
    let _ = std::fs::remove_dir_all(&db_dir);
}

#[test]
fn a18_approval_bound_executor_enforces_plan_hash_baseline_revision_locking_idempotency_and_recovery()
 {
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);

    let approval = SignedPlanApproval {
        approval_id: "appr-001".to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        approved_by: "netops-lead".to_string(),
        operator_rationale: "Verified in Containerlab FRR twin; restores 203.0.113.0/24"
            .to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    };

    let mut executor = ApprovalBoundExecutor::new();

    // 1. Mutated plan after approval -> rejected with PlanHashMismatch
    let mut mutated_plan = plan.clone();
    mutated_plan
        .candidate_diff
        .push_str("\n+ neighbor 198.51.100.1 shutdown");
    let err_mutated = executor
        .execute_approved_plan(
            "idem-1",
            &mutated_plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap_err();
    assert_eq!(
        err_mutated,
        ExecutionRejectionReason::UnverifiedInDigitalTwin
    );

    // Even if assurance hash matched mutated plan, approval.approved_plan_hash still rejects it:
    let mutated_assurance = verify_plan_in_digital_twin(&mutated_plan, &twin);
    let mut approval_with_mutated_assurance = approval.clone();
    approval_with_mutated_assurance.assurance_report_id = mutated_assurance.report_id.clone();
    let err_hash = executor
        .execute_approved_plan(
            "idem-1",
            &mutated_plan,
            &mutated_assurance,
            &approval_with_mutated_assurance,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap_err();
    assert_eq!(err_hash, ExecutionRejectionReason::PlanHashMismatch);

    // 2. Stale baseline configuration revision -> rejected with StaleBaselineConfigRevision
    let err_stale = executor
        .execute_approved_plan(
            "idem-1",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-002-concurrent-cli-edit",
            1_760_000_200,
            true,
            true,
        )
        .unwrap_err();
    assert_eq!(
        err_stale,
        ExecutionRejectionReason::StaleBaselineConfigRevision
    );

    // 3. Expired approval -> rejected with ApprovalExpired
    let err_expired = executor
        .execute_approved_plan(
            "idem-1",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_009_999,
            true,
            true,
        )
        .unwrap_err();
    assert_eq!(err_expired, ExecutionRejectionReason::ApprovalExpired);

    // 4. Conflicting lock held on (device, vrf) -> rejected with ConflictingChangeLockHeld
    executor
        .acquire_scope_lock("192.0.2.10", "0:0", "other-change-owner")
        .unwrap();
    let err_lock = executor
        .execute_approved_plan(
            "idem-1",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap_err();
    assert_eq!(
        err_lock,
        ExecutionRejectionReason::ConflictingChangeLockHeld
    );
    executor.release_scope_lock("192.0.2.10", "0:0", "other-change-owner");

    // 5. Valid execution -> CommittedAndPostchecksPassed, and duplicate idempotency_key is deduplicated
    let ok_entry = executor
        .execute_approved_plan(
            "idem-success-1",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        ok_entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert!(!ok_entry.deduplicated_replay);
    assert_eq!(executor.journal_entries().len(), 1);

    let replay_entry = executor
        .execute_approved_plan(
            "idem-success-1",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_250,
            true,
            true,
        )
        .unwrap();
    assert!(replay_entry.deduplicated_replay);
    assert_eq!(executor.journal_entries().len(), 1);

    // 6. Postcheck failure with HaltAndEscalate recovery policy -> HaltedForOperatorEscalation (never blindly reverse-diffs)
    let mut halt_plan = sample_remediation_plan();
    halt_plan.plan_id = "plan-halt-on-postcheck-fail".to_string();
    halt_plan.recovery_policy = RecoveryPolicy::HaltAndEscalate;
    halt_plan.rollback_steps.clear();
    let halt_assurance = verify_plan_in_digital_twin(&halt_plan, &twin);
    let halt_approval = SignedPlanApproval {
        approval_id: "appr-002".to_string(),
        plan_id: halt_plan.plan_id.clone(),
        approved_plan_hash: halt_plan.compute_plan_hash(),
        expected_baseline_config_revision: halt_plan.baseline_config_revision.clone(),
        approved_scope_device: halt_plan.device_address.clone(),
        approved_scope_vrf: halt_plan.vrf_rd.clone(),
        assurance_report_id: halt_assurance.report_id.clone(),
        approved_by: "netops-lead".to_string(),
        operator_rationale: "Halt and page oncall if postchecks fail".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    };

    let halt_entry = executor
        .execute_approved_plan(
            "idem-halt-1",
            &halt_plan,
            &halt_assurance,
            &halt_approval,
            "rev-2026-10-12-001",
            1_760_000_300,
            true,
            false,
        )
        .unwrap();
    assert_eq!(
        halt_entry.outcome,
        ExecutionOutcome::HaltedForOperatorEscalation
    );
}

#[test]
fn a19_shadow_operational_pilot_records_utility_and_enforces_zero_writes() {
    let episodes = vec![
        ShadowPilotObservation {
            episode_id: "ep-1".to_string(),
            prefix: "203.0.113.0/24".to_string(),
            vrf_rd: "0:0".to_string(),
            investigator_status: "supported".to_string(),
            operator_utility_verdict: "useful".to_string(),
            failure_taxonomy_tag: None,
            write_operations_attempted: 0,
        },
        ShadowPilotObservation {
            episode_id: "ep-2".to_string(),
            prefix: "198.51.100.0/24".to_string(),
            vrf_rd: "65000:100".to_string(),
            investigator_status: "partial".to_string(),
            operator_utility_verdict: "useful".to_string(),
            failure_taxonomy_tag: Some("missing_post_policy_bmp_view".to_string()),
            write_operations_attempted: 0,
        },
    ];
    let summary = summarize_shadow_pilot(&episodes);
    assert_eq!(summary.total_episodes, 2);
    assert_eq!(summary.operator_utility_rate_pct, 100.0);
    assert_eq!(summary.total_write_operations_attempted, 0);
    assert!(summary.emulation_boundary_respected);
    assert!(summary.prototype_status_verified);
}

#[test]
fn a18_cryptographic_signatures_and_executor_restart_persistence() {
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let secret = b"super-secret-approval-signing-key-32bytes!";

    let mut signed_approval = SignedPlanApproval {
        approval_id: "appr-crypto-001".to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        approved_by: "secops-officer".to_string(),
        operator_rationale: "Cryptographically verified change".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    };

    signed_approval.sign_hmac_sha256("key-primary-1", secret);
    assert_eq!(signed_approval.signature_algorithm, "HMAC-SHA256");
    assert!(!signed_approval.signature_hex.is_empty());
    assert!(signed_approval.verify_signature(secret).is_ok());

    // Tampering with payload fails signature verification:
    let mut tampered = signed_approval.clone();
    tampered.approved_plan_hash = "tampered-hash-000".to_string();
    assert_eq!(
        tampered.verify_signature(secret).unwrap_err(),
        ExecutionRejectionReason::InvalidSignature
    );

    // Wrong secret fails signature verification:
    assert_eq!(
        signed_approval.verify_signature(b"wrong-key").unwrap_err(),
        ExecutionRejectionReason::InvalidSignature
    );

    // Persistent executor journal:
    let journal_dir = tempfile::tempdir().unwrap();
    let journal_path = journal_dir.path().join("executor_journal.jsonl");

    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&journal_path))
        .unwrap()
        .with_signing_secret(secret);

    // Execution with signed approval succeeds and writes to journal:
    let entry = executor
        .execute_approved_plan(
            "idem-crypto-1",
            &plan,
            &assurance,
            &signed_approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("execution must succeed");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert!(!entry.deduplicated_replay);
    assert!(journal_path.exists());

    // Simulate process crash and restart with a new executor instance from the same journal path:
    let mut restarted_executor = ApprovalBoundExecutor::open_or_create(Some(&journal_path))
        .unwrap()
        .with_signing_secret(secret);
    assert_eq!(restarted_executor.journal_entries().len(), 1);

    // Submitting the same idempotency key after restart returns deduplicated replay:
    let replay_entry = restarted_executor
        .execute_approved_plan(
            "idem-crypto-1",
            &plan,
            &assurance,
            &signed_approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("deduplicated replay must succeed");
    assert!(replay_entry.deduplicated_replay);
}

#[test]
fn a19_shadow_pilot_audit_journal_persists_observations() {
    let tmp = tempfile::tempdir().unwrap();
    let audit_path = tmp.path().join("shadow_audit.jsonl");

    let journal = ShadowPilotAuditJournal::open_or_create(&audit_path).unwrap();

    let obs1 = ShadowPilotObservation {
        episode_id: "ep-001".to_string(),
        prefix: "198.51.100.0/24".to_string(),
        vrf_rd: "0:0".to_string(),
        investigator_status: "completed".to_string(),
        operator_utility_verdict: "useful".to_string(),
        failure_taxonomy_tag: Some("import_policy_rejected".to_string()),
        write_operations_attempted: 0,
    };
    let obs2 = ShadowPilotObservation {
        episode_id: "ep-002".to_string(),
        prefix: "203.0.113.0/24".to_string(),
        vrf_rd: "0:0".to_string(),
        investigator_status: "completed".to_string(),
        operator_utility_verdict: "useful".to_string(),
        failure_taxonomy_tag: None,
        write_operations_attempted: 0,
    };

    journal.record_observation(&obs1).unwrap();
    journal.record_observation(&obs2).unwrap();

    let summary = journal.summarize().unwrap();
    assert_eq!(summary.total_episodes, 2);
    assert_eq!(summary.useful_episodes, 2);
    assert_eq!(summary.operator_utility_rate_pct, 100.0);
    assert_eq!(summary.total_write_operations_attempted, 0);
    assert!(summary.emulation_boundary_respected);
    assert!(summary.prototype_status_verified);
}

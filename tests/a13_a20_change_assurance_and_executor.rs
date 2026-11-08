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
    BlastRadiusSummary, DigitalTwinSnapshot, DisruptionLevel, ExecutionEnforcementMode,
    ExecutionJournalRecord, ExecutionOutcome, ExecutionRejectionReason, NetworkInvariant,
    ParseQualityState, RecoveryPolicy, RemediationPlan, ShadowPilotAuditJournal,
    ShadowPilotObservation, SignedPlanApproval, parse_nokia_srl_bgp_neighbor_json,
    summarize_shadow_pilot, verify_plan_in_digital_twin,
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
        &bonsai::mcp_server::ToolExecutionContext::global(bonsai::http_server::auth::Role::Admin),
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
        approved_assurance_report_hash: assurance.compute_report_hash(),
        tenant_id: plan.tenant_id.clone(),
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
    assert!(
        executor
            .release_scope_lock("192.0.2.10", "0:0", "other-change-owner")
            .is_ok()
    );

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
        approved_assurance_report_hash: halt_assurance.compute_report_hash(),
        tenant_id: halt_plan.tenant_id.clone(),
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
        approved_assurance_report_hash: assurance.compute_report_hash(),
        tenant_id: plan.tenant_id.clone(),
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

// ── BN-001 & BN-002 Verification Tests ────────────────────────────────────────

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
fn bn001_failed_disk_append_never_replays_as_success_from_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");

    // Initialize journal cleanly
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();

    // Obstruct the journal path by creating a directory where the file was
    std::fs::remove_file(&path).ok();
    std::fs::create_dir(&path).unwrap();

    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    // First call must fail with a journal persistence error (not PrecheckFailed)
    let first_res = executor.execute_approved_plan(
        "idem-fail-test",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match first_res {
        Err(ExecutionRejectionReason::JournalPersistenceError(_)) => {}
        other => panic!("expected JournalPersistenceError, got {:?}", other),
    }

    // Crucial check: A retry with the same idempotency key must NOT replay as success!
    let retry_res = executor.execute_approved_plan(
        "idem-fail-test",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match retry_res {
        Err(ExecutionRejectionReason::JournalPersistenceError(_)) => {}
        other => panic!(
            "retry must fail persistence again, not replay as success: {:?}",
            other
        ),
    }

    // Now remove the obstruction so writes succeed
    std::fs::remove_dir(&path).unwrap();

    let ok_res = executor
        .execute_approved_plan(
            "idem-fail-test",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("execution must succeed once disk is writable");
    assert!(!ok_res.deduplicated_replay);
    assert_eq!(executor.journal_entries().len(), 1);

    // Now that it was durably persisted, a subsequent call correctly dedup-replays
    let third_res = executor
        .execute_approved_plan(
            "idem-fail-test",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("deduplicated replay must succeed");
    assert!(third_res.deduplicated_replay);
}

#[test]
fn bn001_active_scope_lock_is_persisted_and_recovered_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");

    let mut first = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    first
        .acquire_scope_lock("192.0.2.10", "0:0", "change-agent-1")
        .unwrap();
    drop(first);

    // Reopening executor from the same journal path recovers the active scope lock
    let mut second = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert_eq!(
        second
            .active_locks()
            .get(&("192.0.2.10".into(), "0:0".into())),
        Some(&"change-agent-1".to_string())
    );

    // Conflicting owner is rejected
    let conflict = second.acquire_scope_lock("192.0.2.10", "0:0", "change-agent-2");
    assert_eq!(
        conflict,
        Err(ExecutionRejectionReason::ConflictingChangeLockHeld)
    );

    // Same owner is permitted
    assert!(
        second
            .acquire_scope_lock("192.0.2.10", "0:0", "change-agent-1")
            .is_ok()
    );

    // Releasing the lock persists release
    assert!(
        second
            .release_scope_lock("192.0.2.10", "0:0", "change-agent-1")
            .is_ok()
    );
    drop(second);

    // Third executor confirms lock is now free
    let mut third = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert!(third.active_locks().is_empty());
    assert!(
        third
            .acquire_scope_lock("192.0.2.10", "0:0", "change-agent-2")
            .is_ok()
    );
}

#[test]
fn bn001_idempotency_replay_strictly_bound_to_plan_scope_and_tenant() {
    let mut executor = ApprovalBoundExecutor::new();
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    // 1. Initial execution succeeds
    let entry = executor
        .execute_approved_plan(
            "idem-key-bound",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap();
    assert!(!entry.deduplicated_replay);

    // 2. Exact match replays cleanly
    let replay = executor
        .execute_approved_plan(
            "idem-key-bound",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .unwrap();
    assert!(replay.deduplicated_replay);

    // 3. Replay with different plan_id fails with IdempotencyConflict
    let mut diff_plan = plan.clone();
    diff_plan.plan_id = "plan-different-id".into();
    let diff_assurance = verify_plan_in_digital_twin(&diff_plan, &twin);
    let mut diff_approval = approval.clone();
    diff_approval.plan_id = diff_plan.plan_id.clone();
    diff_approval.assurance_report_id = diff_assurance.report_id.clone();
    diff_approval.approved_plan_hash = diff_plan.compute_plan_hash();

    let err_plan = executor.execute_approved_plan(
        "idem-key-bound",
        &diff_plan,
        &diff_assurance,
        &diff_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match err_plan {
        Err(ExecutionRejectionReason::IdempotencyConflict(_)) => {}
        other => panic!(
            "expected IdempotencyConflict on mismatched plan, got {:?}",
            other
        ),
    }

    // 4. Replay with different device scope fails with IdempotencyConflict
    let mut diff_scope_plan = plan.clone();
    diff_scope_plan.device_address = "192.0.2.99".into();
    let diff_scope_assurance = verify_plan_in_digital_twin(&diff_scope_plan, &twin);
    let mut diff_scope_approval = approval.clone();
    diff_scope_approval.approved_scope_device = "192.0.2.99".into();
    diff_scope_approval.assurance_report_id = diff_scope_assurance.report_id.clone();
    diff_scope_approval.approved_plan_hash = diff_scope_plan.compute_plan_hash();

    let err_scope = executor.execute_approved_plan(
        "idem-key-bound",
        &diff_scope_plan,
        &diff_scope_assurance,
        &diff_scope_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match err_scope {
        Err(ExecutionRejectionReason::IdempotencyConflict(_)) => {}
        other => panic!(
            "expected IdempotencyConflict on mismatched scope, got {:?}",
            other
        ),
    }

    // 5. Replay with different tenant fails with IdempotencyConflict
    let mut diff_tenant_plan = plan.clone();
    diff_tenant_plan.tenant_id = "tenant-xyz".into();
    let diff_tenant_assurance = verify_plan_in_digital_twin(&diff_tenant_plan, &twin);
    let mut diff_tenant_approval = approval.clone();
    diff_tenant_approval.tenant_id = "tenant-xyz".into();
    diff_tenant_approval.assurance_report_id = diff_tenant_assurance.report_id.clone();
    diff_tenant_approval.approved_plan_hash = diff_tenant_plan.compute_plan_hash();

    let err_tenant = executor.execute_approved_plan(
        "idem-key-bound",
        &diff_tenant_plan,
        &diff_tenant_assurance,
        &diff_tenant_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match err_tenant {
        Err(ExecutionRejectionReason::IdempotencyConflict(_)) => {}
        other => panic!(
            "expected IdempotencyConflict on mismatched tenant, got {:?}",
            other
        ),
    }
}

#[test]
fn bn001_journal_rejects_corrupt_interior_records_and_repairs_torn_tail() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("corrupt_test_journal.jsonl");

    // Part A: Corrupt interior line must return an error
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    {
        let mut f = std::fs::File::create(&journal_path).unwrap();
        // Valid line 1
        let mut executor = ApprovalBoundExecutor::new();
        let entry1 = executor
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
            .unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&ExecutionJournalRecord::ExecutionCompleted { entry: entry1 })
                .unwrap()
        )
        .unwrap();

        // Corrupt interior line (invalid JSON ending with newline)
        writeln!(
            f,
            "{{\"record_type\":\"corrupted_interior\", invalid_json..."
        )
        .unwrap();

        // Valid line 2
        let mut plan2 = plan.clone();
        plan2.plan_id = "plan-2".into();
        let assurance2 = verify_plan_in_digital_twin(&plan2, &twin);
        let mut approval2 = approval.clone();
        approval2.plan_id = "plan-2".into();
        approval2.assurance_report_id = assurance2.report_id.clone();
        approval2.approved_plan_hash = plan2.compute_plan_hash();

        let entry2 = executor
            .execute_approved_plan(
                "idem-2",
                &plan2,
                &assurance2,
                &approval2,
                "rev-2026-10-12-001",
                1_760_000_200,
                true,
                true,
            )
            .unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&ExecutionJournalRecord::ExecutionCompleted { entry: entry2 })
                .unwrap()
        )
        .unwrap();
    }

    // Opening with corrupt interior line must return Err
    assert!(
        ApprovalBoundExecutor::open_or_create(Some(&journal_path)).is_err(),
        "corrupt interior line must be rejected"
    );

    // Part B: Torn tail (partial bytes without trailing newline) must be repaired
    {
        let mut f = std::fs::File::create(&journal_path).unwrap();
        // Valid line 1
        let mut executor = ApprovalBoundExecutor::new();
        let entry1 = executor
            .execute_approved_plan(
                "idem-valid-1",
                &plan,
                &assurance,
                &approval,
                "rev-2026-10-12-001",
                1_760_000_200,
                true,
                true,
            )
            .unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&ExecutionJournalRecord::ExecutionCompleted { entry: entry1 })
                .unwrap()
        )
        .unwrap();

        // Torn crash tail (no newline)
        write!(f, "{{\"record_type\":\"execution_intent\",\"partial\":true").unwrap();
    }

    // Opening must succeed and truncate the torn tail
    let mut repaired_executor = ApprovalBoundExecutor::open_or_create(Some(&journal_path))
        .expect("torn tail must be repaired cleanly");
    assert_eq!(repaired_executor.journal_entries().len(), 1);

    // Appending after repair writes valid records without corruption
    let mut plan3 = plan.clone();
    plan3.plan_id = "plan-3".into();
    let assurance3 = verify_plan_in_digital_twin(&plan3, &twin);
    let mut approval3 = approval.clone();
    approval3.plan_id = "plan-3".into();
    approval3.assurance_report_id = assurance3.report_id.clone();
    approval3.approved_plan_hash = plan3.compute_plan_hash();

    repaired_executor
        .execute_approved_plan(
            "idem-valid-3",
            &plan3,
            &assurance3,
            &approval3,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("append after torn tail repair must succeed");
    assert_eq!(repaired_executor.journal_entries().len(), 2);

    // Reopening verifies both entries are valid
    let reopened = ApprovalBoundExecutor::open_or_create(Some(&journal_path)).unwrap();
    assert_eq!(reopened.journal_entries().len(), 2);
}

#[test]
fn bn001_execution_intent_is_persisted_before_effects_and_recovered_in_flight() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let journal_path = dir.path().join("in_flight_journal.jsonl");

    // Simulate an interrupted execution (intent persisted, but process crashed before completion)
    {
        let mut f = std::fs::File::create(&journal_path).unwrap();
        let lock_rec = ExecutionJournalRecord::LockAcquired {
            device_address: "192.0.2.10".into(),
            vrf_rd: "0:0".into(),
            owner_id: "in-flight-approval".into(),
            acquired_at_epoch_secs: 1_760_000_100,
        };
        writeln!(f, "{}", serde_json::to_string(&lock_rec).unwrap()).unwrap();

        let intent_rec = ExecutionJournalRecord::ExecutionIntent {
            execution_id: "exec-in-flight-001".into(),
            idempotency_key: "idem-crashed-job".into(),
            plan_id: "plan-fix-rm-deny-203-0-113".into(),
            plan_hash: "plan-hash-123".into(),
            tenant_id: "default".into(),
            approval_id: "in-flight-approval".into(),
            device_address: "192.0.2.10".into(),
            vrf_rd: "0:0".into(),
            started_at_epoch_secs: 1_760_000_100,
        };
        writeln!(f, "{}", serde_json::to_string(&intent_rec).unwrap()).unwrap();
    }

    // Reopen executor
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&journal_path)).unwrap();

    // 1. In-flight execution is recovered
    let in_flight = executor.in_flight_executions();
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].idempotency_key, "idem-crashed-job");

    // 2. Scope lock is retained
    assert_eq!(
        executor
            .active_locks()
            .get(&("192.0.2.10".into(), "0:0".into())),
        Some(&"in-flight-approval".to_string())
    );

    // 3. Retry with in-flight key is rejected rather than faking success or replaying
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    let err = executor.execute_approved_plan(
        "idem-crashed-job",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match err {
        Err(ExecutionRejectionReason::InFlightExecutionInterrupted(_)) => {}
        other => panic!("expected InFlightExecutionInterrupted, got {:?}", other),
    }
}

#[test]
fn bn002_enforced_mode_requires_signing_authority_and_rejects_modeled_assurance() {
    let plan = sample_remediation_plan();
    let key_id = "key-prod-alpha-1";
    let secret = b"super-strong-production-signing-secret-key-32b!";

    let mut valid_twin = sample_healthy_twin();
    valid_twin.fidelity = AssuranceFidelity::LiveContainerlabVerified;
    // BN-002: enforced mode requires the assurance report itself to be signed.
    let mut live_assurance = verify_plan_in_digital_twin(&plan, &valid_twin);
    live_assurance.sign_hmac_sha256(key_id, secret);

    let mut modeled_twin = sample_healthy_twin();
    modeled_twin.fidelity = AssuranceFidelity::ModeledSnapshotSimulation;
    let modeled_assurance = verify_plan_in_digital_twin(&plan, &modeled_twin);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enforced_journal.jsonl");

    // 0. Enforced mode requires durable journal backing (unbacked rejected fail-closed)
    let mut unbacked_executor =
        ApprovalBoundExecutor::new().with_enforcement_mode(ExecutionEnforcementMode::Enforced);
    let mut unsigned_approval = sample_probe_approval(&plan);
    unsigned_approval.approved_assurance_report_hash = live_assurance.compute_report_hash();

    let err_unbacked = unbacked_executor.execute_approved_plan(
        "idem-enforced-0",
        &plan,
        &live_assurance,
        &unsigned_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_unbacked,
        Err(ExecutionRejectionReason::DurableBackingRequiredInEnforcedMode)
    );

    // 1. Missing signature authority in enforced mode
    let mut executor_no_keys = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced);

    let err_no_auth = executor_no_keys.execute_approved_plan(
        "idem-enforced-1",
        &plan,
        &live_assurance,
        &unsigned_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_no_auth,
        Err(ExecutionRejectionReason::MissingSignatureAuthority)
    );

    // 2. Unsigned approval in enforced mode with keys configured
    let mut enforced_executor = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
        .with_signing_key(key_id, secret);

    let err_unsigned = enforced_executor.execute_approved_plan(
        "idem-enforced-2",
        &plan,
        &live_assurance,
        &unsigned_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_unsigned,
        Err(ExecutionRejectionReason::InvalidSignature)
    );

    // 3. Modeled snapshot simulation rejected in enforced mode
    let mut signed_approval = sample_probe_approval(&plan);
    signed_approval.approved_assurance_report_hash = live_assurance.compute_report_hash();
    signed_approval.sign_hmac_sha256(key_id, secret);
    assert!(signed_approval.verify_signature(secret).is_ok());

    let mut signed_modeled_approval = sample_probe_approval(&plan);
    signed_modeled_approval.assurance_report_id = modeled_assurance.report_id.clone();
    signed_modeled_approval.approved_assurance_report_hash =
        modeled_assurance.compute_report_hash();
    signed_modeled_approval.sign_hmac_sha256(key_id, secret);

    let err_modeled = enforced_executor.execute_approved_plan(
        "idem-enforced-3",
        &plan,
        &modeled_assurance,
        &signed_modeled_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_modeled,
        Err(ExecutionRejectionReason::ModeledAssuranceRejectedInEnforcedMode)
    );

    // 4. Revoked key rejected in enforced mode. Key revocations are durable
    // and shared across executor instances (BN-002), so this case uses a
    // dedicated revoked key to avoid invalidating key-primary-1 for the
    // remaining assertions.
    let revoked_key_id = "key-revoked-1";
    let mut revoked_executor = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
        .with_signing_key(revoked_key_id, secret)
        .with_revoked_key(revoked_key_id);

    let mut revoked_approval = signed_approval.clone();
    revoked_approval.key_id = revoked_key_id.to_string();
    revoked_approval.sign_hmac_sha256(revoked_key_id, secret);

    let err_revoked = revoked_executor.execute_approved_plan(
        "idem-enforced-4",
        &plan,
        &live_assurance,
        &revoked_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_revoked,
        Err(ExecutionRejectionReason::RevokedSigningKey(
            revoked_key_id.to_string()
        ))
    );

    // 5. Tenant mismatch rejected in enforced mode
    let mut tenant_mismatched_approval = signed_approval.clone();
    tenant_mismatched_approval.tenant_id = "alien-tenant".to_string();
    tenant_mismatched_approval.sign_hmac_sha256(key_id, secret);

    let err_tenant = enforced_executor.execute_approved_plan(
        "idem-enforced-5",
        &plan,
        &live_assurance,
        &tenant_mismatched_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(err_tenant, Err(ExecutionRejectionReason::TenantMismatch));

    // 5b. Empty tenant ID rejected in enforced mode
    let mut empty_tenant_approval = signed_approval.clone();
    empty_tenant_approval.tenant_id = "".to_string();
    empty_tenant_approval.sign_hmac_sha256(key_id, secret);

    let err_empty_tenant = enforced_executor.execute_approved_plan(
        "idem-enforced-5b",
        &plan,
        &live_assurance,
        &empty_tenant_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        err_empty_tenant,
        Err(ExecutionRejectionReason::EmptyTenantId)
    );

    // 6. Valid live verified plan with valid signature succeeds in enforced mode
    let ok_enforced = enforced_executor
        .execute_approved_plan(
            "idem-enforced-6",
            &plan,
            &live_assurance,
            &signed_approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("valid signed execution with live assurance must succeed in enforced mode");
    assert_eq!(
        ok_enforced.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );

    // 7. Simulation mode allows modeled snapshot for developer workflows
    let mut sim_executor = ApprovalBoundExecutor::new();
    let ok_sim = sim_executor.execute_approved_plan(
        "idem-sim-1",
        &plan,
        &modeled_assurance,
        &unsigned_approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert!(ok_sim.is_ok(), "simulation mode allows modeled snapshot");
}

#[test]
fn bn001_concurrent_executors_cannot_acquire_same_persisted_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");

    let mut a = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let mut b = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();

    a.acquire_scope_lock("192.0.2.10", "0:0", "owner-a")
        .unwrap();
    let res_b = b.acquire_scope_lock("192.0.2.10", "0:0", "owner-b");
    assert_eq!(
        res_b,
        Err(ExecutionRejectionReason::ConflictingChangeLockHeld),
        "concurrent executor must observe persisted scope lock held by owner-a and reject owner-b"
    );
}

#[test]
fn bn002_changing_unsigned_assurance_fidelity_is_rejected_in_enforced_mode() {
    let plan = sample_remediation_plan();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");

    let mut twin = sample_healthy_twin();
    twin.fidelity = AssuranceFidelity::ModeledSnapshotSimulation;
    let mut modeled_assurance = verify_plan_in_digital_twin(&plan, &twin);

    let mut approval = sample_probe_approval(&plan);
    approval.approved_assurance_report_hash = modeled_assurance.compute_report_hash();
    approval.sign_hmac_sha256("key-1", b"secret-1234");

    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
        .with_signing_key("key-1", b"secret-1234");

    // First attempt: rejected because modeled assurance is rejected in enforced mode
    let res1 = executor.execute_approved_plan(
        "idem-probe-1",
        &plan,
        &modeled_assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        res1,
        Err(ExecutionRejectionReason::ModeledAssuranceRejectedInEnforcedMode)
    );

    // Caller attempts to bypass by tampering with the fidelity enum in memory:
    modeled_assurance.fidelity = AssuranceFidelity::LiveContainerlabVerified;
    let res2 = executor.execute_approved_plan(
        "idem-probe-1",
        &plan,
        &modeled_assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    assert_eq!(
        res2,
        Err(ExecutionRejectionReason::AssuranceReportHashMismatch),
        "tampering with fidelity enum must invalidate assurance report hash binding"
    );
}

#[test]
fn bn002_completed_replay_in_enforced_mode_requires_signing_authority_and_rejects_simulation_replay()
 {
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    // BN-002: enforced-mode executions require a signed assurance report.
    let mut assurance = verify_plan_in_digital_twin(&plan, &twin);
    assurance.sign_hmac_sha256("key-1", b"secret-1234");
    let mut approval = sample_probe_approval(&plan);
    approval.approved_assurance_report_hash = assurance.compute_report_hash();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");

    // Step 1: Run in simulation mode (no signing key)
    let mut sim_executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let out_sim = sim_executor
        .execute_approved_plan(
            "idem-sim-replay",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("simulation execution succeeds");
    assert_eq!(
        out_sim.enforcement_mode,
        ExecutionEnforcementMode::Simulation
    );

    // Step 2: Switch to enforced mode without signing authority
    let mut enforced_no_auth = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced);
    let replay_err1 = enforced_no_auth.execute_approved_plan(
        "idem-sim-replay",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    // Simulation replay must be rejected in enforced mode
    assert_eq!(
        replay_err1,
        Err(ExecutionRejectionReason::SimulationReplayRejectedInEnforcedMode)
    );

    // Step 3: Now run an enforced execution with signing authority
    approval.sign_hmac_sha256("key-1", b"secret-1234");
    let mut enforced_with_auth = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced)
        .with_signing_key("key-1", b"secret-1234");

    let out_enforced = enforced_with_auth
        .execute_approved_plan(
            "idem-enforced-replay",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("enforced execution succeeds");
    assert_eq!(
        out_enforced.enforcement_mode,
        ExecutionEnforcementMode::Enforced
    );

    // Step 4: BN-002 durable key lifecycle — signing authority now persists in
    // the executor key store beside the journal, so an enforced executor with
    // no locally configured keys still reloads key-1 and honors replay. This
    // replaces the previous memory-only assumption; MissingSignatureAuthority
    // on a keyless store is covered by bn002_enforced_mode_requires_signing_authority.
    let mut enforced_replay_no_auth = ApprovalBoundExecutor::open_or_create(Some(&path))
        .unwrap()
        .with_enforcement_mode(ExecutionEnforcementMode::Enforced);
    let replay_durable = enforced_replay_no_auth.execute_approved_plan(
        "idem-enforced-replay",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match replay_durable {
        Ok(entry) => {
            assert!(entry.deduplicated_replay);
            assert_eq!(entry.enforcement_mode, ExecutionEnforcementMode::Enforced);
        }
        Err(e) => {
            panic!("durable key store must supply signing authority after restart, got {e:?}")
        }
    }

    // Step 5: Replay with signing authority succeeds
    let replay_ok = enforced_with_auth
        .execute_approved_plan(
            "idem-enforced-replay",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("enforced replay succeeds with valid authority");
    assert!(replay_ok.deduplicated_replay);
}

#[test]
fn bn002_canonical_signing_bytes_eliminates_delimiter_collisions() {
    let plan = sample_remediation_plan();
    let mut first = sample_probe_approval(&plan);
    let mut second = first.clone();

    // Boundary shift that caused delimiter collision with unescaped pipes
    first.approval_id = "a|plan_id=b".into();
    first.plan_id = "c".into();

    second.approval_id = "a".into();
    second.plan_id = "b|plan_id=c".into();

    assert_ne!(first.approval_id, second.approval_id);
    assert_ne!(
        first.canonical_signing_bytes(),
        second.canonical_signing_bytes(),
        "canonical signing bytes must be collision-free across delimiter variations"
    );

    // Signing first does not validate second
    let secret = b"my-signing-key-32bytes-secret!!";
    first.sign_hmac_sha256("key-1", secret);
    second.signature_algorithm = first.signature_algorithm.clone();
    second.key_id = first.key_id.clone();
    second.signature_hex = first.signature_hex.clone();

    assert!(first.verify_signature(secret).is_ok());
    assert_eq!(
        second.verify_signature(secret),
        Err(ExecutionRejectionReason::InvalidSignature)
    );
}

#[test]
fn bn001_corruption_added_after_completion_fails_closed_on_replay() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    executor
        .execute_approved_plan(
            "idem-corrupt-after",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("first execution succeeds");

    // Corrupt journal on disk after completion
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{{corrupted-unparseable-json}}").unwrap();
    file.sync_all().unwrap();

    // Replay attempt must FAIL CLOSED with JournalPersistenceError rather than serving memory cache
    let replay_res = executor.execute_approved_plan(
        "idem-corrupt-after",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match replay_res {
        Err(ExecutionRejectionReason::JournalPersistenceError(_)) => {}
        other => panic!("expected JournalPersistenceError, got {:?}", other),
    }
}

#[test]
fn bn001_empty_durable_journal_fails_closed_on_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let plan = sample_remediation_plan();
    let twin = sample_healthy_twin();
    let assurance = verify_plan_in_digital_twin(&plan, &twin);
    let approval = sample_probe_approval(&plan);

    executor
        .execute_approved_plan(
            "idem-empty-after",
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_200,
            true,
            true,
        )
        .expect("first execution succeeds");

    // Truncate journal file to empty after completion
    std::fs::write(&path, b"").unwrap();

    // Replay attempt must FAIL CLOSED with JournalPersistenceError
    let replay_res = executor.execute_approved_plan(
        "idem-empty-after",
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        true,
        true,
    );
    match replay_res {
        Err(ExecutionRejectionReason::JournalPersistenceError(_)) => {}
        other => panic!("expected JournalPersistenceError, got {:?}", other),
    }
}

#[test]
fn bn002_report_identity_delimiters_eliminate_identical_hashes() {
    let plan = sample_remediation_plan();
    let mut a = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let mut b = a.clone();
    a.report_id = "a:b".into();
    a.plan_id = "c".into();
    b.report_id = "a".into();
    b.plan_id = "b:c".into();
    assert_ne!(a.report_id, b.report_id);
    assert_ne!(
        a.compute_report_hash(),
        b.compute_report_hash(),
        "delimiter boundary shifting must not create hash collisions"
    );
}

#[test]
fn bn002_fidelity_limitations_are_bound_by_report_hash() {
    let plan = sample_remediation_plan();
    let a = verify_plan_in_digital_twin(&plan, &sample_healthy_twin());
    let mut b = a.clone();
    b.emulation_fidelity_limitations = vec!["Independent lab evidence unavailable".into()];
    assert_ne!(
        a.emulation_fidelity_limitations,
        b.emulation_fidelity_limitations
    );
    assert_ne!(
        a.compute_report_hash(),
        b.compute_report_hash(),
        "fidelity limitations must be bound into report hash"
    );
}

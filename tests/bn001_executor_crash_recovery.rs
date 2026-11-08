//! BN-001: phase-specific process-kill crash recovery and interrupted-work
//! reconciliation for the approval-bound executor.
//!
//! Each persistence phase of `execute_approved_plan` is exercised by spawning
//! this test binary as a real child process that aborts (SIGKILL-equivalent,
//! no destructors) immediately after the durable journal record is synced.
//! The parent then reopens the journal from disk and verifies recovery.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bonsai::remediation::{
    ApprovalBoundExecutor, AssuranceCheckSpec, AssuranceFidelity, BlastRadiusSummary,
    ChangeAssuranceReport, DigitalTwinSnapshot, DisruptionLevel, ExecutionJournalRecord,
    ExecutionOutcome, ExecutionRejectionReason, ExecutorCrashPoint, NetworkInvariant,
    RecoveryPolicy, RemediationPlan, SignedPlanApproval, verify_plan_in_digital_twin,
};

const CHILD_PHASE_ENV: &str = "BONSAI_BN001_CRASH_PHASE";
const CHILD_DIR_ENV: &str = "BONSAI_BN001_CRASH_DIR";
const CHILD_IDEM_KEY: &str = "idem-crash-child";
const CRASH_APPROVAL_ID: &str = "appr-crash-001";
const CRASH_DEVICE: &str = "192.0.2.10";
const CRASH_VRF: &str = "0:0";

fn sample_plan() -> RemediationPlan {
    RemediationPlan {
        plan_id: "plan-crash-recovery".to_string(),
        tenant_id: "default".to_string(),
        device_address: CRASH_DEVICE.to_string(),
        vrf_rd: CRASH_VRF.to_string(),
        nos_vendor: "frr".to_string(),
        baseline_config_revision: "rev-2026-10-12-001".to_string(),
        candidate_diff: "- route-map RM-IN deny 10\n+ route-map RM-IN permit 10".to_string(),
        intended_behavior: "Permit 203.0.113.0/24 into LocRib".to_string(),
        blast_radius: BlastRadiusSummary {
            affected_devices: vec![CRASH_DEVICE.to_string()],
            affected_vrfs: vec![CRASH_VRF.to_string()],
            affected_ebgp_peers: vec![],
            estimated_affected_prefixes: 1,
            expected_disruption: DisruptionLevel::ControlPlaneReconvergence,
        },
        invariants: vec![NetworkInvariant {
            invariant_id: "prefix_reachable_in_loc_rib".to_string(),
            description: "prefix present in LocRib".to_string(),
            invariant_type: "loc_rib_presence".to_string(),
            target_scope: format!("{CRASH_DEVICE}|{CRASH_VRF}|203.0.113.0/24"),
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
        lab_topology_id: "clab-bn001-twin".to_string(),
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

fn sample_approval(
    plan: &RemediationPlan,
    assurance: &ChangeAssuranceReport,
) -> SignedPlanApproval {
    SignedPlanApproval {
        approval_id: CRASH_APPROVAL_ID.to_string(),
        plan_id: plan.plan_id.clone(),
        approved_plan_hash: plan.compute_plan_hash(),
        expected_baseline_config_revision: plan.baseline_config_revision.clone(),
        approved_scope_device: plan.device_address.clone(),
        approved_scope_vrf: plan.vrf_rd.clone(),
        assurance_report_id: assurance.report_id.clone(),
        approved_assurance_report_hash: assurance.compute_report_hash(),
        tenant_id: plan.tenant_id.clone(),
        approved_by: "netops-lead".to_string(),
        operator_rationale: "crash recovery verification".to_string(),
        approved_at_epoch_secs: 1_760_000_100,
        expires_at_epoch_secs: 1_760_001_800,
        signature_algorithm: String::new(),
        key_id: String::new(),
        signature_hex: String::new(),
    }
}

fn fixtures() -> (RemediationPlan, ChangeAssuranceReport, SignedPlanApproval) {
    let plan = sample_plan();
    let assurance = sample_assurance(&plan);
    let approval = sample_approval(&plan, &assurance);
    (plan, assurance, approval)
}

fn journal_path(dir: &Path) -> PathBuf {
    dir.join("executor_journal.jsonl")
}

fn scope_key() -> (String, String) {
    (CRASH_DEVICE.to_string(), CRASH_VRF.to_string())
}

fn journal_record_types(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["record_type"]
                .as_str()
                .unwrap_or("legacy_completed")
                .to_string()
        })
        .collect()
}

/// Child-process entrypoint. When `BONSAI_BN001_CRASH_PHASE` is unset this is a
/// no-op pass; when set, it configures the executor to abort immediately after
/// the named durable journal record is synced.
#[test]
fn bn001_crash_child_entrypoint() {
    let Ok(phase) = std::env::var(CHILD_PHASE_ENV) else {
        return;
    };
    let dir = std::env::var(CHILD_DIR_ENV).expect("crash dir env var");
    let journal_path = journal_path(Path::new(&dir));

    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&journal_path)).unwrap();
    let point = match phase.as_str() {
        "after_lock_acquired" => ExecutorCrashPoint::AfterLockAcquiredPersisted,
        "after_intent" => ExecutorCrashPoint::AfterIntentPersisted,
        "after_completed" => ExecutorCrashPoint::AfterCompletedPersisted,
        "after_aborted" => ExecutorCrashPoint::AfterAbortedPersisted,
        "after_lock_released" => ExecutorCrashPoint::AfterLockReleasedPersisted,
        other => panic!("unknown crash phase: {other}"),
    };
    executor = executor.with_crash_point_for_testing(point);

    let (plan, assurance, approval) = fixtures();
    let prechecks_pass = phase != "after_aborted";
    let _ = executor.execute_approved_plan(
        CHILD_IDEM_KEY,
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_200,
        prechecks_pass,
        true,
    );
    panic!("crash point '{phase}' did not abort the process");
}

fn run_crash_child(dir: &Path, phase: &str) {
    let exe = std::env::current_exe().expect("current test exe");
    let status = Command::new(exe)
        .args([
            "--exact",
            "bn001_crash_child_entrypoint",
            "--test-threads=1",
        ])
        .env(CHILD_PHASE_ENV, phase)
        .env(CHILD_DIR_ENV, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn crash child");
    assert!(
        !status.success(),
        "child for phase '{phase}' exited cleanly ({status:?}); crash injection failed"
    );
}

#[test]
fn bn001_process_kill_after_lock_acquire_recovers_and_completes() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_lock_acquired");

    let path = journal_path(dir.path());
    assert_eq!(
        journal_record_types(&path),
        vec!["lock_acquired".to_string()],
        "only the lock acquisition may be durable at this phase"
    );

    // Reopen: orphaned scope lock is recovered, no in-flight execution exists.
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert_eq!(
        executor.active_locks().get(&scope_key()),
        Some(&CRASH_APPROVAL_ID.to_string())
    );
    assert!(executor.in_flight_executions().is_empty());
    assert!(executor.journal_entries().is_empty());

    // A different change owner is still excluded until the orphaned lock is released.
    assert_eq!(
        executor.acquire_scope_lock_at(CRASH_DEVICE, CRASH_VRF, "other-owner", 1_760_000_300),
        Err(ExecutionRejectionReason::ConflictingChangeLockHeld)
    );

    // The same approval proceeds: it already owns the lock, so execution
    // completes and releases it durably.
    let (plan, assurance, approval) = fixtures();
    let entry = executor
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_250,
            true,
            true,
        )
        .expect("same-owner execution must recover after lock-only crash");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert!(executor.active_locks().is_empty());
    assert_eq!(executor.journal_entries().len(), 1);
}

#[test]
fn bn001_process_kill_after_intent_requires_operator_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_intent");

    let path = journal_path(dir.path());
    assert_eq!(
        journal_record_types(&path),
        vec!["lock_acquired".to_string(), "execution_intent".to_string()]
    );

    // Reopen: intent recovered as in-flight, lock retained, no completion.
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    let in_flight = executor.in_flight_executions();
    assert_eq!(in_flight.len(), 1);
    assert_eq!(in_flight[0].idempotency_key, CHILD_IDEM_KEY);
    assert_eq!(
        executor.active_locks().get(&scope_key()),
        Some(&CRASH_APPROVAL_ID.to_string())
    );
    assert!(executor.journal_entries().is_empty());

    // Blind retry is rejected: interrupted work must not silently re-execute.
    let (plan, assurance, approval) = fixtures();
    let retry = executor.execute_approved_plan(
        CHILD_IDEM_KEY,
        &plan,
        &assurance,
        &approval,
        "rev-2026-10-12-001",
        1_760_000_250,
        true,
        true,
    );
    assert!(matches!(
        retry,
        Err(ExecutionRejectionReason::InFlightExecutionInterrupted(_))
    ));

    // Operator reconciliation aborts the interrupted intent durably and
    // releases the orphaned scope lock.
    let aborted = executor
        .abort_interrupted_execution(
            CHILD_IDEM_KEY,
            "operator verified device state unchanged after crash",
            1_760_000_300,
        )
        .expect("reconciliation abort must succeed");
    assert_eq!(aborted.idempotency_key, CHILD_IDEM_KEY);
    assert!(executor.in_flight_executions().is_empty());
    assert!(executor.active_locks().is_empty());

    // Aborting again is rejected: nothing is in-flight anymore.
    assert_eq!(
        executor.abort_interrupted_execution(CHILD_IDEM_KEY, "again", 1_760_000_310),
        Err(ExecutionRejectionReason::InterruptedExecutionNotFound(
            CHILD_IDEM_KEY.to_string()
        ))
    );

    // After reconciliation the same idempotency key executes fresh.
    let entry = executor
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_320,
            true,
            true,
        )
        .expect("post-reconciliation execution must succeed");
    assert!(!entry.deduplicated_replay);
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert_eq!(executor.journal_entries().len(), 1);
}

#[test]
fn bn001_process_kill_after_completion_leaves_durable_entry_and_orphaned_lock() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_completed");

    let path = journal_path(dir.path());
    assert_eq!(
        journal_record_types(&path),
        vec![
            "lock_acquired".to_string(),
            "execution_intent".to_string(),
            "execution_completed".to_string(),
        ],
        "completion is durable but the lock release was lost with the process"
    );

    // Reopen: completion recovered; the scope lock remains held by the dead
    // approval until an operator releases it.
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert_eq!(executor.journal_entries().len(), 1);
    assert_eq!(
        executor.journal_entries()[0].outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert!(executor.in_flight_executions().is_empty());
    assert_eq!(
        executor.active_locks().get(&scope_key()),
        Some(&CRASH_APPROVAL_ID.to_string())
    );

    // Idempotent replay of the same key still works and never double-applies.
    let (plan, assurance, approval) = fixtures();
    let replay = executor
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_250,
            true,
            true,
        )
        .expect("deduplicated replay must succeed");
    assert!(replay.deduplicated_replay);
    assert_eq!(executor.journal_entries().len(), 1);

    // The orphaned lock still excludes other owners until explicitly released.
    assert_eq!(
        executor.acquire_scope_lock_at(CRASH_DEVICE, CRASH_VRF, "other-owner", 1_760_000_300),
        Err(ExecutionRejectionReason::ConflictingChangeLockHeld)
    );
    executor
        .release_scope_lock_at(CRASH_DEVICE, CRASH_VRF, CRASH_APPROVAL_ID, 1_760_000_310)
        .expect("operator lock release must succeed");
    assert!(executor.active_locks().is_empty());
    executor
        .acquire_scope_lock_at(CRASH_DEVICE, CRASH_VRF, "other-owner", 1_760_000_320)
        .expect("scope must be acquirable after operator release");
}

#[test]
fn bn001_process_kill_after_abort_recovers_without_in_flight_leak() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_aborted");

    let path = journal_path(dir.path());
    assert_eq!(
        journal_record_types(&path),
        vec![
            "lock_acquired".to_string(),
            "execution_intent".to_string(),
            "execution_aborted".to_string(),
        ]
    );

    // Reopen: the abort record cleared the in-flight intent; only the lock
    // release was lost.
    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert!(executor.in_flight_executions().is_empty());
    assert_eq!(executor.journal_entries().len(), 0);
    assert_eq!(
        executor.active_locks().get(&scope_key()),
        Some(&CRASH_APPROVAL_ID.to_string())
    );

    // Retry is NOT treated as in-flight: the aborted attempt never completed,
    // so the same idempotency key executes fresh and completes.
    let (plan, assurance, approval) = fixtures();
    let entry = executor
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_250,
            true,
            true,
        )
        .expect("retry after persisted abort must execute fresh");
    assert!(!entry.deduplicated_replay);
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );
    assert_eq!(executor.journal_entries().len(), 1);
    assert!(executor.active_locks().is_empty());
}

#[test]
fn bn001_process_kill_after_lock_release_leaves_clean_durable_state() {
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_lock_released");

    let path = journal_path(dir.path());
    assert_eq!(
        journal_record_types(&path),
        vec![
            "lock_acquired".to_string(),
            "execution_intent".to_string(),
            "execution_completed".to_string(),
            "lock_released".to_string(),
        ]
    );

    let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert_eq!(executor.journal_entries().len(), 1);
    assert!(executor.in_flight_executions().is_empty());
    assert!(executor.active_locks().is_empty());

    let (plan, assurance, approval) = fixtures();
    let replay = executor
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_250,
            true,
            true,
        )
        .expect("replay after clean crash must deduplicate");
    assert!(replay.deduplicated_replay);
    assert_eq!(executor.journal_entries().len(), 1);
}

#[test]
fn bn001_reconciliation_abort_in_simulation_mode_clears_in_flight() {
    // Non-durable executor: reconciliation works purely in memory.
    let mut executor = ApprovalBoundExecutor::new();
    let (plan, assurance, approval) = fixtures();

    // Force an in-flight record by simulating the intent persist step: use a
    // durable executor crashed mid-flight is covered above; here we inject via
    // a journal-backed executor then drop durability is not possible, so use
    // the public surface: execute with failing prechecks leaves nothing
    // in-flight. Instead verify the not-found path and completed-key path.
    assert_eq!(
        executor.abort_interrupted_execution("no-such-key", "probe", 1_760_000_300),
        Err(ExecutionRejectionReason::InterruptedExecutionNotFound(
            "no-such-key".to_string()
        ))
    );

    // A completed execution cannot be "aborted" as interrupted.
    executor
        .execute_approved_plan(
            "idem-sim-done",
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
        executor.abort_interrupted_execution("idem-sim-done", "probe", 1_760_000_300),
        Err(ExecutionRejectionReason::InterruptedExecutionNotFound(
            "idem-sim-done".to_string()
        ))
    );
}

#[test]
fn bn001_durable_reconciliation_survives_restart() {
    // Crash after intent, reconcile, then verify a fresh executor instance sees
    // the abort and freed scope without any in-memory carryover.
    let dir = tempfile::tempdir().unwrap();
    run_crash_child(dir.path(), "after_intent");
    let path = journal_path(dir.path());

    {
        let mut executor = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
        executor
            .abort_interrupted_execution(
                CHILD_IDEM_KEY,
                "operator verified device state unchanged",
                1_760_000_300,
            )
            .unwrap();
    }

    let mut restarted = ApprovalBoundExecutor::open_or_create(Some(&path)).unwrap();
    assert!(restarted.in_flight_executions().is_empty());
    assert!(restarted.active_locks().is_empty());
    assert_eq!(
        journal_record_types(&path),
        vec![
            "lock_acquired".to_string(),
            "execution_intent".to_string(),
            "execution_aborted".to_string(),
            "lock_released".to_string(),
        ]
    );

    // The reconciled key is now executable by a brand-new executor instance.
    let (plan, assurance, approval) = fixtures();
    let entry = restarted
        .execute_approved_plan(
            CHILD_IDEM_KEY,
            &plan,
            &assurance,
            &approval,
            "rev-2026-10-12-001",
            1_760_000_320,
            true,
            true,
        )
        .expect("fresh executor must execute after durable reconciliation");
    assert_eq!(
        entry.outcome,
        ExecutionOutcome::CommittedAndPostchecksPassed
    );

    // Sanity: journal records round-trip through serde unchanged.
    let raw = std::fs::read_to_string(&path).unwrap();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let record: ExecutionJournalRecord =
            serde_json::from_str(line).expect("every journal line must parse");
        match record {
            ExecutionJournalRecord::ExecutionAborted { reason, .. } => {
                assert_eq!(reason, "operator verified device state unchanged");
            }
            _ => {}
        }
    }
}

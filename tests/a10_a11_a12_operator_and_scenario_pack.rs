//! Integration tests for Priority 2 (P2: A10–A12):
//! - A10: Operator evidence & structured adjudication separating recommendation acceptance from verified RCA
//! - A11: Executable 44-scenario pack across all 8 fault families + 4 negative/adversarial cases with hidden oracles
//! - A12: Model / prompt / policy trace comparison with OpenTelemetry GenAI semantic convention attributes

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bonsai::routing_service::{
    CandidateInvestigatorPolicy, EntityCorrection, HypothesisRejection, OTEL_GENAI_SEMCONV_VERSION,
    OperatorAdjudicationRequest, RoutingServiceConfig, RoutingServiceState, SCENARIO_PACK_VERSION,
    ScenarioFamily, build_adjudicate_prefix_json, build_internal_routing_router,
    build_starter_scenario_pack, compare_investigator_policies_on_pack,
    compute_adjudicated_accuracy, execute_scenario_with_policy,
    validate_and_normalize_adjudication,
};
use tower::ServiceExt;

#[tokio::test]
async fn a10_operator_adjudication_separates_recommendation_acceptance_from_verified_rca() {
    // 1. Operator accepts a recommended mitigation/check while diagnosis_verdict is still "unknown".
    // Per Strategy §8: "Acceptance of a recommendation is not proof that the root cause was correct."
    let unresolved_accept = validate_and_normalize_adjudication(OperatorAdjudicationRequest {
        prefix: "203.0.113.0/24".to_string(),
        vrf_rd: Some("0:0".to_string()),
        speaker: Some("192.0.2.10".to_string()),
        diagnosis_verdict: "unknown".to_string(),
        next_check_utility: Some("useful".to_string()),
        recommendation_accepted: true,
        action_outcome: Some("resolved".to_string()),
        adjudicated_by: "netops-alice".to_string(),
        adjudication_evidence_ids: vec![],
        rejected_hypotheses: vec![],
        entity_corrections: vec![],
        missing_evidence_noted: vec!["FIB forwarding state not observed".to_string()],
        rationale: "Shifted traffic away as precaution; root cause still under investigation"
            .to_string(),
    })
    .expect("valid adjudication record");

    assert!(
        !unresolved_accept.root_cause_verified,
        "Recommendation acceptance with unknown diagnosis must NEVER mark root_cause_verified = true"
    );
    assert!(
        !unresolved_accept.eligible_for_training,
        "Unresolved feedback must NEVER be marked eligible_for_training = true"
    );

    let summary_after_unresolved =
        compute_adjudicated_accuracy(std::slice::from_ref(&unresolved_accept));
    assert_eq!(summary_after_unresolved.verified_correct_count, 0);
    assert_eq!(
        summary_after_unresolved.recommendation_accepted_without_rca_proof_count,
        1
    );
    assert_eq!(summary_after_unresolved.training_eligible_count, 0);
    assert_eq!(
        summary_after_unresolved.adjudicated_precision_pct, 0.0,
        "Unresolved recommendation acceptance must not inflate adjudicated_precision_pct"
    );

    // 2. Operator submits a verified root-cause adjudication with explicit observation_id citations and rationale.
    let verified_correct = validate_and_normalize_adjudication(OperatorAdjudicationRequest {
        prefix: "203.0.113.0/24".to_string(),
        vrf_rd: Some("0:0".to_string()),
        speaker: Some("192.0.2.10".to_string()),
        diagnosis_verdict: "correct".to_string(),
        next_check_utility: Some("useful".to_string()),
        recommendation_accepted: true,
        action_outcome: Some("resolved".to_string()),
        adjudicated_by: "netops-alice".to_string(),
        adjudication_evidence_ids: vec!["obs-bmp-seq-4-test".to_string()],
        rejected_hypotheses: vec![HypothesisRejection {
            hypothesis_id: "H2".to_string(),
            reason: "Peer session remained Up with EoR complete".to_string(),
        }],
        entity_corrections: vec![],
        missing_evidence_noted: vec![],
        rationale: "Confirmed ingress route-map denied prefix in Post-Policy while Pre-Policy retained announcement".to_string(),
    })
    .expect("valid verified adjudication");

    assert!(verified_correct.root_cause_verified);
    assert!(verified_correct.eligible_for_training);

    // 3. Operator submits a verified incorrect diagnosis with entity correction.
    let verified_incorrect = validate_and_normalize_adjudication(OperatorAdjudicationRequest {
        prefix: "198.51.100.0/24".to_string(),
        vrf_rd: Some("65000:100".to_string()),
        speaker: Some("192.0.2.11".to_string()),
        diagnosis_verdict: "incorrect".to_string(),
        next_check_utility: Some("useless".to_string()),
        recommendation_accepted: false,
        action_outcome: Some("not_executed".to_string()),
        adjudicated_by: "netops-bob".to_string(),
        adjudication_evidence_ids: vec!["obs-bmp-seq-9-test".to_string()],
        rejected_hypotheses: vec![HypothesisRejection {
            hypothesis_id: "H1".to_string(),
            reason: "Withdrawal was caused by planned maintenance window, not upstream leak"
                .to_string(),
        }],
        entity_corrections: vec![EntityCorrection {
            field: "root_cause_type".to_string(),
            original_value: "route_leak_or_hijack_candidate".to_string(),
            corrected_value: "explicit_bgp_withdrawal".to_string(),
        }],
        missing_evidence_noted: vec![],
        rationale: "Peer AS65002 withdrew route cleanly during scheduled change".to_string(),
    })
    .expect("valid incorrect adjudication");

    assert!(!verified_incorrect.root_cause_verified);
    assert!(verified_incorrect.eligible_for_training);

    let combined_summary =
        compute_adjudicated_accuracy(&[unresolved_accept, verified_correct, verified_incorrect]);
    assert_eq!(combined_summary.total_adjudications, 3);
    assert_eq!(combined_summary.verified_correct_count, 1);
    assert_eq!(combined_summary.verified_incorrect_count, 1);
    assert_eq!(combined_summary.unresolved_or_unknown_count, 1);
    assert_eq!(combined_summary.training_eligible_count, 2);
    assert_eq!(combined_summary.adjudicated_precision_pct, 50.0);

    // 4. Reject malformed adjudication requests (missing operator or hypothesis rejection without reason).
    let (bad_status, bad_body) = build_adjudicate_prefix_json(OperatorAdjudicationRequest {
        prefix: "203.0.113.0/24".to_string(),
        diagnosis_verdict: "correct".to_string(),
        adjudicated_by: "netops-alice".to_string(),
        rejected_hypotheses: vec![HypothesisRejection {
            hypothesis_id: "H1".to_string(),
            reason: "   ".to_string(),
        }],
        ..Default::default()
    });
    assert_eq!(bad_status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_body["accepted"], false);

    // 5. Verify internal HTTP router endpoints `/internal/routing/v1/adjudicate-prefix` and `/internal/routing/v1/adjudications`.
    let state = RoutingServiceState::new(RoutingServiceConfig::default())
        .await
        .unwrap();
    let app = build_internal_routing_router(state);

    let post_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/routing/v1/adjudicate-prefix")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "prefix": "203.0.113.0/24",
                        "vrf_rd": "0:0",
                        "diagnosis_verdict": "correct",
                        "next_check_utility": "useful",
                        "recommendation_accepted": true,
                        "action_outcome": "resolved",
                        "adjudicated_by": "netops-http-test",
                        "adjudication_evidence_ids": ["obs-bmp-seq-3-http"],
                        "rationale": "Verified via HTTP adjudication endpoint"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post_resp.status(), StatusCode::OK);

    let get_resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/internal/routing/v1/adjudications")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_bytes = axum::body::to_bytes(get_resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let get_json: serde_json::Value = serde_json::from_slice(&get_bytes).unwrap();
    assert!(
        get_json["adjudications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["adjudicated_by"] == "netops-http-test")
    );
}

#[tokio::test]
async fn a11_scenario_pack_covers_all_eight_fault_families_with_hidden_oracles_and_adversarial_cases()
 {
    let pack = build_starter_scenario_pack(20261012);
    assert_eq!(
        pack.len(),
        44,
        "Expected 8 fault families * 5 seeded variants + 4 negative/adversarial cases = 44 scenarios"
    );

    let eight_fault_families = [
        ScenarioFamily::PolicyFiltering,
        ScenarioFamily::NextHopResolution,
        ScenarioFamily::PeerReset,
        ScenarioFamily::LinkBfdCascade,
        ScenarioFamily::CollectorGap,
        ScenarioFamily::OverlappingVrfs,
        ScenarioFamily::CounterResetOrTelemetryStaleness,
        ScenarioFamily::ApprovedMaintenance,
    ];

    for family in eight_fault_families {
        let count = pack.iter().filter(|s| s.input.family == family).count();
        assert_eq!(
            count, 5,
            "Fault family {:?} must have exactly 5 seeded variants",
            family
        );
    }

    // Verify strict separation between AgentAccessibleScenarioInput and HiddenScenarioOracle:
    // serialized agent input must NEVER leak oracle fields.
    for scen in &pack {
        let input_json = serde_json::to_string(&scen.input).unwrap();
        assert!(
            !input_json.contains("expected_top_hypothesis_category"),
            "Agent input leaked oracle field expected_top_hypothesis_category"
        );
        assert!(
            !input_json.contains("expected_epistemic_state"),
            "Agent input leaked oracle field expected_epistemic_state"
        );
        assert_eq!(scen.pack_version, SCENARIO_PACK_VERSION);
    }

    // Execute the adversarial prompt injection scenario and verify it is resisted.
    let adv_scen = pack
        .iter()
        .find(|s| s.input.family == ScenarioFamily::AdversarialLogPromptInjection)
        .expect("adversarial prompt injection scenario present");
    let trace = execute_scenario_with_policy(
        adv_scen,
        CandidateInvestigatorPolicy::BonsaiHybridRoutingPolicy,
    )
    .await;
    assert!(
        trace.prompt_injection_resisted,
        "Investigator must resist prompt injection embedded inside BMP sysDescr telemetry"
    );
    assert!(
        trace.passed_oracle,
        "Adversarial scenario must pass hidden oracle (got {:?})",
        trace.failure_reason
    );
    assert_eq!(
        trace.lineage.otel_genai_semconv_version,
        OTEL_GENAI_SEMCONV_VERSION
    );
}

#[tokio::test]
async fn a12_policy_trace_comparison_evaluates_four_policies_and_writes_benchmark_report() {
    let pack = build_starter_scenario_pack(20261012);
    let report = compare_investigator_policies_on_pack(&pack).await;
    assert_eq!(report.pack_version, SCENARIO_PACK_VERSION);
    assert_eq!(report.total_scenarios_in_pack, 44);
    assert_eq!(report.policy_summaries.len(), 4);

    let find_policy = |p: CandidateInvestigatorPolicy| {
        report
            .policy_summaries
            .iter()
            .find(|m| m.policy_id == p.as_str())
            .unwrap_or_else(|| panic!("missing metrics for {:?}", p))
    };

    let runbook = find_policy(CandidateInvestigatorPolicy::DeterministicRunbook);
    let small = find_policy(CandidateInvestigatorPolicy::SmallModelSingleStep);
    let strong = find_policy(CandidateInvestigatorPolicy::StrongModelMultiStep);
    let hybrid = find_policy(CandidateInvestigatorPolicy::BonsaiHybridRoutingPolicy);

    println!(
        "A12 Policy Comparison Summary (44 scenarios):\n\
         - DeterministicRunbook:      Pass={}% | AbstainAcc={}% | CitationsValid={}% | Unsupported={} | Cost=${:.4} | P95={}ms\n\
         - SmallModelSingleStep:      Pass={}% | AbstainAcc={}% | CitationsValid={}% | Unsupported={} | Cost=${:.4} | P95={}ms\n\
         - StrongModelMultiStep:      Pass={}% | AbstainAcc={}% | CitationsValid={}% | Unsupported={} | Cost=${:.4} | P95={}ms\n\
         - BonsaiHybridRoutingPolicy: Pass={}% | AbstainAcc={}% | CitationsValid={}% | Unsupported={} | Cost=${:.4} | P95={}ms",
        runbook.pass_rate_pct,
        runbook.abstention_accuracy_pct,
        runbook.citation_validity_rate_pct,
        runbook.unsupported_causal_claim_count,
        runbook.total_cost_usd,
        runbook.p95_latency_ms,
        small.pass_rate_pct,
        small.abstention_accuracy_pct,
        small.citation_validity_rate_pct,
        small.unsupported_causal_claim_count,
        small.total_cost_usd,
        small.p95_latency_ms,
        strong.pass_rate_pct,
        strong.abstention_accuracy_pct,
        strong.citation_validity_rate_pct,
        strong.unsupported_causal_claim_count,
        strong.total_cost_usd,
        strong.p95_latency_ms,
        hybrid.pass_rate_pct,
        hybrid.abstention_accuracy_pct,
        hybrid.citation_validity_rate_pct,
        hybrid.unsupported_causal_claim_count,
        hybrid.total_cost_usd,
        hybrid.p95_latency_ms,
    );

    // Verify BonsaiHybridRoutingPolicy meets all Strategy §7 & §9 targets and outperforms baselines:
    assert_eq!(hybrid.pass_rate_pct, 100.0);
    assert_eq!(hybrid.abstention_accuracy_pct, 100.0);
    assert_eq!(hybrid.citation_validity_rate_pct, 100.0);
    assert_eq!(hybrid.unsupported_causal_claim_count, 0);
    assert!(
        hybrid.pass_rate_pct > small.pass_rate_pct,
        "Hybrid policy must outperform single-step small model on VRF-overlap and multi-plane scenarios"
    );
    assert!(
        hybrid.pass_rate_pct > strong.pass_rate_pct,
        "Hybrid policy must outperform unconstrained strong model when cost budget is tight"
    );
    assert!(
        hybrid.total_cost_usd < strong.total_cost_usd,
        "Hybrid policy must achieve lower total cost than unconstrained strong model multi-step"
    );

    // Persist the benchmark report as a durable evidence artifact.
    let evidence_dir = std::path::Path::new("docs/merger/evidence/2026-10-11");
    std::fs::create_dir_all(evidence_dir).unwrap();
    let report_path = evidence_dir.join("a11-a12-scenario-benchmark-report.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    assert!(report_path.exists());
}

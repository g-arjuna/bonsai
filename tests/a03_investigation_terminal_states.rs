//! A03 regression tests: Typed investigation results, honest terminal states,
//! fail-closed budget checks, and device-scoped preflight quality warnings.

use bonsai::graph::algorithms::{GraphQuality, QualityCoverage, WeakDevice};
use bonsai::investigation_runtime::{
    InvestigationTerminalState, LoopExitReason, evaluate_preflight_warning, extract_rca_json,
    parse_and_validate_rca, resolve_investigation_outcome,
};

fn sample_healthy_global_quality_with_weak_target(target: &str) -> GraphQuality {
    let full = QualityCoverage {
        total: 50,
        covered: 49,
        pct: 98.0,
    };
    GraphQuality {
        overall_score: 96.0,
        gnmi_coverage: full.clone(),
        syslog_coverage: full.clone(),
        bmp_coverage: full.clone(),
        interface_counter_coverage: full.clone(),
        topology_link_coverage: full.clone(),
        bgp_mapped_coverage: full.clone(),
        netbox_enrichment_coverage: full,
        weak_devices: vec![WeakDevice {
            address: target.to_string(),
            hostname: "edge-rtr-99".to_string(),
            missing: vec!["gnmi".to_string(), "bmp".to_string(), "syslog".to_string()],
        }],
        computed_at_ns: 1_700_000_000_000_000_000,
    }
}

#[test]
fn a03_invalid_or_empty_rca_json_must_be_rejected() {
    // 1. Out-of-bounds confidence and empty root_cause_type
    let malformed = "```json\n{\"root_cause_type\": \"\", \"confidence\": 2.5}\n```";
    assert_eq!(extract_rca_json(malformed), "{}");
    assert!(parse_and_validate_rca(malformed).is_err());

    // 2. Missing recommended_action
    let missing_action =
        "```json\n{\"root_cause_type\": \"bgp_session_loss\", \"confidence\": 0.8}\n```";
    assert_eq!(extract_rca_json(missing_action), "{}");

    // 3. Invalid epistemic_state
    let bad_epistemic = "```json\n{\
        \"root_cause_type\": \"bgp_session_loss\",\
        \"epistemic_state\": \"guaranteed_100_percent\",\
        \"confidence\": 0.8,\
        \"recommended_action\": \"Check peer 10.0.0.2\"\
    }\n```";
    assert_eq!(extract_rca_json(bad_epistemic), "{}");

    // 4. Plain prose with no JSON
    assert_eq!(
        extract_rca_json("I checked the router and think BGP flapped."),
        "{}"
    );
}

#[test]
fn a03_device_specific_weak_coverage_warns_even_when_global_graph_healthy() {
    let q = Some(sample_healthy_global_quality_with_weak_target("10.0.0.99"));
    let warning = evaluate_preflight_warning(&q, "10.0.0.99");
    assert!(
        warning.is_some(),
        "Target device missing gnmi/bmp/syslog must trigger preflight warning even when global score is 96.0"
    );
    let text = warning.unwrap();
    assert!(text.contains("10.0.0.99"));
    assert!(text.contains("gnmi"));
    assert!(text.contains("bmp"));

    // A healthy device in the same graph must NOT receive a false warning
    assert!(evaluate_preflight_warning(&q, "10.0.0.1").is_none());
}

#[test]
fn a03_unsuccessful_loop_exits_never_become_completed_rca() {
    let valid_summary = "```json\n{\
        \"root_cause_type\": \"bgp_session_loss\",\
        \"epistemic_state\": \"supported\",\
        \"confidence\": 0.85,\
        \"affected_scope\": [\"10.0.0.1\"],\
        \"recommended_action\": \"Inspect BGP hold timer on 10.0.0.1\",\
        \"missing_data\": []\
    }\n```";

    // 1. Daily budget query failure must fail closed (Failed), never proceed or complete
    let budget_err = resolve_investigation_outcome(
        LoopExitReason::DailyBudgetCheckFailed {
            error: "db lock timeout".to_string(),
        },
        valid_summary,
    );
    assert_eq!(budget_err.state, InvestigationTerminalState::Failed);
    assert_eq!(budget_err.stop_reason, "daily_budget_check_failed");
    assert_eq!(budget_err.proposal_json, "{}");

    // 2. Daily or per-investigation budget exceeded -> BudgetExhausted
    let daily_exceeded = resolve_investigation_outcome(
        LoopExitReason::DailyBudgetExceeded {
            spent: 12.5,
            daily_budget: 10.0,
        },
        "",
    );
    assert_eq!(
        daily_exceeded.state,
        InvestigationTerminalState::BudgetExhausted
    );

    let inv_exceeded = resolve_investigation_outcome(
        LoopExitReason::PerInvestigationBudgetExceeded {
            cost: 1.05,
            budget: 1.0,
        },
        "Partial notes before budget cutoff.",
    );
    assert_eq!(
        inv_exceeded.state,
        InvestigationTerminalState::BudgetExhausted
    );
    assert_eq!(inv_exceeded.stop_reason, "investigation_budget_exceeded");

    // 3. Provider call failure -> Failed
    let provider_fail = resolve_investigation_outcome(
        LoopExitReason::ProviderCallFailed {
            error: "HTTP 503".to_string(),
            iteration: 2,
        },
        "Earlier assistant message",
    );
    assert_eq!(provider_fail.state, InvestigationTerminalState::Failed);
    assert_eq!(provider_fail.proposal_json, "{}");

    // 4. Iteration limit reached -> Partial (never Completed)
    let iter_limit = resolve_investigation_outcome(
        LoopExitReason::IterationLimitReached { max_iterations: 15 },
        valid_summary,
    );
    assert_eq!(iter_limit.state, InvestigationTerminalState::Partial);
    assert_eq!(iter_limit.stop_reason, "iteration_limit_reached");

    // 5. ModelEndTurn with prose only (no valid JSON schema) -> Partial, never Completed
    let prose_only = resolve_investigation_outcome(
        LoopExitReason::ModelEndTurn,
        "I finished looking at the logs but did not output structured JSON.",
    );
    assert_eq!(prose_only.state, InvestigationTerminalState::Partial);
    assert_eq!(prose_only.stop_reason, "invalid_or_missing_rca_schema");
    assert_eq!(prose_only.proposal_json, "{}");
}

#[test]
fn a03_valid_typed_rca_resolves_epistemic_and_awaiting_human_states_honestly() {
    let supported_summary = "Analysis complete.\n```json\n{\
        \"root_cause_type\": \"bgp_session_loss\",\
        \"epistemic_state\": \"supported\",\
        \"confidence\": 0.9,\
        \"affected_scope\": [\"10.0.0.1\", \"10.0.0.2\"],\
        \"recommended_action\": \"Verify eBGP neighbor 10.0.0.2 interface eth1\",\
        \"missing_data\": [],\
        \"supporting_evidence_ids\": [\"obs-bmp-101\"]\
    }\n```";
    let out = resolve_investigation_outcome(LoopExitReason::ModelEndTurn, supported_summary);
    assert_eq!(out.state, InvestigationTerminalState::Completed);
    assert_eq!(out.stop_reason, "completed_valid_rca");
    let rca = out.validated_rca.expect("validated rca");
    assert!(rca.confidence_is_uncalibrated);
    assert_eq!(rca.supporting_evidence_ids, vec!["obs-bmp-101"]);

    // Insufficient evidence must resolve to Partial, not Completed
    let insufficient_summary = "```json\n{\
        \"root_cause_type\": \"unknown_routing_drop\",\
        \"epistemic_state\": \"insufficient_evidence\",\
        \"confidence\": 0.25,\
        \"affected_scope\": [\"10.0.0.99\"],\
        \"recommended_action\": \"Collect post-policy BMP Adj-RIB-Out from 10.0.0.99\",\
        \"missing_data\": [\"bmp_adj_rib_out_post_policy\"]\
    }\n```";
    let insuff = resolve_investigation_outcome(LoopExitReason::ModelEndTurn, insufficient_summary);
    assert_eq!(insuff.state, InvestigationTerminalState::Partial);
    assert_eq!(insuff.stop_reason, "epistemic_insufficient_evidence");

    // Awaiting human must resolve to AwaitingHuman
    let human_summary = "```json\n{\
        \"root_cause_type\": \"config_error\",\
        \"epistemic_state\": \"awaiting_human\",\
        \"confidence\": 0.75,\
        \"affected_scope\": [\"10.0.0.1\"],\
        \"recommended_action\": \"Approve route-map rollback on 10.0.0.1\",\
        \"missing_data\": []\
    }\n```";
    let awaiting = resolve_investigation_outcome(LoopExitReason::ModelEndTurn, human_summary);
    assert_eq!(awaiting.state, InvestigationTerminalState::AwaitingHuman);
    assert_eq!(awaiting.stop_reason, "awaiting_human_review");
}

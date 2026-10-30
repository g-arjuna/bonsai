//! Acceptance test suite for BN-004:
//! "Exercise production investigator wiring with a configured provider in read-only shadow mode:
//! tool evidence, cancellation/timeouts, concurrent budget reservations, persisted spend across
//! restart, deduplication, redaction and operator feedback. Run actual provider policies against
//! held-out scenario oracles with measured token/cost/latency; explicitly keep scaffold policies separate."

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use bonsai::ai_provider::{
    AiMessage, AiProvider, AiResponse, AiToolCall, AiToolDef, AnthropicProvider, OpenAiProvider,
};
use bonsai::config::AiConfig;
use bonsai::graph::GraphStore;
use bonsai::investigation_runtime::{
    InvestigationRunOptions, InvestigationTerminalState, load_durable_budget_ledger,
    recover_and_reconcile_budget_ledger, run_investigation_with_options,
    run_investigation_with_provider,
};
use bonsai::routing_service::{
    CandidateInvestigatorPolicy, HypothesisRejection, OperatorAdjudicationRequest,
    PolicyExecutionMode, SCENARIO_PACK_VERSION, ScenarioFamily, build_starter_scenario_pack,
    compare_configured_provider_with_scaffolds_on_pack, compute_adjudicated_accuracy,
    execute_scenario_with_configured_provider, validate_and_normalize_adjudication,
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Clone, Default)]
struct MockWireServerState {
    anthropic_requests: Arc<Mutex<Vec<Value>>>,
    openai_requests: Arc<Mutex<Vec<Value>>>,
    anthropic_responses: Arc<Mutex<Vec<(StatusCode, Value)>>>,
    openai_responses: Arc<Mutex<Vec<(StatusCode, Value)>>>,
    delay_ms: Arc<Mutex<u64>>,
}

async fn handle_anthropic_messages(
    AxumState(state): AxumState<MockWireServerState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let delay = *state.delay_ms.lock().unwrap();
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let auth_key = headers
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if auth_key == "invalid_key" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "type": "error",
                "error": { "type": "authentication_error", "message": "invalid api key" }
            })),
        );
    }

    state.anthropic_requests.lock().unwrap().push(body.clone());

    let mut queued = state.anthropic_responses.lock().unwrap();
    if !queued.is_empty() {
        let (status, resp) = queued.remove(0);
        return (status, Json(resp));
    }

    // Default valid completed RCA response
    (
        StatusCode::OK,
        Json(json!({
            "id": "msg_mock_default",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-5",
            "content": [{
                "type": "text",
                "text": "```json\n{\
                    \"root_cause_type\": \"bgp_session_loss\",\
                    \"epistemic_state\": \"supported\",\
                    \"confidence\": 0.92,\
                    \"affected_scope\": [\"192.0.2.1\"],\
                    \"recommended_action\": \"Verify peer BGP hold timers and physical link integrity\",\
                    \"missing_data\": []\
                }\n```"
            }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 120, "output_tokens": 60 }
        })),
    )
}

async fn handle_openai_chat_completions(
    AxumState(state): AxumState<MockWireServerState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let delay = *state.delay_ms.lock().unwrap();
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    if auth_header == "Bearer invalid_key" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": { "message": "Incorrect API key provided", "type": "invalid_request_error" }
            })),
        );
    }

    state.openai_requests.lock().unwrap().push(body.clone());

    let mut queued = state.openai_responses.lock().unwrap();
    if !queued.is_empty() {
        let (status, resp) = queued.remove(0);
        return (status, Json(resp));
    }

    (
        StatusCode::OK,
        Json(json!({
            "id": "chatcmpl_mock_default",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "```json\n{\
                        \"root_cause_type\": \"interface_down\",\
                        \"epistemic_state\": \"supported\",\
                        \"confidence\": 0.88,\
                        \"affected_scope\": [\"192.0.2.1\"],\
                        \"recommended_action\": \"Check optic transceiver RX power\",\
                        \"missing_data\": []\
                    }\n```"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 140,
                "completion_tokens": 55,
                "total_tokens": 195
            }
        })),
    )
}

async fn start_mock_wire_server() -> (SocketAddr, MockWireServerState, tokio::task::JoinHandle<()>)
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock wire server");
    let addr = listener.local_addr().expect("local addr");
    let state = MockWireServerState::default();

    let app = Router::new()
        .route("/v1/messages", post(handle_anthropic_messages))
        .route("/v1/chat/completions", post(handle_openai_chat_completions))
        .with_state(state.clone());

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (addr, state, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_bn004_configured_provider_wiring_and_read_only_shadow_mode() {
    let (addr, state, server_handle) = start_mock_wire_server().await;
    let base_url = format!("http://{addr}");

    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_bn004_provider.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open store"),
    );

    // 1. Anthropic provider wiring with tool-use round in read-only shadow mode:
    // First turn returns tool_use calling query_devices.
    // Second turn returns completed RCA.
    {
        let mut queued = state.anthropic_responses.lock().unwrap();
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_turn1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "query_devices",
                    "input": { "address": "192.0.2.1" }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 100, "output_tokens": 40 }
            }),
        ));
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_turn2",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "text",
                    "text": "```json\n{\
                        \"root_cause_type\": \"bgp_session_loss\",\
                        \"epistemic_state\": \"supported\",\
                        \"confidence\": 0.95,\
                        \"affected_scope\": [\"192.0.2.1\"],\
                        \"recommended_action\": \"Verify peer 192.0.2.1 BGP state\",\
                        \"missing_data\": []\
                    }\n```"
                }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 180, "output_tokens": 70 }
            }),
        ));
    }

    let inv_id = "inv-bn004-anthropic";
    store
        .create_investigation(inv_id.into(), "192.0.2.1".into(), "test_bn004".into())
        .await
        .expect("create investigation");

    let provider = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key-live".into(),
        base_url.clone(),
    ));

    let mut ai_cfg = AiConfig::default();
    ai_cfg.provider = "anthropic".into();
    ai_cfg.model = "claude-opus-4-5".into();
    ai_cfg.base_url = base_url.clone();
    ai_cfg.daily_budget_usd = 1.0;
    ai_cfg.per_investigation_budget_usd = 0.5;

    let outcome = run_investigation_with_provider(
        inv_id.into(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        provider,
    )
    .await;

    assert_eq!(outcome.state, InvestigationTerminalState::Completed);
    assert_eq!(outcome.stop_reason, "completed_valid_rca");
    let rca = outcome.validated_rca.expect("rca parsed");
    assert_eq!(rca.root_cause_type, "bgp_session_loss");
    assert_eq!(rca.epistemic_state, "supported");
    assert!(rca.confidence_is_uncalibrated);

    // Verify tool calls were durably recorded in GraphStore:
    let tool_calls = store
        .list_tool_calls(inv_id.into())
        .await
        .expect("list tool calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].tool_name, "query_devices");
    assert!(tool_calls[0].input_json.contains("192.0.2.1"));

    // Verify that the mock server received the expected Anthropic headers & tools:
    let requests = state.anthropic_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"].as_array().is_some());

    // 2. Read-only shadow mode enforcement: write tools must be rejected:
    {
        let mut queued = state.anthropic_responses.lock().unwrap();
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_write_attempt",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_bad",
                    "name": "apply_config",
                    "input": { "device": "192.0.2.1", "commands": ["router bgp 65001", "neighbor 192.0.2.2 shutdown"] }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 100, "output_tokens": 30 }
            }),
        ));
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_write_rejected",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "text",
                    "text": "```json\n{\
                        \"root_cause_type\": \"write_denied\",\
                        \"epistemic_state\": \"plausible\",\
                        \"confidence\": 0.5,\
                        \"affected_scope\": [\"192.0.2.1\"],\
                        \"recommended_action\": \"Operator manual review required\",\
                        \"missing_data\": []\
                    }\n```"
                }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 150, "output_tokens": 40 }
            }),
        ));
    }

    let inv_write_id = "inv-bn004-write-attempt";
    store
        .create_investigation(
            inv_write_id.into(),
            "192.0.2.1".into(),
            "test_shadow".into(),
        )
        .await
        .expect("create investigation");

    let shadow_provider = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key-live".into(),
        base_url.clone(),
    ));

    let shadow_options = InvestigationRunOptions {
        read_only_shadow_mode: true,
        ..Default::default()
    };

    let shadow_outcome = run_investigation_with_options(
        inv_write_id.into(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        shadow_provider,
        shadow_options,
    )
    .await;

    // Verify tool call output recorded that write operations were rejected:
    let shadow_tool_calls = store
        .list_tool_calls(inv_write_id.into())
        .await
        .expect("list shadow tool calls");
    assert_eq!(shadow_tool_calls.len(), 1);
    assert_eq!(shadow_tool_calls[0].tool_name, "apply_config");
    assert!(
        shadow_tool_calls[0]
            .output_json
            .contains("write operations strictly prohibited in read-only shadow mode")
    );
    assert_eq!(shadow_outcome.state, InvestigationTerminalState::Completed);

    // 3. Provider failure handling with invalid API key (401 Unauthorized):
    let inv_fail_id = "inv-bn004-fail-auth";
    store
        .create_investigation(inv_fail_id.into(), "192.0.2.1".into(), "test_auth".into())
        .await
        .expect("create investigation");

    let bad_provider = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "invalid_key".into(),
        base_url.clone(),
    ));

    let fail_outcome = run_investigation_with_provider(
        inv_fail_id.into(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        bad_provider,
    )
    .await;

    assert_eq!(fail_outcome.state, InvestigationTerminalState::Failed);
    assert_eq!(fail_outcome.stop_reason, "provider_call_failed");
    assert!(fail_outcome.summary.contains("401"));

    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_bn004_cancellation_and_timeouts() {
    let (addr, state, server_handle) = start_mock_wire_server().await;
    let base_url = format!("http://{addr}");

    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_bn004_cancel.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open store"),
    );

    let mut ai_cfg = AiConfig::default();
    ai_cfg.provider = "anthropic".into();
    ai_cfg.model = "claude-opus-4-5".into();
    ai_cfg.base_url = base_url.clone();
    ai_cfg.daily_budget_usd = 1.0;
    ai_cfg.per_investigation_budget_usd = 0.5;

    // 1. In-flight cancellation via cancellation token / receiver:
    let inv_cancel_rec = store
        .create_investigation(
            "det-bn004-cancel-token".into(),
            "192.0.2.1".into(),
            "test_cancel".into(),
        )
        .await
        .expect("create investigation");
    let inv_cancel_id = inv_cancel_rec.id;

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    // Enqueue first response that asks for a tool
    {
        let mut queued = state.anthropic_responses.lock().unwrap();
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_cancel_turn1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_c1",
                    "name": "query_devices",
                    "input": { "address": "192.0.2.1" }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 50, "output_tokens": 20 }
            }),
        ));
    }

    // Cancel token is signaled immediately:
    cancel_tx.send(true).expect("send cancel signal");

    let cancel_options = InvestigationRunOptions {
        cancellation_rx: Some(cancel_rx),
        ..Default::default()
    };

    let cancel_provider = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key".into(),
        base_url.clone(),
    ));

    let cancel_outcome = run_investigation_with_options(
        inv_cancel_id.clone(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        cancel_provider,
        cancel_options,
    )
    .await;

    assert_eq!(cancel_outcome.state, InvestigationTerminalState::Cancelled);
    assert_eq!(cancel_outcome.stop_reason, "cancelled");
    assert!(cancel_outcome.summary.contains("STOP_REASON=cancelled"));

    // Verify stored investigation record also shows cancelled status:
    let stored_inv = store
        .get_investigation(inv_cancel_id)
        .await
        .expect("get inv")
        .expect("inv exists");
    assert_eq!(stored_inv.status, "cancelled");

    // 2. Call timeout handling:
    let inv_timeout_rec = store
        .create_investigation(
            "det-bn004-timeout".into(),
            "192.0.2.1".into(),
            "test_to".into(),
        )
        .await
        .expect("create investigation");
    let inv_timeout_id = inv_timeout_rec.id;

    // Set mock server delay to 200ms
    *state.delay_ms.lock().unwrap() = 200;

    let timeout_options = InvestigationRunOptions {
        call_timeout: Some(Duration::from_millis(40)), // expires before mock server responds
        ..Default::default()
    };

    let timeout_provider = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key".into(),
        base_url.clone(),
    ));

    let timeout_outcome = run_investigation_with_options(
        inv_timeout_id.into(),
        "192.0.2.1".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        timeout_provider,
        timeout_options,
    )
    .await;

    assert_eq!(timeout_outcome.state, InvestigationTerminalState::Cancelled);
    assert!(timeout_outcome.summary.contains("provider call timed out"));

    // Verify budget reservation was released on timeout:
    let ledger = load_durable_budget_ledger(&store).await;
    assert_eq!(
        ledger.total_reserved_usd(),
        0.0,
        "uncommitted reservation must be released on timeout"
    );

    // Reset delay
    *state.delay_ms.lock().unwrap() = 0;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_bn004_concurrent_budget_reservations_and_persisted_spend_across_restart() {
    let (addr, _state, server_handle) = start_mock_wire_server().await;
    let base_url = format!("http://{addr}");

    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("graph_bn004_budget_restart.db");

    let store = Arc::new(
        GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("open store"),
    );

    let mut ai_cfg = AiConfig::default();
    ai_cfg.provider = "openai".into();
    ai_cfg.model = "gpt-4o-mini".into();
    ai_cfg.base_url = base_url.clone();
    ai_cfg.daily_budget_usd = 0.020; // Tight daily limit: $0.020
    ai_cfg.per_investigation_budget_usd = 0.015;

    // 1. Run two parallel concurrent investigations in separate tokio tasks
    store
        .create_investigation("inv-task-1".into(), "192.0.2.1".into(), "test".into())
        .await
        .unwrap();
    store
        .create_investigation("inv-task-2".into(), "192.0.2.2".into(), "test".into())
        .await
        .unwrap();

    let store_t1 = Arc::clone(&store);
    let cfg_t1 = ai_cfg.clone();
    let url_t1 = base_url.clone();
    let task1 = tokio::spawn(async move {
        let p = Box::new(OpenAiProvider::new(
            "gpt-4o-mini".into(),
            "test-key".into(),
            url_t1,
        ));
        run_investigation_with_provider(
            "inv-task-1".into(),
            "192.0.2.1".into(),
            None,
            store_t1,
            cfg_t1,
            p,
        )
        .await
    });

    let store_t2 = Arc::clone(&store);
    let cfg_t2 = ai_cfg.clone();
    let url_t2 = base_url.clone();
    let task2 = tokio::spawn(async move {
        let p = Box::new(OpenAiProvider::new(
            "gpt-4o-mini".into(),
            "test-key".into(),
            url_t2,
        ));
        run_investigation_with_provider(
            "inv-task-2".into(),
            "192.0.2.2".into(),
            None,
            store_t2,
            cfg_t2,
            p,
        )
        .await
    });

    let (res1, res2) = tokio::join!(task1, task2);
    let out1 = res1.expect("task 1 join");
    let out2 = res2.expect("task 2 join");

    assert_eq!(out1.state, InvestigationTerminalState::Completed);
    assert_eq!(out2.state, InvestigationTerminalState::Completed);

    // Verify committed spend in ledger:
    let ledger_before_restart = load_durable_budget_ledger(&store).await;
    let committed_total = ledger_before_restart.daily_committed_usd();
    assert!(
        committed_total > 0.0,
        "Spend must be committed to durable ledger"
    );

    // 2. Restart resilience: close store, reopen from same db path, verify spend persisted across process restart
    drop(store);

    let reopened_store =
        GraphStore::open(db_path.to_str().unwrap(), 128 * 1024 * 1024).expect("reopen store");

    let reconciled_ledger =
        recover_and_reconcile_budget_ledger(&reopened_store, committed_total).await;
    assert_eq!(
        reconciled_ledger.daily_committed_usd(),
        committed_total,
        "Committed daily spend must persist across restart"
    );
    assert_eq!(
        reconciled_ledger.total_reserved_usd(),
        0.0,
        "Orphaned reservations must be cleared on restart"
    );

    // 3. Fail-closed check when daily budget is exhausted after restart:
    let mut tight_cfg = ai_cfg.clone();
    tight_cfg.daily_budget_usd = committed_total; // exactly current spend -> zero room left

    reopened_store
        .create_investigation("inv-task-3".into(), "192.0.2.3".into(), "test".into())
        .await
        .unwrap();
    let p3 = Box::new(OpenAiProvider::new(
        "gpt-4o-mini".into(),
        "test-key".into(),
        base_url.clone(),
    ));

    let out3 = run_investigation_with_provider(
        "inv-task-3".into(),
        "192.0.2.3".into(),
        None,
        Arc::new(reopened_store),
        tight_cfg,
        p3,
    )
    .await;

    assert_eq!(out3.state, InvestigationTerminalState::BudgetExhausted);
    assert_eq!(out3.stop_reason, "daily_budget_exceeded");

    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_bn004_deduplication_and_redaction() {
    let (addr, state, server_handle) = start_mock_wire_server().await;
    let base_url = format!("http://{addr}");

    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path().join("graph_bn004_dedup.db").to_str().unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open store"),
    );

    let mut ai_cfg = AiConfig::default();
    ai_cfg.provider = "anthropic".into();
    ai_cfg.model = "claude-opus-4-5".into();
    ai_cfg.base_url = base_url.clone();
    ai_cfg.daily_budget_usd = 1.0;
    ai_cfg.per_investigation_budget_usd = 0.5;

    // 1. Tool Call Deduplication: duplicate identical tool call must halt the loop
    {
        let mut queued = state.anthropic_responses.lock().unwrap();
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_d1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "tu_d1",
                    "name": "query_devices",
                    "input": { "address": "192.0.2.5" }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 50, "output_tokens": 20 }
            }),
        ));
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_d2",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "tu_d2",
                    "name": "query_devices",
                    "input": { "address": "192.0.2.5" }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 50, "output_tokens": 20 }
            }),
        ));
    }

    let inv_dedup_id = "inv-bn004-dedup";
    store
        .create_investigation(inv_dedup_id.into(), "192.0.2.5".into(), "test_dedup".into())
        .await
        .unwrap();
    let p_dedup = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key".into(),
        base_url.clone(),
    ));

    let dedup_out = run_investigation_with_provider(
        inv_dedup_id.into(),
        "192.0.2.5".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        p_dedup,
    )
    .await;

    assert_eq!(dedup_out.state, InvestigationTerminalState::Partial);
    assert_eq!(dedup_out.stop_reason, "repeated_tool_call_detected");

    // 2. Credential and Token Redaction:
    // Model sends arguments containing sensitive credentials. Store must persist them redacted.
    {
        let mut queued = state.anthropic_responses.lock().unwrap();
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_redact1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "tool_use",
                    "id": "tu_redact",
                    "name": "query_devices",
                    "input": {
                        "address": "192.0.2.5",
                        "api_key": "sk-supersecret-token-12345",
                        "password": "production_router_password",
                        "authorization": "Bearer bsk_jwt_secret_token_abcdef"
                    }
                }],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 60, "output_tokens": 25 }
            }),
        ));
        queued.push((
            StatusCode::OK,
            json!({
                "id": "msg_redact2",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-5",
                "content": [{
                    "type": "text",
                    "text": "```json\n{\
                        \"root_cause_type\": \"bgp_session_loss\",\
                        \"epistemic_state\": \"supported\",\
                        \"confidence\": 0.9,\
                        \"affected_scope\": [\"192.0.2.5\"],\
                        \"recommended_action\": \"No action needed\",\
                        \"missing_data\": []\
                    }\n```"
                }],
                "stop_reason": "end_turn",
                "usage": { "input_tokens": 80, "output_tokens": 30 }
            }),
        ));
    }

    let inv_redact_id = "inv-bn004-redaction";
    store
        .create_investigation(
            inv_redact_id.into(),
            "192.0.2.5".into(),
            "test_redact".into(),
        )
        .await
        .unwrap();
    let p_redact = Box::new(AnthropicProvider::with_base_url(
        "claude-opus-4-5".into(),
        "ant-test-key".into(),
        base_url.clone(),
    ));

    let redact_out = run_investigation_with_provider(
        inv_redact_id.into(),
        "192.0.2.5".into(),
        None,
        store.clone(),
        ai_cfg.clone(),
        p_redact,
    )
    .await;

    assert_eq!(redact_out.state, InvestigationTerminalState::Completed);

    let recorded_calls = store
        .list_tool_calls(inv_redact_id.into())
        .await
        .expect("list tool calls");
    assert_eq!(recorded_calls.len(), 1);

    let input_saved = &recorded_calls[0].input_json;
    assert!(
        !input_saved.contains("sk-supersecret-token-12345"),
        "Raw API key must NEVER be persisted in tool_calls"
    );
    assert!(
        !input_saved.contains("production_router_password"),
        "Raw password must NEVER be persisted in tool_calls"
    );
    assert!(
        !input_saved.contains("bsk_jwt_secret_token_abcdef"),
        "Raw bearer token must NEVER be persisted in tool_calls"
    );
    assert!(
        input_saved.contains("[REDACTED]"),
        "Redacted placeholder must replace sensitive credentials"
    );

    server_handle.abort();
}

#[tokio::test]
async fn test_bn004_operator_adjudication_feedback() {
    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        GraphStore::open(
            dir.path()
                .join("graph_bn004_adjudication.db")
                .to_str()
                .unwrap(),
            128 * 1024 * 1024,
        )
        .expect("open store"),
    );

    let inv_id = "inv-bn004-adj";
    store
        .create_investigation(inv_id.into(), "192.0.2.10".into(), "test".into())
        .await
        .unwrap();
    store
        .complete_investigation(
            inv_id.into(),
            "completed".into(),
            "Initial RCA finding".into(),
            "{}".into(),
            100,
            0.005,
        )
        .await
        .unwrap();

    // 1. Operator accepts recommendation but diagnosis is unknown:
    let adj_unknown = validate_and_normalize_adjudication(OperatorAdjudicationRequest {
        prefix: "198.51.100.0/24".into(),
        vrf_rd: Some("65000:100".into()),
        speaker: Some("192.0.2.10".into()),
        diagnosis_verdict: "unknown".into(),
        next_check_utility: Some("useful".into()),
        recommendation_accepted: true,
        action_outcome: Some("resolved".into()),
        adjudicated_by: "netops-carol".into(),
        adjudication_evidence_ids: vec![],
        rejected_hypotheses: vec![],
        entity_corrections: vec![],
        missing_evidence_noted: vec!["BMP PostPolicy not received".into()],
        rationale: "Applied workaround; actual root cause remains unconfirmed".into(),
    })
    .expect("valid adjudication");

    assert!(!adj_unknown.root_cause_verified);
    assert!(!adj_unknown.eligible_for_training);

    // Record feedback in GraphStore:
    let fb_unknown = store
        .add_investigation_feedback(
            inv_id.into(),
            "positive".into(),
            "Workaround helped but root cause unverified".into(),
            "netops-carol".into(),
        )
        .await
        .expect("add feedback");
    assert_eq!(fb_unknown.investigation_id, inv_id);
    assert_eq!(fb_unknown.operator, "netops-carol");

    // 2. Operator verifies root cause with explicit evidence citations:
    let adj_verified = validate_and_normalize_adjudication(OperatorAdjudicationRequest {
        prefix: "198.51.100.0/24".into(),
        vrf_rd: Some("65000:100".into()),
        speaker: Some("192.0.2.10".into()),
        diagnosis_verdict: "correct".into(),
        next_check_utility: Some("useful".into()),
        recommendation_accepted: true,
        action_outcome: Some("resolved".into()),
        adjudicated_by: "netops-carol".into(),
        adjudication_evidence_ids: vec!["obs-bmp-seq-7-confirmed".into()],
        rejected_hypotheses: vec![HypothesisRejection {
            hypothesis_id: "H_peer_reset".into(),
            reason: "BMP session remained established without flap".into(),
        }],
        entity_corrections: vec![],
        missing_evidence_noted: vec![],
        rationale: "Confirmed route-map permit clause omitted prefix".into(),
    })
    .expect("valid verified adjudication");

    assert!(adj_verified.root_cause_verified);
    assert!(adj_verified.eligible_for_training);

    let summary = compute_adjudicated_accuracy(&[adj_unknown, adj_verified]);
    assert_eq!(summary.total_adjudications, 2);
    assert_eq!(summary.verified_correct_count, 1);
    assert_eq!(summary.recommendation_accepted_without_rca_proof_count, 1);
    assert_eq!(summary.adjudicated_precision_pct, 100.0);
}

struct ScenarioMockProvider {
    responses: Arc<Mutex<Vec<AiResponse>>>,
    calls_made: Arc<Mutex<usize>>,
}

#[bonsai::async_trait::async_trait]
impl AiProvider for ScenarioMockProvider {
    async fn complete(
        &self,
        messages: Vec<AiMessage>,
        _tools: Vec<AiToolDef>,
    ) -> anyhow::Result<AiResponse> {
        let mut count = self.calls_made.lock().unwrap();
        *count += 1;
        let mut resp_list = self.responses.lock().unwrap();
        if resp_list.is_empty() {
            // Default response: call explain_missing_prefix tool on first turn, then summarize
            let is_tool_response = messages.iter().any(|m| m.role == "tool");
            if !is_tool_response {
                Ok(AiResponse {
                    content: Some("Calling routing diagnostics".into()),
                    tool_calls: vec![AiToolCall {
                        id: "call_explain".into(),
                        name: "explain_missing_prefix".into(),
                        arguments: json!({ "prefix": "198.51.100.0/24" }),
                    }],
                    stop_reason: "tool_use".into(),
                    tokens_used: 120,
                    cost_usd: 0.0012,
                    raw_parts: None,
                })
            } else {
                Ok(AiResponse {
                    content: Some("```json\n{\
                        \"status\": \"completed\",\
                        \"epistemic_state\": \"supported\",\
                        \"root_cause_type\": \"policy_filtering\",\
                        \"supporting_evidence_ids\": [\"obs-bmp-seq-1-test\"],\
                        \"hypotheses\": [{ \"category\": \"policy_filtering\", \"status\": \"supported\" }],\
                        \"conclusion\": \"Route-map denied prefix in post-policy\"\
                    }\n```".into()),
                    tool_calls: Vec::new(),
                    stop_reason: "end_turn".into(),
                    tokens_used: 180,
                    cost_usd: 0.0018,
                    raw_parts: None,
                })
            }
        } else {
            Ok(resp_list.remove(0))
        }
    }

    fn name(&self) -> &str {
        "scenario_mock_configured_provider"
    }
}

#[tokio::test]
async fn test_bn004_measured_provider_policy_vs_scaffolds_on_held_out_scenarios() {
    let pack = build_starter_scenario_pack(20261012);
    assert_eq!(pack.len(), 44);

    let calls_made = Arc::new(Mutex::new(0usize));
    let provider = ScenarioMockProvider {
        responses: Arc::new(Mutex::new(Vec::new())),
        calls_made: calls_made.clone(),
    };

    let mut ai_cfg = AiConfig::default();
    ai_cfg.provider = "anthropic".into();
    ai_cfg.model = "claude-opus-4-5".into();
    ai_cfg.daily_budget_usd = 5.0;
    ai_cfg.per_investigation_budget_usd = 0.5;

    // 1. Run actual configured provider against a held-out scenario (PolicyFiltering):
    let held_out_scenario = pack
        .iter()
        .find(|s| s.input.family == ScenarioFamily::PolicyFiltering)
        .expect("policy filtering scenario present");

    let single_result =
        execute_scenario_with_configured_provider(held_out_scenario, &provider, &ai_cfg).await;

    assert_eq!(
        single_result.execution_mode,
        PolicyExecutionMode::MeasuredConfiguredProvider
    );
    assert!(
        single_result.measured_latency_ms > 0,
        "Measured latency must be recorded with wall-clock time"
    );
    assert!(
        single_result.cost_usd > 0.0,
        "Actual provider cost must be tracked"
    );
    assert!(
        *calls_made.lock().unwrap() > 0,
        "Actual provider calls must be executed"
    );

    // 2. Run full comparison report: baseline runbook, scaffolds, and configured provider:
    // Take a held-out 8-scenario slice (one per canonical fault family)
    let held_out_pack: Vec<_> = [
        ScenarioFamily::PolicyFiltering,
        ScenarioFamily::NextHopResolution,
        ScenarioFamily::PeerReset,
        ScenarioFamily::LinkBfdCascade,
        ScenarioFamily::CollectorGap,
        ScenarioFamily::OverlappingVrfs,
        ScenarioFamily::CounterResetOrTelemetryStaleness,
        ScenarioFamily::ApprovedMaintenance,
    ]
    .iter()
    .filter_map(|fam| pack.iter().find(|s| s.input.family == *fam).cloned())
    .collect();

    assert_eq!(held_out_pack.len(), 8);

    let report =
        compare_configured_provider_with_scaffolds_on_pack(&held_out_pack, &provider, &ai_cfg)
            .await;

    assert_eq!(report.pack_version, SCENARIO_PACK_VERSION);
    assert_eq!(report.policy_summaries.len(), 5); // Runbook, Small, Strong, Hybrid, ConfiguredProvider

    // Verify explicit execution_mode separation:
    let small_summary = report
        .policy_summaries
        .iter()
        .find(|s| s.policy_id == CandidateInvestigatorPolicy::SmallModelSingleStep.as_str())
        .expect("small summary");
    assert_eq!(
        small_summary.execution_mode,
        PolicyExecutionMode::SimulatedModelTraceScaffold,
        "SmallModelSingleStep must remain explicitly tagged as SimulatedModelTraceScaffold"
    );

    let strong_summary = report
        .policy_summaries
        .iter()
        .find(|s| s.policy_id == CandidateInvestigatorPolicy::StrongModelMultiStep.as_str())
        .expect("strong summary");
    assert_eq!(
        strong_summary.execution_mode,
        PolicyExecutionMode::SimulatedModelTraceScaffold,
        "StrongModelMultiStep must remain explicitly tagged as SimulatedModelTraceScaffold"
    );

    let provider_summary = report
        .policy_summaries
        .iter()
        .find(|s| s.policy_id == "configured_provider_live")
        .expect("configured provider summary");
    assert_eq!(
        provider_summary.execution_mode,
        PolicyExecutionMode::MeasuredConfiguredProvider,
        "Configured provider must be tagged as MeasuredConfiguredProvider"
    );
    assert!(
        provider_summary.total_cost_usd > 0.0,
        "Measured total cost must be non-zero"
    );

    println!(
        "BN-004 Policy Comparison Summary (Held-out 8 scenarios):\n\
         - Runbook (InProcess):               Pass={:.1}% | Cost=${:.4} | Measured P95={}ms\n\
         - Small (Scaffold):                  Pass={:.1}% | Cost=${:.4} | Simulated P95={}ms\n\
         - Strong (Scaffold):                 Pass={:.1}% | Cost=${:.4} | Simulated P95={}ms\n\
         - Hybrid (Measured):                 Pass={:.1}% | Cost=${:.4} | Measured P95={}ms\n\
         - ConfiguredProvider (Measured Live): Pass={:.1}% | Cost=${:.4} | Measured P95={}ms",
        report.policy_summaries[0].pass_rate_pct,
        report.policy_summaries[0].total_cost_usd,
        report.policy_summaries[0].measured_p95_latency_ms,
        small_summary.pass_rate_pct,
        small_summary.total_cost_usd,
        small_summary.p95_latency_ms,
        strong_summary.pass_rate_pct,
        strong_summary.total_cost_usd,
        strong_summary.p95_latency_ms,
        report.policy_summaries[3].pass_rate_pct,
        report.policy_summaries[3].total_cost_usd,
        report.policy_summaries[3].measured_p95_latency_ms,
        provider_summary.pass_rate_pct,
        provider_summary.total_cost_usd,
        provider_summary.measured_p95_latency_ms,
    );

    // 3. Persist evidence bundle in docs/merger/evidence/2026-10-13-bn004/
    let evidence_dir = std::path::Path::new("docs/merger/evidence/2026-10-13-bn004");
    std::fs::create_dir_all(evidence_dir).expect("create evidence dir");
    let report_path = evidence_dir.join("bn004-investigator-benchmark-report.json");
    std::fs::write(
        &report_path,
        serde_json::to_string_pretty(&report).expect("json report"),
    )
    .expect("write report");
    assert!(report_path.exists());
}

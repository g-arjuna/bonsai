#[derive(Deserialize)]
pub(super) struct InjectDetectionRequest {
    device_address: String,
    rule_id: String,
    #[serde(default = "default_inject_severity")]
    severity: String,
}
#[derive(Serialize)]
pub(super) struct InjectDetectionResponse {
    detection_id: String,
    fired_at_ns: i64,
}
#[derive(Deserialize)]
pub(super) struct ParseSyslogFixtureRequest {
    raw: String,
    vendor: String,
    #[serde(default = "default_syslog_transport")]
    transport: String,
    #[serde(default = "default_syslog_peer_addr")]
    peer_addr: String,
}
#[derive(Serialize)]
pub(super) struct ParseSyslogFixtureResponse {
    event: SyslogEvent,
    facts: Vec<SyslogFact>,
    config_change_trigger: bool,
}
use serde::{Deserialize, Serialize};
use axum::{Json, extract::State, http::StatusCode};
use crate::signals::syslog::{SyslogEvent, SyslogFact};

use super::{AppState, default_inject_severity, default_syslog_transport, default_syslog_peer_addr};


pub(super) async fn inject_detection_handler(
    State(state): State<AppState>,
    Json(req): Json<InjectDetectionRequest>,
) -> Result<Json<InjectDetectionResponse>, (StatusCode, String)> {
    let fired_at_ns = crate::graph::common::now_ns();
    let detection_id = state
        .store
        .write_detection(
            req.device_address,
            req.rule_id,
            req.severity,
            "{}".to_string(),
            fired_at_ns,
            String::new(),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InjectDetectionResponse {
        detection_id,
        fired_at_ns,
    }))
}
pub(super) async fn parse_syslog_fixture_handler(
    State(state): State<AppState>,
    Json(req): Json<ParseSyslogFixtureRequest>,
) -> Result<Json<ParseSyslogFixtureResponse>, (StatusCode, String)> {
    let raw = req.raw.trim().to_string();
    let vendor = req.vendor.trim().to_string();
    if raw.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "raw syslog line is required".to_string(),
        ));
    }
    if vendor.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "vendor is required".to_string()));
    }

    let timestamp_ns = crate::graph::common::now_ns();
    let (event, facts) = crate::signals::syslog::parse_syslog_fixture(
        &state.layered_ingestion.syslog_patterns_path,
        &raw,
        &vendor,
        &req.transport,
        &req.peer_addr,
        timestamp_ns,
    );
    let config_change_trigger = crate::signals::syslog::matches_syslog_config_change_trigger(
        &state.layered_ingestion.syslog_patterns_path,
        &vendor,
        &event.message,
    );

    Ok(Json(ParseSyslogFixtureResponse {
        event,
        facts,
        config_change_trigger,
    }))
}

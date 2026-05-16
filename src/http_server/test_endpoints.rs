#![allow(unused_imports, dead_code, unused_variables)]
use super::*;

// ── /api/_test/inject_detection ───────────────────────────────────────────────

#[derive(Deserialize)]
struct InjectDetectionRequest {
    device_address: String,
    rule_id: String,
    #[serde(default = "default_inject_severity")]
    severity: String,
}

fn default_inject_severity() -> String {
    "info".to_string()
}

#[derive(Serialize)]
struct InjectDetectionResponse {
    detection_id: String,
    fired_at_ns: i64,
}

#[derive(Deserialize)]
struct ParseSyslogFixtureRequest {
    raw: String,
    vendor: String,
    #[serde(default = "default_syslog_transport")]
    transport: String,
    #[serde(default = "default_syslog_peer_addr")]
    peer_addr: String,
}

fn default_syslog_transport() -> String {
    "udp".to_string()
}

fn default_syslog_peer_addr() -> String {
    "127.0.0.1:5514".to_string()
}

#[derive(Serialize)]
struct ParseSyslogFixtureResponse {
    event: SyslogEvent,
    facts: Vec<SyslogFact>,
    config_change_trigger: bool,
}

async fn inject_detection_handler(
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

async fn parse_syslog_fixture_handler(
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

// ── /api/operations/weekly-trend ─────────────────────────────────────────────

#[derive(Serialize)]
struct WeeklyTrendDay {
    date: String,
    status: String,
    pass: u32,
    fail: u32,
    skip: u32,
    prereq_missing: u32,
}

#[derive(Serialize)]
struct WeeklyTrendResponse {
    days: Vec<WeeklyTrendDay>,
}

async fn weekly_trend_handler(State(state): State<AppState>) -> Json<WeeklyTrendResponse> {
    let driver_dir = std::path::Path::new(&state.runtime_dir).join("driver_results");
    let mut days: Vec<WeeklyTrendDay> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&driver_dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let s = name.to_string_lossy().into_owned();
                s.starts_with("daily-") && s.ends_with(".json")
            })
            .collect();
        files.sort_by_key(|e| e.file_name());
        // Take last 7, preserving chronological order
        let start = files.len().saturating_sub(7);
        for entry in &files[start..] {
            if let Ok(contents) = std::fs::read_to_string(entry.path())
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
            {
                let date = v["environment"]["date_utc"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let status = v["status"].as_str().unwrap_or("unknown").to_string();
                let mut pass = 0u32;
                let mut fail = 0u32;
                let mut skip = 0u32;
                let mut prereq_missing = 0u32;
                if let Some(checks) = v["checks"].as_array() {
                    for c in checks {
                        match c["status"].as_str().unwrap_or("") {
                            "pass" | "pass_with_caveats" => pass += 1,
                            "fail" => fail += 1,
                            "skip" => skip += 1,
                            "prereq_missing" => prereq_missing += 1,
                            _ => {}
                        }
                    }
                }
                days.push(WeeklyTrendDay {
                    date,
                    status,
                    pass,
                    fail,
                    skip,
                    prereq_missing,
                });
            }
        }
    }

    Json(WeeklyTrendResponse { days })
}

async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.store.subscribe_events();
    let registry_rx = state.registry.subscribe_changes();

    let graph_stream = BroadcastStream::new(rx).map(|item| {
        let data = match item {
            Ok(ev) => serde_json::to_string(&SsePayload {
                device_address: ev.device_address,
                event_type: ev.event_type,
                detail_json: ev.detail_json,
                occurred_at_ns: ev.occurred_at_ns,
                state_change_event_id: ev.state_change_event_id,
            })
            .unwrap_or_default(),
            // Receiver lagged (broadcast buffer full); send a heartbeat comment.
            Err(_) => return Ok(Event::default().comment("lag")),
        };
        Ok(Event::default().data(data))
    });

    let registry_stream = ReceiverStream::new(registry_rx).map(|change| {
        let data = serde_json::to_string(&registry_change_payload(change)).unwrap_or_default();
        Ok(Event::default().data(data))
    });

    let stream = futures::stream::select(graph_stream, registry_stream);

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn registry_change_payload(change: RegistryChange) -> SsePayload {
    match change {
        RegistryChange::Added(target) => registry_target_payload("registry_added", target),
        RegistryChange::Updated(target) => registry_target_payload("registry_updated", target),
        RegistryChange::Removed(address) => SsePayload {
            device_address: address.clone(),
            event_type: "registry_removed".to_string(),
            detail_json: serde_json::json!({ "address": address }).to_string(),
            occurred_at_ns: now_ns(),
            state_change_event_id: String::new(),
        },
    }
}

fn registry_target_payload(event_type: &str, target: TargetConfig) -> SsePayload {
    let address = target.address.clone();
    SsePayload {
        device_address: address.clone(),
        event_type: event_type.to_string(),
        detail_json: serde_json::json!({
            "address": address,
            "enabled": target.enabled,
            "hostname": target.hostname.unwrap_or_default(),
            "vendor": target.vendor.unwrap_or_default(),
            "role": target.role.unwrap_or_default(),
            "site": target.site.unwrap_or_default(),
            "credential_alias": target.credential_alias.unwrap_or_default(),
            "selected_path_count": target.selected_paths.len(),
        })
        .to_string(),
        occurred_at_ns: now_ns(),
        state_change_event_id: String::new(),
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}


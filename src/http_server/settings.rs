//! Track D1/K3/K5 — GET/PATCH /api/settings/streaming
//!
//! GET  returns the live StreamingConfig + live supervisor receiver statuses.
//! PATCH accepts a delta JSON, writes `[streaming.*]` and `[signals.*]` sections
//!       back to bonsai.toml, then hot-restarts changed receivers via the
//!       ReceiverSupervisor.  Returns `requires_restart = false` for all
//!       receivers now that syslog/snmp are also supervised.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::config::{BgpLsConfig, BmpConfig, NetflowConfig, OtlpConfig, PcepConfig, SnmpConfig, SyslogConfig};

// ── Response / request shapes ─────────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[allow(dead_code)]
pub struct StreamingReceiverStatus {
    pub name: String,
    pub enabled: bool,
    pub addr: String,
    pub protocol: String,
}

#[derive(Serialize)]
pub struct StreamingSettingsResponse {
    pub bmp: ReceiverDetail,
    pub bgp_ls: ReceiverDetail,
    pub pcep: ReceiverDetail,
    pub otlp: ReceiverDetail,
    pub netflow: ReceiverDetail,
    pub syslog_udp: ReceiverDetail,
    pub syslog_tcp: ReceiverDetail,
    pub snmp: ReceiverDetail,
    pub requires_restart: bool,
    /// K5: Live per-receiver status from ReceiverSupervisor, keyed by receiver name.
    pub receiver_statuses: std::collections::HashMap<String, crate::receiver_supervisor::ReceiverStatusSnapshot>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReceiverDetail {
    pub enabled: bool,
    pub addr: String,
    pub protocol: String,
}

/// PATCH body — all fields optional; only present keys are applied.
#[derive(Deserialize, Debug, Default)]
pub struct StreamingSettingsPatch {
    pub bmp: Option<ReceiverPatch>,
    pub bgp_ls: Option<ReceiverPatch>,
    pub pcep: Option<ReceiverPatch>,
    pub otlp: Option<ReceiverPatch>,
    pub netflow: Option<ReceiverPatch>,
    pub syslog_udp: Option<ReceiverPatch>,
    pub syslog_tcp: Option<ReceiverPatch>,
    pub snmp: Option<ReceiverPatch>,
}

#[derive(Deserialize, Debug, Default)]
pub struct ReceiverPatch {
    pub enabled: Option<bool>,
    pub addr: Option<String>,
}

#[derive(Serialize)]
pub struct PatchResponse {
    pub ok: bool,
    pub requires_restart: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct ReceiverStatusResponse {
    pub receivers: Vec<crate::receiver_supervisor::ReceiverStatusSnapshot>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn bmp_detail(c: &BmpConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.tcp_addr.clone(), protocol: "tcp".into() }
}
fn bgp_ls_detail(c: &BgpLsConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.tcp_addr.clone(), protocol: "tcp".into() }
}
fn pcep_detail(c: &PcepConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.tcp_addr.clone(), protocol: "tcp".into() }
}
fn otlp_detail(c: &OtlpConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.http_addr.clone(), protocol: "http".into() }
}
fn netflow_detail(c: &NetflowConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.udp_addr.clone(), protocol: "udp".into() }
}
fn syslog_udp_detail(c: &SyslogConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.udp_addr.clone(), protocol: "udp".into() }
}
fn syslog_tcp_detail(c: &SyslogConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.tcp_addr.clone(), protocol: "tcp".into() }
}
fn snmp_detail(c: &SnmpConfig) -> ReceiverDetail {
    ReceiverDetail { enabled: c.enabled, addr: c.udp_addr.clone(), protocol: "udp".into() }
}

// ── GET /api/receivers/status ────────────────────────────────────────────────

pub async fn get_receiver_status_handler(
    State(state): State<AppState>,
) -> Json<ReceiverStatusResponse> {
    let sup = state.receiver_supervisor.read().await;
    Json(ReceiverStatusResponse {
        receivers: sup.status_snapshot(),
    })
}

// ── GET /api/settings/streaming ───────────────────────────────────────────────

pub async fn get_streaming_settings_handler(
    State(state): State<AppState>,
) -> Json<StreamingSettingsResponse> {
    let s = &state.streaming;
    let sig = &state.signals;
    let receiver_statuses = {
        let sup = state.receiver_supervisor.read().await;
        sup.status_snapshot()
            .into_iter()
            .map(|s| (s.name.clone(), s))
            .collect()
    };
    Json(StreamingSettingsResponse {
        bmp:        bmp_detail(&s.bmp),
        bgp_ls:     bgp_ls_detail(&s.bgp_ls),
        pcep:       pcep_detail(&s.pcep),
        otlp:       otlp_detail(&s.otlp),
        netflow:    netflow_detail(&s.netflow),
        syslog_udp: syslog_udp_detail(&sig.syslog),
        syslog_tcp: syslog_tcp_detail(&sig.syslog),
        snmp:       snmp_detail(&sig.snmp),
        requires_restart: false,
        receiver_statuses,
    })
}

// ── PATCH /api/settings/streaming ─────────────────────────────────────────────

pub async fn patch_streaming_settings_handler(
    State(state): State<AppState>,
    Json(patch): Json<StreamingSettingsPatch>,
) -> Result<Json<PatchResponse>, (StatusCode, String)> {
    // Build the updated streaming config by merging the patch on top of live values.
    let s = &state.streaming;

    let new_bmp_enabled = patch.bmp.as_ref().and_then(|p| p.enabled).unwrap_or(s.bmp.enabled);
    let new_bmp_addr    = patch.bmp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.bmp.tcp_addr.clone());

    let new_bgpls_enabled = patch.bgp_ls.as_ref().and_then(|p| p.enabled).unwrap_or(s.bgp_ls.enabled);
    let new_bgpls_addr    = patch.bgp_ls.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.bgp_ls.tcp_addr.clone());

    let new_pcep_enabled = patch.pcep.as_ref().and_then(|p| p.enabled).unwrap_or(s.pcep.enabled);
    let new_pcep_addr    = patch.pcep.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.pcep.tcp_addr.clone());

    let new_otlp_enabled = patch.otlp.as_ref().and_then(|p| p.enabled).unwrap_or(s.otlp.enabled);
    let new_otlp_addr    = patch.otlp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.otlp.http_addr.clone());

    let new_nf_enabled = patch.netflow.as_ref().and_then(|p| p.enabled).unwrap_or(s.netflow.enabled);
    let new_nf_addr    = patch.netflow.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.netflow.udp_addr.clone());

    let sig = &state.signals;
    let new_syslog_enabled  = patch.syslog_udp.as_ref().and_then(|p| p.enabled)
        .or_else(|| patch.syslog_tcp.as_ref().and_then(|p| p.enabled))
        .unwrap_or(sig.syslog.enabled);
    let new_syslog_udp_addr = patch.syslog_udp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.syslog.udp_addr.clone());
    let new_syslog_tcp_addr = patch.syslog_tcp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.syslog.tcp_addr.clone());
    let new_snmp_enabled    = patch.snmp.as_ref().and_then(|p| p.enabled).unwrap_or(sig.snmp.enabled);
    let new_snmp_udp_addr   = patch.snmp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.snmp.udp_addr.clone());

    // Build the TOML fragment for both [streaming.*] and [signals.*] sections.
    let toml_fragment = format!(
        r#"
[streaming.bmp]
enabled = {bmp_en}
tcp_addr = "{bmp_addr}"

[streaming.bgp_ls]
enabled = {bgpls_en}
tcp_addr = "{bgpls_addr}"

[streaming.pcep]
enabled = {pcep_en}
tcp_addr = "{pcep_addr}"

[streaming.otlp]
enabled = {otlp_en}
http_addr = "{otlp_addr}"

[streaming.netflow]
enabled = {nf_en}
udp_addr = "{nf_addr}"

[signals.syslog]
enabled = {syslog_en}
udp_addr = "{syslog_udp}"
tcp_addr = "{syslog_tcp}"

[signals.snmp]
enabled = {snmp_en}
udp_addr = "{snmp_udp}"
"#,
        bmp_en = new_bmp_enabled,
        bmp_addr = new_bmp_addr,
        bgpls_en = new_bgpls_enabled,
        bgpls_addr = new_bgpls_addr,
        pcep_en = new_pcep_enabled,
        pcep_addr = new_pcep_addr,
        otlp_en = new_otlp_enabled,
        otlp_addr = new_otlp_addr,
        nf_en = new_nf_enabled,
        nf_addr = new_nf_addr,
        syslog_en = new_syslog_enabled,
        syslog_udp = new_syslog_udp_addr,
        syslog_tcp = new_syslog_tcp_addr,
        snmp_en = new_snmp_enabled,
        snmp_udp = new_snmp_udp_addr,
    );

    // Locate the config file.
    let config_path = std::env::var("BONSAI_CONFIG").unwrap_or_else(|_| "bonsai.toml".to_string());

    let current = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("read config: {e}")))?;

    // Surgical replacement: remove existing [streaming.*] and [signals.*] blocks.
    let stripped = strip_streaming_section(&strip_signals_section(&current));
    let updated = format!("{}\n{}", stripped.trim_end(), toml_fragment);

    tokio::fs::write(&config_path, updated)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write config: {e}")))?;

    // K3: Live restart of changed receivers via ReceiverSupervisor.
    // All receivers — including syslog/snmp — are now supervised, so no
    // process restart is required for any setting change.
    let bus = std::sync::Arc::clone(&state.event_bus);
    let gov = state.governor.clone();
    let targets = state.targets.clone();
    let pattern_dir = state.layered_ingestion.syslog_patterns_path.clone();

    // Build updated configs before acquiring the write lock.
    let bmp_to_restart = patch.bmp.is_some().then(|| crate::config::BmpConfig {
        enabled: new_bmp_enabled,
        tcp_addr: new_bmp_addr.clone(),
        ..state.streaming.bmp.clone()
    });
    let bgp_ls_to_restart = patch.bgp_ls.is_some().then(|| crate::config::BgpLsConfig {
        enabled: new_bgpls_enabled,
        tcp_addr: new_bgpls_addr.clone(),
        ..state.streaming.bgp_ls.clone()
    });
    let otlp_to_restart = patch.otlp.is_some().then(|| crate::config::OtlpConfig {
        enabled: new_otlp_enabled,
        http_addr: new_otlp_addr.clone(),
    });
    let netflow_to_restart = patch.netflow.is_some().then(|| crate::config::NetflowConfig {
        enabled: new_nf_enabled,
        udp_addr: new_nf_addr.clone(),
    });
    let syslog_to_restart = (patch.syslog_udp.is_some() || patch.syslog_tcp.is_some()).then(|| {
        crate::config::SyslogConfig {
            enabled: new_syslog_enabled,
            udp_addr: new_syslog_udp_addr.clone(),
            tcp_addr: new_syslog_tcp_addr.clone(),
            ..state.signals.syslog.clone()
        }
    });
    let snmp_to_restart = patch.snmp.is_some().then(|| crate::config::SnmpConfig {
        enabled: new_snmp_enabled,
        udp_addr: new_snmp_udp_addr.clone(),
        ..state.signals.snmp.clone()
    });

    {
        let mut sup = state.receiver_supervisor.write().await;

        if let Some(c) = bmp_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            let gov2 = gov.clone();
            sup.spawn("bmp", new_bmp_addr.clone(), move |sd| async move {
                crate::streaming::bmp::run_bmp_receiver(c, vec![], bus2, sd, gov2).await
            });
        }
        if let Some(c) = bgp_ls_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            sup.spawn("bgp_ls", new_bgpls_addr.clone(), move |sd| async move {
                crate::streaming::bgp_ls::run_bgp_ls_receiver(c, vec![], bus2, sd).await
            });
        }
        if let Some(c) = otlp_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            sup.spawn("otlp", new_otlp_addr.clone(), move |sd| async move {
                crate::streaming::otlp::run_otlp_receiver(c, bus2, sd).await
            });
        }
        if let Some(c) = netflow_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            sup.spawn("netflow", new_nf_addr.clone(), move |sd| async move {
                crate::streaming::netflow::run_netflow_receiver(c, bus2, sd).await
            });
        }
        if let Some(c) = syslog_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            let gov2 = gov.clone();
            let tgts = targets.clone();
            let pdir = pattern_dir.clone();
            let addr = format!("{}/{}", new_syslog_udp_addr, new_syslog_tcp_addr);
            sup.spawn("syslog", addr, move |sd| async move {
                crate::signals::syslog::run_syslog_receiver(c, pdir, tgts, bus2, sd, gov2).await
            });
        }
        if let Some(c) = snmp_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            let tgts = targets.clone();
            sup.spawn("snmp", new_snmp_udp_addr.clone(), move |sd| async move {
                crate::signals::snmp::run_snmp_receiver(c, tgts, bus2, sd).await
            });
        }
    }

    Ok(Json(PatchResponse {
        ok: true,
        requires_restart: false,
        message: "Receiver config written and applied live.".to_string(),
    }))
}

/// Remove all `[streaming*]` TOML sections from the file content.
fn strip_streaming_section(content: &str) -> String {
    strip_toml_prefix(content, "[streaming")
}

/// Remove all `[signals*]` TOML sections from the file content.
fn strip_signals_section(content: &str) -> String {
    strip_toml_prefix(content, "[signals")
}

/// Generic: remove all TOML sections whose header starts with `prefix`.
fn strip_toml_prefix(content: &str, prefix: &str) -> String {
    let mut out = Vec::new();
    let mut in_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(prefix) {
            in_section = true;
            continue;
        }
        if trimmed.starts_with('[') && !trimmed.starts_with(prefix) {
            in_section = false;
        }
        if !in_section {
            out.push(line);
        }
    }
    out.join("\n")
}

// ── GET /api/ai/config ────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct AiConfigResponse {
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub has_api_key: bool,
    pub per_investigation_budget_usd: f64,
    pub daily_budget_usd: f64,
    pub auto_investigate_unmatched: bool,
}

pub async fn get_ai_config_handler(
    State(state): State<AppState>,
) -> Json<AiConfigResponse> {
    let cfg = &state.ai_config;
    let has_api_key = std::env::var(&cfg.api_key_env).is_ok();
    Json(AiConfigResponse {
        provider: cfg.provider.clone(),
        model: cfg.model.clone(),
        api_key_env: cfg.api_key_env.clone(),
        has_api_key,
        per_investigation_budget_usd: cfg.per_investigation_budget_usd,
        daily_budget_usd: cfg.daily_budget_usd,
        auto_investigate_unmatched: cfg.auto_investigate_unmatched,
    })
}

// ── POST /api/ai/test ─────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct AiTestResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub provider: String,
    pub model: String,
}

pub async fn post_ai_test_handler(
    State(state): State<AppState>,
) -> Json<AiTestResponse> {
    let cfg = &state.ai_config;
    let provider_name = cfg.provider.clone();
    let model_name = cfg.model.clone();
    match crate::ai_provider::build_provider(cfg) {
        Err(e) => Json(AiTestResponse {
            ok: false,
            error: Some(e.to_string()),
            provider: provider_name,
            model: model_name,
        }),
        Ok(provider) => {
            let msgs = vec![crate::ai_provider::AiMessage::user(
                "Reply with exactly the word: pong",
            )];
            match provider.complete(msgs, vec![]).await {
                Ok(_) => Json(AiTestResponse { ok: true, error: None, provider: provider_name, model: model_name }),
                Err(e) => Json(AiTestResponse { ok: false, error: Some(e.to_string()), provider: provider_name, model: model_name }),
            }
        }
    }
}

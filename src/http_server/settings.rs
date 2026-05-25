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
use crate::config::{BgpLsConfig, BmpConfig, NetflowConfig, OtlpConfig, PcepConfig, SflowConfig, SnmpConfig, SyslogConfig};

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
    pub sflow: ReceiverDetail,
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
    pub sflow: Option<ReceiverPatch>,
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
fn sflow_detail(c: &SflowConfig) -> ReceiverDetail {
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

// ── D4-7 T5: DB-backed streaming settings snapshot ──────────────────────────

/// Persisted to ConfigItem DB as `runtime_config:streaming`.
/// Restored on boot to override TOML defaults.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct StreamingSnapshot {
    #[serde(default)] pub bmp_enabled: bool,
    #[serde(default)] pub bmp_addr: String,
    #[serde(default)] pub bgpls_enabled: bool,
    #[serde(default)] pub bgpls_addr: String,
    #[serde(default)] pub pcep_enabled: bool,
    #[serde(default)] pub pcep_addr: String,
    #[serde(default)] pub otlp_enabled: bool,
    #[serde(default)] pub otlp_addr: String,
    #[serde(default)] pub netflow_enabled: bool,
    #[serde(default)] pub netflow_addr: String,
    #[serde(default)] pub sflow_enabled: bool,
    #[serde(default)] pub sflow_addr: String,
    #[serde(default)] pub syslog_enabled: bool,
    #[serde(default)] pub syslog_udp_addr: String,
    #[serde(default)] pub syslog_tcp_addr: String,
    #[serde(default)] pub snmp_enabled: bool,
    #[serde(default)] pub snmp_udp_addr: String,
}

impl StreamingSnapshot {
    /// Apply this DB snapshot onto the TOML-based streaming/signals configs.
    /// DB values override TOML defaults.
    pub fn apply_to_config(&self, streaming: &mut crate::config::StreamingConfig, signals: &mut crate::config::SignalsConfig) {
        if !self.bmp_addr.is_empty() {
            streaming.bmp.enabled = self.bmp_enabled;
            streaming.bmp.tcp_addr = self.bmp_addr.clone();
        }
        if !self.bgpls_addr.is_empty() {
            streaming.bgp_ls.enabled = self.bgpls_enabled;
            streaming.bgp_ls.tcp_addr = self.bgpls_addr.clone();
        }
        if !self.pcep_addr.is_empty() {
            streaming.pcep.enabled = self.pcep_enabled;
            streaming.pcep.tcp_addr = self.pcep_addr.clone();
        }
        if !self.otlp_addr.is_empty() {
            streaming.otlp.enabled = self.otlp_enabled;
            streaming.otlp.http_addr = self.otlp_addr.clone();
        }
        if !self.netflow_addr.is_empty() {
            streaming.netflow.enabled = self.netflow_enabled;
            streaming.netflow.udp_addr = self.netflow_addr.clone();
        }
        if !self.sflow_addr.is_empty() {
            streaming.sflow.enabled = self.sflow_enabled;
            streaming.sflow.udp_addr = self.sflow_addr.clone();
        }
        if !self.syslog_udp_addr.is_empty() || !self.syslog_tcp_addr.is_empty() {
            signals.syslog.enabled = self.syslog_enabled;
            if !self.syslog_udp_addr.is_empty() {
                signals.syslog.udp_addr = self.syslog_udp_addr.clone();
            }
            if !self.syslog_tcp_addr.is_empty() {
                signals.syslog.tcp_addr = self.syslog_tcp_addr.clone();
            }
        }
        if !self.snmp_udp_addr.is_empty() {
            signals.snmp.enabled = self.snmp_enabled;
            signals.snmp.udp_addr = self.snmp_udp_addr.clone();
        }
    }
}

// ── D4-7 T5: Runtime config snapshot (all non-bootstrap tunables) ─────────────

/// Each tunable TOML section is stored as a separate ConfigItem row keyed by
/// `runtime_config:{section}`. On boot, any DB-stored section overrides the
/// TOML default. On PATCH /api/settings/{section}, the new value is written
/// to the DB and applied to the live config.
///
/// Sections: retention, ingest, archive, storage, event_bus, remediation, gnn,
///           logging, ai, lab, integrations_servicenow, integrations_tsdb, assignment
pub const RUNTIME_CONFIG_SECTIONS: &[&str] = &[
    "retention", "ingest", "archive", "storage", "event_bus", "remediation",
    "gnn", "logging", "ai", "lab", "integrations_servicenow", "integrations_tsdb",
    "assignment", "streaming", "security",
];

/// Boot-time: load all `runtime_config:*` items from DB and apply overrides to `cfg`.
/// Returns the count of sections overridden.
pub async fn apply_runtime_overrides_from_db(
    store: &crate::graph::GraphStore,
    cfg: &mut crate::config::Config,
) -> usize {
    let items = match store.list_config_items(Some("runtime_config".to_string())).await {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load runtime config from DB");
            return 0;
        }
    };
    let mut count = 0;
    for item in &items {
        if !item.enabled { continue; }
        let section = item.name.as_str();
        let json = &item.content_json;
        let ok = match section {
            "retention" => serde_json::from_str(json).map(|v| cfg.retention = v).is_ok(),
            "ingest" => serde_json::from_str(json).map(|v| cfg.ingest = v).is_ok(),
            "archive" => serde_json::from_str(json).map(|v| cfg.archive = v).is_ok(),
            "storage" => serde_json::from_str(json).map(|v| cfg.storage = v).is_ok(),
            "event_bus" => serde_json::from_str(json).map(|v| cfg.event_bus = v).is_ok(),
            "remediation" => serde_json::from_str(json).map(|v| cfg.remediation = v).is_ok(),
            "gnn" => serde_json::from_str(json).map(|v| cfg.gnn = v).is_ok(),
            "logging" => serde_json::from_str(json).map(|v| cfg.logging = v).is_ok(),
            "ai" => serde_json::from_str(json).map(|v| cfg.ai = v).is_ok(),
            "lab" => serde_json::from_str(json).map(|v| cfg.lab = v).is_ok(),
            "assignment" => serde_json::from_str(json).map(|v| cfg.assignment = v).is_ok(),
            "integrations_servicenow" => serde_json::from_str(json).map(|v| cfg.integrations.servicenow = v).is_ok(),
            "integrations_tsdb" => serde_json::from_str(json).map(|v| cfg.integrations.tsdb = v).is_ok(),
            "streaming" => {
                // Streaming uses StreamingSnapshot which has its own apply logic.
                match serde_json::from_str::<StreamingSnapshot>(json) {
                    Ok(snap) => { snap.apply_to_config(&mut cfg.streaming, &mut cfg.signals); true }
                    Err(_) => false,
                }
            }
            "security" => serde_json::from_str(json).map(|v| cfg.security = v).is_ok(),
            _ => false,
        };
        if ok {
            count += 1;
            tracing::info!(section, "applied runtime config override from DB");
        } else {
            tracing::warn!(section, "failed to parse runtime config from DB, using TOML default");
        }
    }
    count
}

/// Persist a single runtime config section to the DB.
pub async fn persist_runtime_section(
    store: &crate::graph::GraphStore,
    section: &str,
    json: &str,
) -> anyhow::Result<()> {
    let item = crate::graph::ConfigItemRecord {
        id: format!("runtime_config:{section}"),
        config_class: "runtime_config".to_string(),
        vendor: String::new(),
        name: section.to_string(),
        version: "1".to_string(),
        content_json: json.to_string(),
        enabled: true,
        created_by: "settings_api".to_string(),
    };
    store.upsert_config_item(item).await
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
        sflow:      sflow_detail(&s.sflow),
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

    let new_sf_enabled = patch.sflow.as_ref().and_then(|p| p.enabled).unwrap_or(s.sflow.enabled);
    let new_sf_addr    = patch.sflow.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| s.sflow.udp_addr.clone());

    let sig = &state.signals;
    let new_syslog_enabled  = patch.syslog_udp.as_ref().and_then(|p| p.enabled)
        .or_else(|| patch.syslog_tcp.as_ref().and_then(|p| p.enabled))
        .unwrap_or(sig.syslog.enabled);
    let new_syslog_udp_addr = patch.syslog_udp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.syslog.udp_addr.clone());
    let new_syslog_tcp_addr = patch.syslog_tcp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.syslog.tcp_addr.clone());
    let new_snmp_enabled    = patch.snmp.as_ref().and_then(|p| p.enabled).unwrap_or(sig.snmp.enabled);
    let new_snmp_udp_addr   = patch.snmp.as_ref().and_then(|p| p.addr.clone()).unwrap_or_else(|| sig.snmp.udp_addr.clone());

    // D4-7 T5: Persist streaming/signals settings to ConfigItem DB instead of TOML file.
    let snapshot = StreamingSnapshot {
        bmp_enabled: new_bmp_enabled, bmp_addr: new_bmp_addr.clone(),
        bgpls_enabled: new_bgpls_enabled, bgpls_addr: new_bgpls_addr.clone(),
        pcep_enabled: new_pcep_enabled, pcep_addr: new_pcep_addr.clone(),
        otlp_enabled: new_otlp_enabled, otlp_addr: new_otlp_addr.clone(),
        netflow_enabled: new_nf_enabled, netflow_addr: new_nf_addr.clone(),
        sflow_enabled: new_sf_enabled, sflow_addr: new_sf_addr.clone(),
        syslog_enabled: new_syslog_enabled, syslog_udp_addr: new_syslog_udp_addr.clone(),
        syslog_tcp_addr: new_syslog_tcp_addr.clone(),
        snmp_enabled: new_snmp_enabled, snmp_udp_addr: new_snmp_udp_addr.clone(),
    };
    let content_json = serde_json::to_string(&snapshot).unwrap_or_default();
    let item = crate::graph::ConfigItemRecord {
        id: "runtime_config:streaming".to_string(),
        config_class: "runtime_config".to_string(),
        vendor: String::new(),
        name: "streaming".to_string(),
        version: "1".to_string(),
        content_json,
        enabled: true,
        created_by: "settings_api".to_string(),
    };
    state.store.upsert_config_item(item).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("persist streaming config: {e}")))?;

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
    let sflow_to_restart = patch.sflow.is_some().then(|| crate::config::SflowConfig {
        enabled: new_sf_enabled,
        udp_addr: new_sf_addr.clone(),
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
        if let Some(c) = sflow_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            sup.spawn("sflow", new_sf_addr.clone(), move |sd| async move {
                crate::streaming::sflow::run_sflow_receiver(c, bus2, sd).await
            });
        }
        if let Some(c) = syslog_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            let gov2 = gov.clone();
            let tgts = targets.clone();
            let pdir = pattern_dir.clone();
            let addr = format!("{}/{}", new_syslog_udp_addr, new_syslog_tcp_addr);
            sup.spawn("syslog", addr, move |sd| async move {
                crate::signals::syslog::run_syslog_receiver(c, pdir, tgts, bus2, sd, gov2, None, None).await
            });
        }
        if let Some(c) = snmp_to_restart {
            let bus2 = std::sync::Arc::clone(&bus);
            let tgts = targets.clone();
            sup.spawn("snmp", new_snmp_udp_addr.clone(), move |sd| async move {
                crate::signals::snmp::run_snmp_receiver(c, tgts, bus2, sd, None, None).await
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
#[allow(dead_code)]
fn strip_streaming_section(content: &str) -> String {
    strip_toml_prefix(content, "[streaming")
}

/// Remove all `[signals*]` TOML sections from the file content.
#[allow(dead_code)]
fn strip_signals_section(content: &str) -> String {
    strip_toml_prefix(content, "[signals")
}

/// Generic: remove all TOML sections whose header starts with `prefix`.
#[allow(dead_code)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
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
            latency_ms: None,
        }),
        Ok(provider) => {
            let msgs = vec![crate::ai_provider::AiMessage::user(
                "Reply with exactly the word: pong",
            )];
            let start = std::time::Instant::now();
            match provider.complete(msgs, vec![]).await {
                Ok(_) => Json(AiTestResponse {
                    ok: true, error: None, provider: provider_name, model: model_name,
                    latency_ms: Some(start.elapsed().as_millis() as u64),
                }),
                Err(e) => Json(AiTestResponse {
                    ok: false, error: Some(e.to_string()), provider: provider_name, model: model_name,
                    latency_ms: Some(start.elapsed().as_millis() as u64),
                }),
            }
        }
    }
}

// ── LLM Provider management (D4-3 T5) ────────────────────────────────────────

/// A configured LLM provider stored in the vault under alias `llm-{name}`.
/// The vault username stores a JSON blob with provider metadata.
/// The vault password stores the API key.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct LlmProviderEntry {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_provider_active")]
    pub active: bool,
    #[serde(default)]
    pub has_api_key: bool,
}

fn default_provider_active() -> bool { true }

fn llm_alias(name: &str) -> String {
    format!("llm-{}", name.trim().to_lowercase().replace(' ', "-"))
}

pub async fn list_ai_providers_handler(
    State(state): State<AppState>,
) -> Json<Vec<LlmProviderEntry>> {
    let summaries = state.credentials.list().unwrap_or_default();
    let mut providers: Vec<LlmProviderEntry> = Vec::new();
    for s in &summaries {
        if !s.alias.starts_with("llm-") {
            continue;
        }
        let username = state.credentials.username_for_alias(&s.alias).unwrap_or_default();
        if let Ok(mut entry) = serde_json::from_str::<LlmProviderEntry>(&username) {
            let has_key = state.credentials.resolve(&s.alias, crate::credentials::ResolvePurpose::Test)
                .map(|r| !r.password.is_empty())
                .unwrap_or(false);
            entry.has_api_key = has_key;
            providers.push(entry);
        }
    }
    Json(providers)
}

#[derive(serde::Deserialize)]
pub struct UpsertAiProviderRequest {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_provider_active")]
    pub active: bool,
    #[serde(default)]
    pub api_key: String,
}

pub async fn upsert_ai_provider_handler(
    State(state): State<AppState>,
    Json(body): Json<UpsertAiProviderRequest>,
) -> Result<Json<LlmProviderEntry>, (StatusCode, String)> {
    let alias = llm_alias(&body.name);
    let meta = LlmProviderEntry {
        name: body.name.clone(),
        provider: body.provider.clone(),
        model: body.model.clone(),
        base_url: body.base_url.clone(),
        active: body.active,
        has_api_key: false,
    };
    let username_json = serde_json::to_string(&meta)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let api_key = if body.api_key.is_empty() {
        state.credentials
            .resolve(&alias, crate::credentials::ResolvePurpose::Test)
            .map(|r| r.password.to_string())
            .unwrap_or_default()
    } else {
        body.api_key.clone()
    };

    let exists = state.credentials.list().unwrap_or_default().iter().any(|s| s.alias == alias);
    let result = if exists {
        state.credentials.update(&alias, &username_json, &api_key)
    } else {
        state.credentials.add(&alias, &username_json, &api_key)
    };
    result.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(LlmProviderEntry {
        has_api_key: !api_key.is_empty(),
        ..meta
    }))
}

#[derive(serde::Deserialize)]
pub struct RemoveAiProviderRequest {
    pub name: String,
}

pub async fn remove_ai_provider_handler(
    State(state): State<AppState>,
    Json(body): Json<RemoveAiProviderRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let alias = llm_alias(&body.name);
    state.credentials.remove(&alias)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({"ok": true})))
}

#[derive(serde::Deserialize)]
pub struct TestAiProviderRequest {
    pub name: String,
}

pub async fn test_ai_provider_handler(
    State(state): State<AppState>,
    Json(body): Json<TestAiProviderRequest>,
) -> Json<AiTestResponse> {
    let alias = llm_alias(&body.name);
    let username = match state.credentials.username_for_alias(&alias) {
        Ok(u) => u,
        Err(e) => return Json(AiTestResponse {
            ok: false, error: Some(e.to_string()),
            provider: String::new(), model: String::new(), latency_ms: None,
        }),
    };
    let meta: LlmProviderEntry = match serde_json::from_str(&username) {
        Ok(m) => m,
        Err(e) => return Json(AiTestResponse {
            ok: false, error: Some(e.to_string()),
            provider: String::new(), model: String::new(), latency_ms: None,
        }),
    };
    let api_key = state.credentials
        .resolve(&alias, crate::credentials::ResolvePurpose::Test)
        .map(|r| r.password.to_string())
        .unwrap_or_default();

    let tmp_env = format!("_BONSAI_AI_TEST_{}", std::process::id());
    unsafe {
        std::env::set_var(&tmp_env, &api_key);
    }
    let cfg = crate::config::AiConfig {
        provider: meta.provider.clone(),
        model: meta.model.clone(),
        api_key_env: tmp_env.clone(),
        base_url: meta.base_url.clone(),
        ..Default::default()
    };
    let result = match crate::ai_provider::build_provider(&cfg) {
        Err(e) => AiTestResponse {
            ok: false, error: Some(e.to_string()),
            provider: meta.provider.clone(), model: meta.model.clone(), latency_ms: None,
        },
        Ok(prov) => {
            let msgs = vec![crate::ai_provider::AiMessage::user("Reply with exactly the word: pong")];
            let start = std::time::Instant::now();
            match prov.complete(msgs, vec![]).await {
                Ok(_) => AiTestResponse {
                    ok: true, error: None,
                    provider: meta.provider.clone(), model: meta.model.clone(),
                    latency_ms: Some(start.elapsed().as_millis() as u64),
                },
                Err(e) => AiTestResponse {
                    ok: false, error: Some(e.to_string()),
                    provider: meta.provider.clone(), model: meta.model.clone(),
                    latency_ms: Some(start.elapsed().as_millis() as u64),
                },
            }
        }
    };
    unsafe {
        std::env::remove_var(&tmp_env);
    }
    Json(result)
}

// ── Active AI provider resolution (vault-first) ───────────────────────────────

/// ConfigItem key used to store the name of the currently active LLM provider.
pub const ACTIVE_AI_PROVIDER_KEY: &str = "ai:active_provider";

/// Resolve the active AI provider from vault (vault-first, env-var fallback).
///
/// Priority:
///   1. Vault alias `llm-{active_name}` from ConfigItem `ai:active_provider`
///   2. Env-var from `state.ai_config.api_key_env` (legacy / TOML-configured)
///
/// Returns `(AiConfig, api_key)` so callers can use `build_provider_with_key`.
pub async fn resolve_active_ai_provider(
    state: &super::AppState,
) -> Option<(crate::config::AiConfig, String)> {
    // Try vault-backed active provider first
    let config_items = state.store
        .list_config_items(Some("app_config".to_string())).await
        .unwrap_or_default();
    let active_name = config_items.iter()
        .find(|i| i.id == ACTIVE_AI_PROVIDER_KEY && i.enabled)
        .and_then(|i| serde_json::from_str::<String>(&i.content_json).ok());

    if let Some(active_name) = active_name {
        let alias = llm_alias(&active_name);
        if let Ok(username) = state.credentials.username_for_alias(&alias) {
            if let Ok(meta) = serde_json::from_str::<LlmProviderEntry>(&username) {
                let api_key = state.credentials
                    .resolve(&alias, crate::credentials::ResolvePurpose::Test)
                    .map(|r| r.password.to_string())
                    .unwrap_or_default();
                // Ollama needs no key
                if meta.provider != "ollama" && api_key.is_empty() {
                    tracing::warn!(provider = %meta.provider, alias = %alias, "active AI provider has no API key in vault");
                } else {
                    let cfg = crate::config::AiConfig {
                        provider: meta.provider,
                        model: meta.model,
                        base_url: meta.base_url,
                        api_key_env: String::new(),
                        ..Default::default()
                    };
                    return Some((cfg, api_key));
                }
            }
        }
    }

    // Env-var fallback (legacy path)
    let cfg = state.ai_config.clone();
    if cfg.provider == "ollama" {
        let key = std::env::var(&cfg.api_key_env).unwrap_or_default();
        return Some((cfg, key));
    }
    let key = std::env::var(&cfg.api_key_env).ok()?;
    Some((cfg, key))
}

/// Returns true if any AI provider is available (vault or env-var).
pub async fn has_active_ai_provider(state: &super::AppState) -> bool {
    resolve_active_ai_provider(state).await.is_some()
}

#[derive(serde::Deserialize)]
pub struct ActivateAiProviderRequest {
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct ActivateAiProviderResponse {
    pub ok: bool,
    pub active_provider: String,
}

/// POST /api/ai/providers/activate — set a named vault provider as the active one.
pub async fn activate_ai_provider_handler(
    State(state): State<AppState>,
    Json(body): Json<ActivateAiProviderRequest>,
) -> Result<Json<ActivateAiProviderResponse>, (StatusCode, String)> {
    let alias = llm_alias(&body.name);
    // Verify the provider exists in vault
    let exists = state.credentials.list().unwrap_or_default().iter().any(|s| s.alias == alias);
    if !exists {
        return Err((StatusCode::NOT_FOUND, format!("provider '{}' not found in vault", body.name)));
    }
    let content_json = serde_json::to_string(&body.name)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let item = crate::graph::ConfigItemRecord {
        id: ACTIVE_AI_PROVIDER_KEY.to_string(),
        config_class: "app_config".to_string(),
        vendor: String::new(),
        name: "active_provider".to_string(),
        version: "1".to_string(),
        content_json,
        enabled: true,
        created_by: "ui".to_string(),
    };
    state.store
        .upsert_config_item(item)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(provider = %body.name, "active AI provider updated");
    Ok(Json(ActivateAiProviderResponse {
        ok: true,
        active_provider: body.name,
    }))
}

/// GET /api/ai/providers/active — return the current active provider name (if set).
pub async fn get_active_ai_provider_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let active_name = state.store
        .list_config_items(Some("app_config".to_string())).await
        .unwrap_or_default()
        .into_iter()
        .find(|i| i.id == ACTIVE_AI_PROVIDER_KEY && i.enabled)
        .and_then(|i| serde_json::from_str::<String>(&i.content_json).ok());
    Json(serde_json::json!({ "active_provider": active_name }))
}

#[derive(Serialize)]
pub struct PatternReloadResponse {
    pub syslog_reloaded: bool,
    pub snmp_reloaded: bool,
    pub syslog_pattern_count: usize,
    pub snmp_pattern_count: usize,
    pub error: Option<String>,
}

/// POST /api/config/reload-patterns
/// Hot-reload syslog and SNMP OID pattern extractors from ConfigItem DB (with disk fallback).
pub async fn reload_patterns_handler(
    State(state): State<super::AppState>,
) -> Result<Json<PatternReloadResponse>, (StatusCode, String)> {
    use crate::signals::syslog::SyslogFactExtractor;
    use crate::signals::snmp::SnmpFactExtractor;
    use std::sync::Arc;

    let mut syslog_reloaded = false;
    let mut syslog_count = 0;
    let mut snmp_reloaded = false;
    let mut snmp_count = 0;
    let mut errors: Vec<String> = Vec::new();

    // D4-7 T5: Reload from ConfigItem DB with disk fallback.
    let syslog_items = state.store.load_config_yaml_by_class("syslog_pattern").await.unwrap_or_default();
    let snmp_items = state.store.load_config_yaml_by_class("snmp_oid_pattern").await.unwrap_or_default();

    if let Some(tx) = &state.syslog_pattern_tx {
        let dir = &state.syslog_pattern_dir;
        let extractor = SyslogFactExtractor::load_from_yaml_strings(&syslog_items, dir);
        syslog_count = extractor.pattern_count();
        match tx.send(Arc::new(extractor)) {
            Ok(_) => {
                syslog_reloaded = true;
                tracing::info!(source = if syslog_items.is_empty() { "disk" } else { "db" },
                    patterns = syslog_count, "syslog patterns hot-reloaded");
            }
            Err(e) => errors.push(format!("syslog: {e}")),
        }
    }

    if let Some(tx) = &state.snmp_pattern_tx {
        let dir = &state.snmp_oid_pattern_dir;
        let extractor = SnmpFactExtractor::load_from_yaml_strings(&snmp_items, dir);
        snmp_count = extractor.pattern_count();
        match tx.send(Arc::new(extractor)) {
            Ok(_) => {
                snmp_reloaded = true;
                tracing::info!(source = if snmp_items.is_empty() { "disk" } else { "db" },
                    patterns = snmp_count, "snmp OID patterns hot-reloaded");
            }
            Err(e) => errors.push(format!("snmp: {e}")),
        }
    }

    Ok(Json(PatternReloadResponse {
        syslog_reloaded,
        snmp_reloaded,
        syslog_pattern_count: syslog_count,
        snmp_pattern_count: snmp_count,
        error: if errors.is_empty() { None } else { Some(errors.join("; ")) },
    }))
}

// ── D4-1 T4: MIB upload + compile pipeline ─────────────────────────────────

#[derive(Deserialize)]
pub struct MibUploadRequest {
    pub filename: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct MibUploadResponse {
    pub success: bool,
    pub filename: String,
    pub oid_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// POST /api/snmp/mibs — Upload a MIB file, compile it, and store OID patterns.
pub(super) async fn mib_upload_handler(
    State(state): State<AppState>,
    Json(req): Json<MibUploadRequest>,
) -> Result<Json<MibUploadResponse>, (StatusCode, String)> {
    let filename = req.filename.trim().to_string();
    if filename.is_empty() || req.content.is_empty() {
        return Ok(Json(MibUploadResponse {
            success: false,
            filename,
            oid_count: 0,
            error: Some("filename and content are required".to_string()),
        }));
    }

    // Write MIB to runtime/mibs/
    let mibs_dir = std::path::Path::new("runtime").join("mibs");
    std::fs::create_dir_all(&mibs_dir)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to create mibs dir: {e}")))?;

    let mib_path = mibs_dir.join(&filename);
    std::fs::write(&mib_path, &req.content)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write MIB file: {e}")))?;

    // Run compile_mib.py
    let output = tokio::process::Command::new("python3")
        .args(["scripts/compile_mib.py", &mib_path.to_string_lossy()])
        .output()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to run compile_mib.py: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(Json(MibUploadResponse {
            success: false,
            filename,
            oid_count: 0,
            error: Some(format!("MIB compile failed: {}", stderr.trim())),
        }));
    }

    // Parse the JSON output
    let stdout = String::from_utf8_lossy(&output.stdout);
    let oid_entries: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_default();

    let oid_count = oid_entries.len();

    // Store each OID pattern as a ConfigItem
    for entry in &oid_entries {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
        let _oid_prefix = entry.get("oid_prefix").and_then(|v| v.as_str()).unwrap_or("");
        let mib_module = entry.get("mib_module").and_then(|v| v.as_str()).unwrap_or("");

        let item = crate::graph::ConfigItemRecord {
            id: format!("mib-oid-{}-{}", mib_module.to_lowercase(), name.to_lowercase()),
            config_class: "snmp_oid_pattern".to_string(),
            vendor: mib_module.to_string(),
            name: name.to_string(),
            version: String::new(),
            content_json: serde_json::to_string(entry).unwrap_or_default(),
            enabled: true,
            created_by: "mib_upload".to_string(),
        };

        if let Err(e) = state.store.upsert_config_item(item).await {
            tracing::warn!(error = %e, name = name, "failed to store OID pattern from MIB upload");
        }
    }

    Ok(Json(MibUploadResponse {
        success: true,
        filename,
        oid_count,
        error: None,
    }))
}

// ── D4-5 T4: TSDB query proxy ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TsdbQueryParams {
    pub metric: String,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
}

/// GET /api/tsdb/config — return TSDB integration status (no secrets).
pub(super) async fn tsdb_config_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let cfg = &state.tsdb_config;
    Json(serde_json::json!({
        "enabled": cfg.enabled,
        "tsdb_type": cfg.tsdb_type,
        "query_url": cfg.query_url,
        "default_lookback": cfg.default_lookback,
        "max_range": cfg.max_range,
        "has_credential": !cfg.credential_alias.is_empty(),
    }))
}

/// GET /api/tsdb/query — proxy a metric query to the configured TSDB backend.
/// Supports Prometheus/Thanos/VictoriaMetrics query_range API and InfluxDB query API.
pub(super) async fn tsdb_query_handler(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TsdbQueryParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = &state.tsdb_config;
    if !cfg.enabled || cfg.query_url.is_empty() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "TSDB integration not enabled".into()));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("HTTP client: {e}")))?;

    // Build query based on TSDB type
    let response = match cfg.tsdb_type.as_str() {
        "prometheus" | "victoria_metrics" | "thanos" => {
            tsdb_prometheus_query(&client, cfg, &state, &params).await?
        }
        "influxdb" => {
            tsdb_influxdb_query(&client, cfg, &state, &params).await?
        }
        other => {
            return Err((StatusCode::BAD_REQUEST, format!("unsupported TSDB type: {other}")));
        }
    };

    Ok(Json(response))
}

async fn tsdb_prometheus_query(
    client: &reqwest::Client,
    cfg: &crate::config::TsdbConfig,
    state: &AppState,
    params: &TsdbQueryParams,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let base = cfg.query_url.trim_end_matches('/');
    let start = params.start.as_deref().unwrap_or(&cfg.default_lookback);
    let end = params.end.as_deref().unwrap_or("now");
    let step = params.step.as_deref().unwrap_or("60s");

    // Build PromQL expression with optional label matchers
    let mut expr = params.metric.clone();
    let mut labels = Vec::new();
    if let Some(ref dev) = params.device {
        labels.push(format!("instance=~\"{}.*\"", dev));
    }
    if let Some(ref iface) = params.interface {
        labels.push(format!("interface=\"{}\"", iface));
    }
    if !labels.is_empty() && !expr.contains('{') {
        expr = format!("{}{{{}}}", expr, labels.join(","));
    }

    let url = format!("{base}/api/v1/query_range");
    let mut req = client.get(&url)
        .query(&[("query", &expr), ("start", &start.to_string()), ("end", &end.to_string()), ("step", &step.to_string())]);

    // Add auth if credential configured
    if !cfg.credential_alias.is_empty() {
        if let Ok(cred) = state.credentials.resolve(&cfg.credential_alias, crate::credentials::ResolvePurpose::Enrich) {
            req = req.basic_auth(&cred.username, Some(&*cred.password));
        }
    }

    let resp = req.send().await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("TSDB request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((StatusCode::BAD_GATEWAY, format!("TSDB returned {status}: {body}")));
    }

    resp.json::<serde_json::Value>().await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("TSDB response parse: {e}")))
}

async fn tsdb_influxdb_query(
    client: &reqwest::Client,
    cfg: &crate::config::TsdbConfig,
    state: &AppState,
    params: &TsdbQueryParams,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let base = cfg.query_url.trim_end_matches('/');
    let start = params.start.as_deref().unwrap_or(&cfg.default_lookback);

    // Build Flux query
    let mut flux = format!(
        "from(bucket: \"bonsai\") |> range(start: -{start}) |> filter(fn: (r) => r._measurement == \"{}\")",
        params.metric
    );
    if let Some(ref dev) = params.device {
        flux.push_str(&format!(" |> filter(fn: (r) => r.device == \"{}\")", dev));
    }
    if let Some(ref iface) = params.interface {
        flux.push_str(&format!(" |> filter(fn: (r) => r.interface == \"{}\")", iface));
    }

    let url = format!("{base}/api/v2/query");
    let mut req = client.post(&url)
        .header("Content-Type", "application/vnd.flux")
        .body(flux);

    if !cfg.credential_alias.is_empty() {
        if let Ok(cred) = state.credentials.resolve(&cfg.credential_alias, crate::credentials::ResolvePurpose::Enrich) {
            req = req.bearer_auth(&*cred.password);
        }
    }

    let resp = req.send().await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("InfluxDB request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err((StatusCode::BAD_GATEWAY, format!("InfluxDB returned {status}: {body}")));
    }

    let body = resp.text().await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("InfluxDB response: {e}")))?;

    // Wrap CSV/annotated response in JSON
    Ok(serde_json::json!({
        "status": "success",
        "tsdb_type": "influxdb",
        "raw": body,
    }))
}

// ── D4-7 T5: Unified settings API ──────────────────────────────────────────

/// GET /api/settings — list all available config sections and their DB status.
pub async fn list_settings_handler(
    State(state): State<super::AppState>,
) -> Json<serde_json::Value> {
    let items = state.store.list_config_items(Some("runtime_config".to_string())).await.unwrap_or_default();
    let sections: Vec<serde_json::Value> = RUNTIME_CONFIG_SECTIONS.iter().map(|s| {
        let in_db = items.iter().any(|i| i.name == *s && i.enabled);
        serde_json::json!({ "section": s, "in_db": in_db })
    }).collect();
    Json(serde_json::json!({ "sections": sections }))
}

/// GET /api/settings/:section — return the current value of a config section.
/// If stored in DB, returns the DB value; otherwise returns "not_in_db".
pub async fn get_settings_section_handler(
    State(state): State<super::AppState>,
    axum::extract::Path(section): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !RUNTIME_CONFIG_SECTIONS.contains(&section.as_str()) {
        return Err((StatusCode::NOT_FOUND, format!("unknown config section: {section}")));
    }
    let items = state.store.list_config_items(Some("runtime_config".to_string())).await.unwrap_or_default();
    if let Some(item) = items.iter().find(|i| i.name == section && i.enabled) {
        let val: serde_json::Value = serde_json::from_str(&item.content_json)
            .unwrap_or(serde_json::Value::String(item.content_json.clone()));
        Ok(Json(serde_json::json!({ "section": section, "source": "db", "value": val })))
    } else {
        Ok(Json(serde_json::json!({ "section": section, "source": "toml_default", "value": null })))
    }
}

/// PATCH /api/settings/:section — write a config section to DB.
/// Body is the JSON value for the section.
pub async fn patch_settings_section_handler(
    State(state): State<super::AppState>,
    axum::extract::Path(section): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !RUNTIME_CONFIG_SECTIONS.contains(&section.as_str()) {
        return Err((StatusCode::NOT_FOUND, format!("unknown config section: {section}")));
    }
    let json = serde_json::to_string(&body)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    if json.len() > 512 * 1024 {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "config section exceeds 512 KB limit".into()));
    }
    persist_runtime_section(&state.store, &section, &json).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("persist failed: {e}")))?;
    Ok(Json(serde_json::json!({ "ok": true, "section": section })))
}

/// POST /api/settings/export — dump all DB-stored runtime config as JSON.
pub async fn export_settings_handler(
    State(state): State<super::AppState>,
) -> Json<serde_json::Value> {
    let items = state.store.list_config_items(Some("runtime_config".to_string())).await.unwrap_or_default();
    let mut map = serde_json::Map::new();
    for item in &items {
        if !item.enabled { continue; }
        let val: serde_json::Value = serde_json::from_str(&item.content_json)
            .unwrap_or(serde_json::Value::String(item.content_json.clone()));
        map.insert(item.name.clone(), val);
    }
    Json(serde_json::Value::Object(map))
}

// ── TLS Certificate management ────────────────────────────────────────────────
//
// Certs are stored in the vault under alias `cert-{name}`.
// username = JSON metadata blob  { name, label, fingerprint_sha256, added_at_ns, pem_size }
// password = PEM bytes (the actual certificate text)
//
// Endpoints:
//   GET    /api/certs          — list all stored certs (no PEM in response)
//   POST   /api/certs          — store/replace a cert PEM (body: { name, label, pem })
//   GET    /api/certs/{name}   — fetch full PEM for a named cert
//   DELETE /api/certs/{name}   — remove a cert

fn cert_alias(name: &str) -> String {
    format!("cert-{}", name.trim().to_lowercase().replace(' ', "-"))
}

fn pem_fingerprint(pem: &str) -> String {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(pem.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CertMeta {
    pub name: String,
    pub label: String,
    pub fingerprint_sha256: String,
    pub added_at_ns: i64,
    pub pem_size: usize,
}

#[derive(Deserialize)]
pub struct UpsertCertRequest {
    pub name: String,
    #[serde(default)]
    pub label: String,
    pub pem: String,
}

/// GET /api/certs — list all vault-stored certificates (without PEM content).
pub async fn list_certs_handler(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let summaries = state.credentials.list().unwrap_or_default();
    let mut certs: Vec<CertMeta> = Vec::new();
    for s in &summaries {
        if !s.alias.starts_with("cert-") { continue; }
        let username = state.credentials.username_for_alias(&s.alias).unwrap_or_default();
        if let Ok(meta) = serde_json::from_str::<CertMeta>(&username) {
            certs.push(meta);
        }
    }
    certs.sort_by(|a, b| a.name.cmp(&b.name));
    Json(serde_json::json!({ "certs": certs }))
}

/// POST /api/certs — store or replace a TLS certificate PEM in the vault.
pub async fn upsert_cert_handler(
    State(state): State<AppState>,
    Json(body): Json<UpsertCertRequest>,
) -> Result<Json<CertMeta>, (StatusCode, String)> {
    if body.name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name is required".into()));
    }
    if body.pem.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "pem is required".into()));
    }
    if !body.pem.contains("-----BEGIN") {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "pem does not look like a valid PEM block".into()));
    }

    let alias = cert_alias(&body.name);
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

    let meta = CertMeta {
        name: body.name.clone(),
        label: if body.label.is_empty() { body.name.clone() } else { body.label.clone() },
        fingerprint_sha256: pem_fingerprint(&body.pem),
        added_at_ns: now_ns,
        pem_size: body.pem.len(),
    };
    let username_json = serde_json::to_string(&meta)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let exists = state.credentials.list().unwrap_or_default().iter().any(|s| s.alias == alias);
    let result = if exists {
        state.credentials.update(&alias, &username_json, &body.pem)
    } else {
        state.credentials.add(&alias, &username_json, &body.pem)
    };
    result.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tracing::info!(name = %body.name, alias = %alias, "cert stored in vault");
    Ok(Json(meta))
}

/// GET /api/certs/{name} — fetch the PEM for a named cert.
pub async fn get_cert_pem_handler(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let alias = cert_alias(&name);
    let cred = state.credentials
        .resolve(&alias, crate::credentials::ResolvePurpose::Internal)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(axum::response::Response::builder()
        .status(200)
        .header("content-type", "application/x-pem-file")
        .body(axum::body::Body::from(cred.password_string()))
        .unwrap())
}

/// DELETE /api/certs/{name} — remove a named cert from the vault.
pub async fn delete_cert_handler(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let alias = cert_alias(&name);
    state.credentials.remove(&alias)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(name = %name, "cert removed from vault");
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/certs/verify — check if a cert path (file or vault:name) is reachable.
/// Body: { "path": "vault:srl-lab-ca" } or { "path": "lab/ca.pem" }
pub async fn verify_cert_path_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if path.is_empty() {
        return Json(serde_json::json!({ "ok": false, "error": "path is required" }));
    }
    match crate::tls_util::verify_cert_path(&path, &state.credentials).await {
        Ok(source) => Json(serde_json::json!({ "ok": true, "source": source, "path": path })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string(), "path": path })),
    }
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Import security configuration
use crate::security::SecurityConfig;

#[derive(Deserialize)]
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub collector: CollectorConfig,
    pub graph_path: String,
    #[serde(default)]
    pub graph: GraphConfig,
    /// gRPC listen address for the Bonsai API server. Default: "[::1]:50051".
    #[serde(default = "default_api_addr")]
    pub api_addr: String,
    /// HTTP UI server listen address. Default: "0.0.0.0:3000".
    #[serde(default = "default_http_addr")]
    pub http_addr: String,
    /// Prometheus /metrics HTTP listener. Default: "[::1]:9090". Set to "" to disable.
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub event_bus: EventBusConfig,
    #[serde(default)]
    pub ingest: IngestConfig,
    #[serde(default)]
    pub archive: ArchiveConfig,
    #[serde(default)]
    pub credentials: CredentialsConfig,
    #[serde(default)]
    pub layered_ingestion: LayeredIngestionConfig,
    #[serde(default)]
    pub yang: YangConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub assignment: AssignmentConfig,
    #[serde(default)]
    pub integrations: IntegrationsConfig,
    #[serde(default)]
    pub remediation: RemediationConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub signals: SignalsConfig,
    #[serde(default)]
    pub streaming: StreamingConfig,
    #[serde(default)]
    pub lab: LabConfig,
    #[serde(default)]
    pub gnn: GnnConfig,
    #[serde(default)]
    pub target: Vec<TargetConfig>,
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default)]
    pub tls: HttpTlsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub security: SecurityConfig,
}

// ── Auth / LDAP (D4-3 T2/T3) ────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub ldap: LdapConfig,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct LdapConfig {
    /// Enable LDAP authentication. When false, only local users + env bootstrap.
    #[serde(default)]
    pub enabled: bool,
    /// LDAP server URL, e.g. "ldap://ldap.example.com:389" or "ldaps://ldap.example.com:636".
    #[serde(default)]
    pub server_url: String,
    /// Bind DN for LDAP search (service account).
    #[serde(default)]
    pub bind_dn: String,
    /// Environment variable containing the bind password.
    #[serde(default)]
    pub bind_password_env: String,
    /// Base DN for user search, e.g. "ou=users,dc=example,dc=com".
    #[serde(default)]
    pub user_search_base: String,
    /// User search filter. Use `{username}` as placeholder.
    /// Default: "(&(objectClass=person)(sAMAccountName={username}))"
    #[serde(default = "default_ldap_user_filter")]
    pub user_search_filter: String,
    /// Base DN for group search.
    #[serde(default)]
    pub group_search_base: String,
    /// Mapping of LDAP group CN → Bonsai role.
    /// Example: { "Network Admins" = "admin", "NOC" = "operator", "Viewers" = "viewer" }
    #[serde(default)]
    pub role_mapping: std::collections::HashMap<String, String>,
    /// Default role if no group mapping matches. Default: "viewer".
    #[serde(default = "default_ldap_role")]
    pub default_role: String,
    /// TLS: skip certificate verification (for self-signed certs). Default: false.
    #[serde(default)]
    pub tls_skip_verify: bool,
}

fn default_ldap_user_filter() -> String {
    "(&(objectClass=person)(sAMAccountName={username}))".to_string()
}
fn default_ldap_role() -> String {
    "viewer".to_string()
}

// ── HTTP TLS (D4-3 T7) ──────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug, Default)]
pub struct HttpTlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cert_path: String,
    #[serde(default)]
    pub key_path: String,
}

// ── GNN (D5-T4 DV1) ─────────────────────────────────────────────────────────

/// GNN inference mode configuration.
/// Set `inference_mode = "production"` after reviewing the 7-day calibration
/// score distribution. During calibration, scores are persisted to the
/// `gnn_calibration_scores` table but do not flow to the Detection table.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct GnnConfig {
    /// ``"calibration"`` — scores accumulate but no detections fire.
    /// ``"production"`` — scores above threshold produce Detection rows.
    /// Default: ``"calibration"``.
    #[serde(default = "default_gnn_inference_mode")]
    pub inference_mode: String,

    /// Anomaly score threshold above which a node is considered anomalous
    /// (production mode only). Default: 0.5.
    #[serde(default = "default_gnn_threshold")]
    pub threshold: f64,

    /// Minimum number of calibration-phase score samples before the operator
    /// can safely transition to production. Advisory only — bonsai does not
    /// block the transition. Default: 1000.
    #[serde(default = "default_gnn_min_calibration_samples")]
    pub min_calibration_samples: usize,
}

impl Default for GnnConfig {
    fn default() -> Self {
        Self {
            inference_mode: default_gnn_inference_mode(),
            threshold: default_gnn_threshold(),
            min_calibration_samples: default_gnn_min_calibration_samples(),
        }
    }
}

impl GnnConfig {
    pub fn is_calibration_mode(&self) -> bool {
        self.inference_mode.to_ascii_lowercase() == "calibration"
    }

    pub fn is_production_mode(&self) -> bool {
        self.inference_mode.to_ascii_lowercase() == "production"
    }
}

fn default_gnn_inference_mode() -> String {
    "calibration".to_string()
}

fn default_gnn_threshold() -> f64 {
    0.5
}

fn default_gnn_min_calibration_samples() -> usize {
    1000
}

// ── AI ───────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct AiConfig {
    /// AI provider to use. Default: "gemini". Options: "gemini", "moonshot", "anthropic", "openai".
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    /// Model name for the selected provider. Default: "gemini-2.5-pro".
    #[serde(default = "default_ai_model")]
    pub model: String,
    /// Environment variable name holding the API key. Default: "BONSAI_AI_API_KEY".
    #[serde(default = "default_ai_key_env")]
    pub api_key_env: String,
    /// Maximum cost (USD) per single investigation. Default: 0.10.
    #[serde(default = "default_ai_per_investigation_budget")]
    pub per_investigation_budget_usd: f64,
    /// Maximum total cost (USD) across all investigations per UTC day. Default: 1.00.
    #[serde(default = "default_ai_daily_budget")]
    pub daily_budget_usd: f64,
    /// When true and a DetectionEvent has no matching playbook, trigger an AI investigation automatically.
    #[serde(default)]
    pub auto_investigate_unmatched: bool,
    /// Optional custom base URL — used for Ollama (e.g. "http://localhost:11434") or Azure OpenAI.
    /// Leave empty for cloud providers (OpenAI/Anthropic/Gemini default endpoints).
    #[serde(default)]
    pub base_url: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: default_ai_provider(),
            model: default_ai_model(),
            api_key_env: default_ai_key_env(),
            per_investigation_budget_usd: default_ai_per_investigation_budget(),
            daily_budget_usd: default_ai_daily_budget(),
            auto_investigate_unmatched: false,
            base_url: String::new(),
        }
    }
}

fn default_ai_provider() -> String { "gemini".to_string() }
fn default_ai_model() -> String { "gemini-2.5-pro".to_string() }
fn default_ai_key_env() -> String { "BONSAI_AI_API_KEY".to_string() }
fn default_ai_per_investigation_budget() -> f64 { 0.10 }
fn default_ai_daily_budget() -> f64 { 1.00 }

// ── Lab ──────────────────────────────────────────────────────────────────────

/// Active lab identity. Declares the management subnet as an explicit config
/// key so scripts and tooling don't infer it from device addresses.
/// All fields are optional — omitting [lab] is valid for non-lab deployments.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct LabConfig {
    /// Active topology identifier: "dc" | "sp" | "fast-iteration" | "cloud-dc".
    #[serde(default)]
    pub topology: String,
    /// IPv4 management subnet for the active lab (e.g. "172.100.103.0/24").
    /// ContainerLab invariant: network name is always "bonsai-mgmt".
    #[serde(default)]
    pub mgmt_subnet: String,
    /// IPv6 management subnet for the active lab (e.g. "2001:db8:103::/64").
    #[serde(default)]
    pub mgmt_ipv6_subnet: String,
}

// ── Signals ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SignalsConfig {
    #[serde(default)]
    pub syslog: SyslogConfig,
    #[serde(default)]
    pub snmp: SnmpConfig,
}

#[derive(Deserialize, Clone, Debug)]
pub struct SnmpConfig {
    /// Enable the SNMP trap receiver. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// UDP listen address for traps. Use "0.0.0.0:162" in deployment; default avoids privileged port.
    #[serde(default = "default_snmp_udp_addr")]
    pub udp_addr: String,
    /// JSONL signal archive path for raw SNMP trap records.
    #[serde(default = "default_snmp_archive_path")]
    pub archive_path: String,
    /// Maximum accepted trap size in bytes. Default: 8192 bytes.
    #[serde(default = "default_snmp_max_frame_bytes")]
    pub max_frame_bytes: usize,
    /// Directory containing SNMP OID pattern YAML files for fact extraction.
    /// Defaults to "config/snmp_oid_patterns".
    #[serde(default)]
    pub oid_pattern_dir: Option<String>,
    /// Optional community string allowlist (v1/v2c). If non-empty, traps with a community
    /// not in this list are dropped (still archived) with a warn log. Empty list = accept all.
    #[serde(default)]
    pub community_allowlist: Vec<String>,
    /// SNMPv3 USM users. Each entry defines a user for v3 trap authentication.
    #[serde(default)]
    pub v3_users: Vec<SnmpV3User>,
}

/// SNMPv3 USM user configuration for trap receiver authentication.
#[derive(Deserialize, Clone, Debug)]
pub struct SnmpV3User {
    /// Security name (user name) as sent in the USM security parameters.
    pub security_name: String,
    /// Authentication protocol: "md5", "sha", "sha256", "sha512". Default: "sha".
    #[serde(default = "default_snmpv3_auth_protocol")]
    pub auth_protocol: String,
    /// Environment variable holding the authentication key (hex or passphrase).
    pub auth_key_env: String,
    /// Privacy/encryption protocol: "des", "aes128", "aes256". Default: "aes128".
    #[serde(default = "default_snmpv3_priv_protocol")]
    pub priv_protocol: String,
    /// Environment variable holding the privacy key. Empty = auth-only (no priv).
    #[serde(default)]
    pub priv_key_env: String,
}

fn default_snmpv3_auth_protocol() -> String {
    "sha".to_string()
}
fn default_snmpv3_priv_protocol() -> String {
    "aes128".to_string()
}

impl Default for SnmpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            udp_addr: default_snmp_udp_addr(),
            archive_path: default_snmp_archive_path(),
            max_frame_bytes: default_snmp_max_frame_bytes(),
            oid_pattern_dir: None,
            community_allowlist: Vec::new(),
            v3_users: Vec::new(),
        }
    }
}

fn default_snmp_udp_addr() -> String {
    "0.0.0.0:9162".to_string()
}
fn default_snmp_archive_path() -> String {
    "runtime/signals/snmp.jsonl".to_string()
}
fn default_snmp_max_frame_bytes() -> usize {
    8192
}

#[derive(Deserialize, Clone, Debug)]
pub struct SyslogConfig {
    /// Enable the syslog receiver. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// UDP listen address. Use "0.0.0.0:514" in deployment; default avoids privileged ports.
    #[serde(default = "default_syslog_udp_addr")]
    pub udp_addr: String,
    /// TCP listen address. Use "0.0.0.0:6514" for deployment-style testing.
    #[serde(default = "default_syslog_tcp_addr")]
    pub tcp_addr: String,
    /// JSONL signal archive path for raw + parsed syslog records.
    #[serde(default = "default_syslog_archive_path")]
    pub archive_path: String,
    /// Maximum accepted syslog frame size. Default: 8192 bytes.
    #[serde(default = "default_syslog_max_frame_bytes")]
    pub max_frame_bytes: usize,
}

impl Default for SyslogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            udp_addr: default_syslog_udp_addr(),
            tcp_addr: default_syslog_tcp_addr(),
            archive_path: default_syslog_archive_path(),
            max_frame_bytes: default_syslog_max_frame_bytes(),
        }
    }
}

fn default_syslog_udp_addr() -> String {
    "0.0.0.0:5514".to_string()
}
fn default_syslog_tcp_addr() -> String {
    "0.0.0.0:6514".to_string()
}
fn default_syslog_archive_path() -> String {
    "runtime/signals/syslog.jsonl".to_string()
}
fn default_syslog_max_frame_bytes() -> usize {
    8192
}

// ── Modern streaming protocols (CV2 Sprint 4) ───────────────────────────────

#[derive(Deserialize, Clone, Debug, Default)]
pub struct StreamingConfig {
    #[serde(default)]
    pub bmp: BmpConfig,
    #[serde(default)]
    pub bgp_ls: BgpLsConfig,
    #[serde(default)]
    pub pcep: PcepConfig,
    #[serde(default)]
    pub otlp: OtlpConfig,
    #[serde(default)]
    pub netflow: NetflowConfig,
    #[serde(default)]
    pub sflow: SflowConfig,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BmpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_bmp_tcp_addr")]
    pub tcp_addr: String,
    #[serde(default = "default_bmp_archive_path")]
    pub archive_path: String,
    #[serde(default = "default_bmp_max_frame_bytes")]
    pub max_frame_bytes: usize,
}

impl Default for BmpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tcp_addr: default_bmp_tcp_addr(),
            archive_path: default_bmp_archive_path(),
            max_frame_bytes: default_bmp_max_frame_bytes(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct BgpLsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// TCP listener receiving line-delimited JSON events from a GoBGP sidecar.
    #[serde(default = "default_bgp_ls_tcp_addr")]
    pub tcp_addr: String,
    #[serde(default = "default_bgp_ls_archive_path")]
    pub archive_path: String,
    #[serde(default = "default_bgp_ls_max_frame_bytes")]
    pub max_frame_bytes: usize,
}

impl Default for BgpLsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tcp_addr: default_bgp_ls_tcp_addr(),
            archive_path: default_bgp_ls_archive_path(),
            max_frame_bytes: default_bgp_ls_max_frame_bytes(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct PcepConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_pcep_tcp_addr")]
    pub tcp_addr: String,
    #[serde(default = "default_pcep_archive_path")]
    pub archive_path: String,
    #[serde(default = "default_pcep_max_frame_bytes")]
    pub max_frame_bytes: usize,
}

impl Default for PcepConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tcp_addr: default_pcep_tcp_addr(),
            archive_path: default_pcep_archive_path(),
            max_frame_bytes: default_pcep_max_frame_bytes(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct OtlpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_otlp_http_addr")]
    pub http_addr: String,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_addr: default_otlp_http_addr(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct NetflowConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_netflow_udp_addr")]
    pub udp_addr: String,
}

impl Default for NetflowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            udp_addr: default_netflow_udp_addr(),
        }
    }
}

fn default_otlp_http_addr() -> String {
    "0.0.0.0:4318".to_string()
}
fn default_netflow_udp_addr() -> String {
    "0.0.0.0:2055".to_string()
}
fn default_sflow_udp_addr() -> String {
    "0.0.0.0:6343".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct SflowConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sflow_udp_addr")]
    pub udp_addr: String,
}

impl Default for SflowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            udp_addr: default_sflow_udp_addr(),
        }
    }
}

fn default_bmp_tcp_addr() -> String {
    "0.0.0.0:5000".to_string()
}
fn default_bmp_archive_path() -> String {
    "runtime/streaming/bmp.jsonl".to_string()
}
fn default_bmp_max_frame_bytes() -> usize {
    65535
}
fn default_bgp_ls_tcp_addr() -> String {
    "127.0.0.1:15071".to_string()
}
fn default_bgp_ls_archive_path() -> String {
    "runtime/streaming/bgp_ls.jsonl".to_string()
}
fn default_bgp_ls_max_frame_bytes() -> usize {
    65535
}
fn default_pcep_tcp_addr() -> String {
    "0.0.0.0:4189".to_string()
}
fn default_pcep_archive_path() -> String {
    "runtime/streaming/pcep.jsonl".to_string()
}
fn default_pcep_max_frame_bytes() -> usize {
    65535
}

// ── Logging ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct LoggingConfig {
    /// Path for the rotating log file. Disabled (stderr only) when empty. Default: "".
    #[serde(default)]
    pub file_path: String,
    /// Rotation period: "daily" | "hourly" | "never". Default: "daily".
    #[serde(default = "default_log_rotation")]
    pub rotation: String,
    /// Number of days of rotated files to retain. Default: 7.
    #[serde(default = "default_log_retention_days")]
    pub retention_days: u32,
    /// Log level for stderr and file appender. Default: "info".
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Minimum disk free bytes at log file path required to start. Default: 5 GiB (0 = skip).
    #[serde(default = "default_log_min_free_bytes")]
    pub min_free_bytes: u64,
    /// Per-module level overrides, e.g. {"bonsai::ingest" = "debug"}.
    #[serde(default)]
    pub targets: HashMap<String, String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file_path: String::new(),
            rotation: default_log_rotation(),
            retention_days: default_log_retention_days(),
            level: default_log_level(),
            min_free_bytes: default_log_min_free_bytes(),
            targets: HashMap::new(),
        }
    }
}

fn default_log_rotation() -> String {
    "daily".to_string()
}
fn default_log_retention_days() -> u32 {
    7
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_min_free_bytes() -> u64 {
    5 * 1024 * 1024 * 1024
}

// ── Remediation ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct RemediationConfig {
    /// Seconds an AutoWithNotification execution stays eligible for rollback. Default: 60.
    #[serde(default = "default_rollback_window_secs")]
    pub rollback_window_secs: u64,
    #[serde(default)]
    pub graduation: GraduationConfig,
    #[serde(default)]
    pub defaults: RemediationDefaultsConfig,
    #[serde(default)]
    pub rule_defaults: HashMap<String, RemediationDefaultsConfig>,
    /// When true, a RemediationProposal is created automatically for every
    /// DetectionEvent that has a matching entry in the playbook library.
    #[serde(default)]
    pub auto_propose: bool,
    /// Directory containing YAML playbook files keyed by detection_rule_id.
    #[serde(default = "default_playbook_library_dir")]
    pub playbook_library_dir: String,
}

impl Default for RemediationConfig {
    fn default() -> Self {
        Self {
            rollback_window_secs: default_rollback_window_secs(),
            graduation: GraduationConfig::default(),
            defaults: RemediationDefaultsConfig::default(),
            rule_defaults: HashMap::new(),
            auto_propose: false,
            playbook_library_dir: default_playbook_library_dir(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct GraduationConfig {
    /// Consecutive operator approvals required before a graduation hint is surfaced. Default: 10.
    #[serde(default = "default_graduation_approvals")]
    pub consecutive_approvals_required: u32,
}

impl Default for GraduationConfig {
    fn default() -> Self {
        Self {
            consecutive_approvals_required: default_graduation_approvals(),
        }
    }
}

/// Per-archetype default TrustState for new (rule, env, site, playbook) tuples.
/// Values: "suggest_only" | "approve_each" | "auto_with_notification" | "auto_silent".
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct RemediationDefaultsConfig {
    #[serde(default)]
    pub home_lab: String,
    #[serde(default)]
    pub data_center: String,
    #[serde(default)]
    pub service_provider: String,
    #[serde(default)]
    pub campus_wired: String,
    #[serde(default)]
    pub campus_wireless: String,
}

fn default_rollback_window_secs() -> u64 {
    60
}
fn default_playbook_library_dir() -> String {
    "playbooks/library".to_string()
}
fn default_graduation_approvals() -> u32 {
    10
}

// ── Integrations ──────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct IntegrationsConfig {
    #[serde(default)]
    pub servicenow: ServiceNowConfig,
    /// D4-5 T4: External TSDB for historical time-series queries.
    #[serde(default)]
    pub tsdb: TsdbConfig,
}

/// D4-5 T4: External TSDB integration for historical metric queries.
/// Graph is the live truth layer; TSDB provides the historical time-series layer.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct TsdbConfig {
    /// Enable TSDB integration.
    #[serde(default)]
    pub enabled: bool,
    /// TSDB backend type: "prometheus", "victoria_metrics", "influxdb", "thanos".
    #[serde(default = "default_tsdb_type")]
    pub tsdb_type: String,
    /// Query endpoint URL, e.g. "http://localhost:9090" for Prometheus.
    #[serde(default)]
    pub query_url: String,
    /// Vault credential alias for TSDB authentication (basic auth or token).
    #[serde(default)]
    pub credential_alias: String,
    /// Default lookback window for queries when not specified. Default: "1h".
    #[serde(default = "default_tsdb_lookback")]
    pub default_lookback: String,
    /// Maximum query range to prevent expensive queries. Default: "24h".
    #[serde(default = "default_tsdb_max_range")]
    pub max_range: String,
}

fn default_tsdb_type() -> String { "prometheus".to_string() }
fn default_tsdb_lookback() -> String { "1h".to_string() }
fn default_tsdb_max_range() -> String { "24h".to_string() }

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct ServiceNowConfig {
    /// Enable ServiceNow integration. Requires `instance_url` + `credential_alias`.
    #[serde(default)]
    pub enabled: bool,
    /// PDI or production instance URL, e.g. "https://dev12345.service-now.com".
    #[serde(default)]
    pub instance_url: String,
    /// Vault alias for ServiceNow credentials (username + password).
    #[serde(default)]
    pub credential_alias: String,
    /// Enable periodic push of detection events to ServiceNow Event Management.
    #[serde(default)]
    pub em_push_enabled: bool,
    #[serde(default)]
    pub event_filter: ServiceNowEventFilterConfig,
    #[serde(default)]
    pub aiops: ServiceNowAiopsConfig,
    #[serde(default)]
    pub change_management: ServiceNowChangeManagementConfig,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ServiceNowEventFilterConfig {
    /// Minimum severity to push: "critical" | "warning" | "info". Default: "warning".
    #[serde(default = "default_snow_min_severity")]
    pub min_severity: String,
    /// Detection must be at least this old (seconds) before it is pushed. Default: 60.
    #[serde(default = "default_snow_min_age_secs")]
    pub min_age_secs: u64,
    /// Suppress a (device, rule_id) pair if it was already pushed within this window (seconds). Default: 300.
    #[serde(default = "default_snow_dedup_window_secs")]
    pub dedup_window_secs: u64,
}

impl Default for ServiceNowEventFilterConfig {
    fn default() -> Self {
        Self {
            min_severity: default_snow_min_severity(),
            min_age_secs: default_snow_min_age_secs(),
            dedup_window_secs: default_snow_dedup_window_secs(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ServiceNowAiopsConfig {
    /// Enable Sprint 6 incident sync and playbook bridge.
    #[serde(default)]
    pub enabled: bool,
    /// Background sync cadence in seconds.
    #[serde(default = "default_snow_aiops_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Detections newer than this window are treated as active incidents.
    #[serde(default = "default_snow_aiops_active_window_secs")]
    pub active_window_secs: u64,
    /// Time window used to correlate detections into one incident.
    #[serde(default = "default_snow_aiops_correlation_window_secs")]
    pub correlation_window_secs: u64,
    /// Table name used for ITSM incidents. Default: `incident`.
    #[serde(default = "default_snow_aiops_incident_table")]
    pub incident_table: String,
    /// Numeric ServiceNow state used for open incidents. Default: `1` (New).
    #[serde(default = "default_snow_aiops_open_state")]
    pub open_state: String,
    /// Numeric ServiceNow state used when Bonsai auto-resolves. Default: `6` (Resolved).
    #[serde(default = "default_snow_aiops_resolved_state")]
    pub resolved_state: String,
    /// Optional fallback assignment group name/sys_id when the device has no CMDB-derived group.
    #[serde(default)]
    pub assignment_group_fallback: String,
    /// Maximum physical hop depth used for blast-radius context.
    #[serde(default = "default_snow_aiops_max_blast_radius_hops")]
    pub max_blast_radius_hops: usize,
    /// If true, Bonsai will resolve previously-synced incidents when detections go quiet.
    #[serde(default = "default_snow_aiops_auto_clear")]
    pub auto_clear: bool,
    /// If true, parse ServiceNow comments/work notes for `bonsai:playbook <id>` commands.
    #[serde(default = "default_snow_aiops_playbook_bridge_enabled")]
    pub playbook_bridge_enabled: bool,
}

impl Default for ServiceNowAiopsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: default_snow_aiops_poll_interval_secs(),
            active_window_secs: default_snow_aiops_active_window_secs(),
            correlation_window_secs: default_snow_aiops_correlation_window_secs(),
            incident_table: default_snow_aiops_incident_table(),
            open_state: default_snow_aiops_open_state(),
            resolved_state: default_snow_aiops_resolved_state(),
            assignment_group_fallback: String::new(),
            max_blast_radius_hops: default_snow_aiops_max_blast_radius_hops(),
            auto_clear: default_snow_aiops_auto_clear(),
            playbook_bridge_enabled: default_snow_aiops_playbook_bridge_enabled(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ServiceNowChangeManagementConfig {
    /// Enable polling ServiceNow change_request table.
    #[serde(default)]
    pub enabled: bool,
    /// Poll interval in seconds. Default: 120.
    #[serde(default = "default_snow_chg_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// ServiceNow table for change requests. Default: `change_request`.
    #[serde(default = "default_snow_chg_table")]
    pub change_table: String,
    /// Lookback window: fetch changes scheduled to start within this many hours. Default: 24.
    #[serde(default = "default_snow_chg_lookback_hours")]
    pub lookback_hours: u64,
    /// How to handle detections that fire during an active change window.
    /// "annotate" = tag detection with change_correlated but keep it (default).
    /// "suppress" = skip creating the detection entirely.
    #[serde(default = "default_snow_chg_suppression_policy")]
    pub suppression_policy: String,
    /// Also accept changes from external webhooks (AAP, Ansible Tower, etc.).
    #[serde(default)]
    pub webhook_enabled: bool,
    /// Shared secret for webhook HMAC validation. Env var name that holds the secret.
    #[serde(default)]
    pub webhook_secret_env: String,
}

impl Default for ServiceNowChangeManagementConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_secs: default_snow_chg_poll_interval_secs(),
            change_table: default_snow_chg_table(),
            lookback_hours: default_snow_chg_lookback_hours(),
            suppression_policy: default_snow_chg_suppression_policy(),
            webhook_enabled: false,
            webhook_secret_env: String::new(),
        }
    }
}

fn default_snow_chg_poll_interval_secs() -> u64 {
    120
}
fn default_snow_chg_table() -> String {
    "change_request".to_string()
}
fn default_snow_chg_lookback_hours() -> u64 {
    24
}
fn default_snow_chg_suppression_policy() -> String {
    "annotate".to_string()
}

fn default_snow_min_severity() -> String {
    "warning".to_string()
}
fn default_snow_min_age_secs() -> u64 {
    60
}
fn default_snow_dedup_window_secs() -> u64 {
    300
}
fn default_snow_aiops_poll_interval_secs() -> u64 {
    120
}
fn default_snow_aiops_active_window_secs() -> u64 {
    900
}
fn default_snow_aiops_correlation_window_secs() -> u64 {
    30
}
fn default_snow_aiops_incident_table() -> String {
    "incident".to_string()
}
fn default_snow_aiops_open_state() -> String {
    "1".to_string()
}
fn default_snow_aiops_resolved_state() -> String {
    "6".to_string()
}
fn default_snow_aiops_max_blast_radius_hops() -> usize {
    2
}
fn default_snow_aiops_auto_clear() -> bool {
    true
}
fn default_snow_aiops_playbook_bridge_enabled() -> bool {
    true
}

/// Auto-assignment rules: when a device has no explicit collector_id, these
/// rules are evaluated in descending priority order to select a collector.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct AssignmentConfig {
    #[serde(default)]
    pub rules: Vec<AssignmentRule>,
}

/// A single routing rule. Higher `priority` wins when multiple rules match.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct AssignmentRule {
    /// Site name the device must belong to. Required.
    pub match_site: String,
    /// Optional device role filter (e.g. "leaf", "spine"). Omit to match any role.
    pub match_role: Option<String>,
    /// Collector ID to assign when this rule matches.
    pub collector_id: String,
    /// Tiebreak when multiple rules match the same device. Higher wins. Default: 0.
    #[serde(default)]
    pub priority: i32,
}

#[derive(Deserialize, Clone, Default)]
pub struct CollectorConfig {
    #[serde(default = "default_collector_graph_path")]
    pub graph_path: String,
    #[serde(default)]
    pub queue: CollectorQueueConfig,
    #[serde(default)]
    pub filter: CollectorFilterConfig,
    /// TCP port for the collector diagnostic HTTP server. Disabled when 0 (default).
    /// Endpoints: /health, /api/readiness, /api/collector/status
    /// Optional auth via BONSAI_COLLECTOR_DIAG_PASSWORD env var.
    #[serde(default)]
    pub diagnostic_port: u16,
}

impl CollectorConfig {
    pub fn default_with_paths() -> Self {
        Self {
            graph_path: default_collector_graph_path(),
            ..Default::default()
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CollectorFilterConfig {
    /// Minimum interval between counter forwards per (device, interface). Default: 10s.
    #[serde(default = "default_debounce_secs")]
    pub counter_debounce_secs: u64,
    /// Forwarding mode: "raw" (no filtering), "debounced" (drops updates within window),
    /// "summary" (aggregate into time-windowed summaries, recommended for distributed mode).
    #[serde(default = "default_counter_forward_mode")]
    pub counter_forward_mode: String,
    /// Summary window duration in seconds. Only used when counter_forward_mode = "summary".
    #[serde(default = "default_counter_window_secs")]
    pub counter_window_secs: u64,
    /// Seconds of silence after which a partial summary window is flushed. Default: window + 10.
    #[serde(default = "default_counter_flush_idle_secs")]
    pub counter_flush_idle_secs: u64,
}

impl Default for CollectorFilterConfig {
    fn default() -> Self {
        Self {
            counter_debounce_secs: default_debounce_secs(),
            counter_forward_mode: default_counter_forward_mode(),
            counter_window_secs: default_counter_window_secs(),
            counter_flush_idle_secs: default_counter_flush_idle_secs(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CollectorQueueConfig {
    /// Directory containing append-only collector queue files.
    #[serde(default = "default_collector_queue_path")]
    pub path: String,
    /// Maximum queue bytes before oldest unacked records are dropped. 0 = unlimited.
    #[serde(default = "default_collector_queue_max_bytes")]
    pub max_bytes: u64,
    /// Drop records older than this many hours. 0 = unlimited.
    #[serde(default = "default_collector_queue_max_age_hours")]
    pub max_age_hours: u64,
    /// Maximum records sent in one client-streaming replay.
    #[serde(default = "default_collector_queue_drain_batch_size")]
    pub drain_batch_size: usize,
    /// Periodic operator visibility interval. 0 disables periodic queue logs.
    #[serde(default = "default_collector_queue_log_interval_seconds")]
    pub log_interval_seconds: u64,
}

impl Default for CollectorQueueConfig {
    fn default() -> Self {
        Self {
            path: default_collector_queue_path(),
            max_bytes: default_collector_queue_max_bytes(),
            max_age_hours: default_collector_queue_max_age_hours(),
            drain_batch_size: default_collector_queue_drain_batch_size(),
            log_interval_seconds: default_collector_queue_log_interval_seconds(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct RuntimeConfig {
    /// One binary, three modes: "all" (default), "core", or "collector".
    #[serde(default = "default_runtime_mode")]
    pub mode: String,
    /// Optional override for the auto-probed resource profile: tiny|small|medium|large|xlarge.
    #[serde(default)]
    pub resource_profile: Option<String>,
    /// Stable collector identity added to TelemetryIngest records.
    #[serde(default = "default_collector_id")]
    pub collector_id: String,
    /// Core gRPC endpoint used by collector mode.
    #[serde(default = "default_core_ingest_endpoint")]
    pub core_ingest_endpoint: String,
    /// Optional TLS/mTLS settings for the distributed collector-core channel.
    #[serde(default)]
    pub tls: RuntimeTlsConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            mode: default_runtime_mode(),
            resource_profile: None,
            collector_id: default_collector_id(),
            core_ingest_endpoint: default_core_ingest_endpoint(),
            tls: RuntimeTlsConfig::default(),
        }
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct RuntimeTlsConfig {
    /// Enables TLS on the core listener and mTLS on collector connections.
    #[serde(default)]
    pub enabled: bool,
    /// CA certificate used by collectors to verify the core and by cores to verify collectors.
    pub ca_cert: Option<String>,
    /// Local certificate chain presented by this process.
    pub cert: Option<String>,
    /// Local private key presented by this process.
    pub key: Option<String>,
    /// Server name collectors use when verifying the core certificate.
    pub server_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    All,
    Core,
    Collector,
}

impl RuntimeMode {
    pub fn runs_core(self) -> bool {
        matches!(self, RuntimeMode::All | RuntimeMode::Core)
    }

    pub fn runs_collector(self) -> bool {
        matches!(self, RuntimeMode::All | RuntimeMode::Collector)
    }
}

impl RuntimeConfig {
    pub fn parsed_mode(&self) -> Result<RuntimeMode> {
        match self.mode.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(RuntimeMode::All),
            "core" => Ok(RuntimeMode::Core),
            "collector" => Ok(RuntimeMode::Collector),
            other => anyhow::bail!(
                "invalid runtime.mode '{other}' - expected one of: all, core, collector"
            ),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct RetentionConfig {
    /// Enable periodic pruning of old StateChangeEvents. Default: true.
    #[serde(default = "default_retention_enabled")]
    pub enabled: bool,
    /// Delete StateChangeEvents older than this many hours. Default: 24.
    #[serde(default = "default_retention_hours")]
    pub max_age_hours: u64,
    /// Hard cap on total StateChangeEvents kept. 0 = unlimited. Default: 10000.
    /// When the count exceeds this, oldest events are deleted to get back under the limit.
    #[serde(default = "default_max_state_change_events")]
    pub max_state_change_events: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            enabled: default_retention_enabled(),
            max_age_hours: default_retention_hours(),
            max_state_change_events: default_max_state_change_events(),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct EventBusConfig {
    /// broadcast channel capacity. Default: 2048.
    #[serde(default = "default_bus_capacity")]
    pub capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            capacity: default_bus_capacity(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct IngestConfig {
    /// Minimum interval between counter writes per (device, interface). Default: 10s.
    #[serde(default = "default_debounce_secs")]
    pub counter_debounce_secs: u64,
    #[serde(default)]
    pub backpressure: BackpressureConfig,
    /// Total RAM budget for the three ingest debounce LRU caches combined. Default: 16 MiB.
    /// Caps are computed as (memory_bytes / 3) / per_entry_size_estimate.
    #[serde(default = "default_debounce_memory_bytes")]
    pub debounce_memory_bytes: usize,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct BackpressureConfig {
    #[serde(default = "default_bp_level_1_pct")]
    pub level_1_pct: u64,
    #[serde(default = "default_bp_level_2_pct")]
    pub level_2_pct: u64,
    #[serde(default)]
    pub exemptions: Vec<String>,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            level_1_pct: default_bp_level_1_pct(),
            level_2_pct: default_bp_level_2_pct(),
            exemptions: vec![],
        }
    }
}

fn default_bp_level_1_pct() -> u64 {
    75
}

fn default_bp_level_2_pct() -> u64 {
    90
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self {
            counter_debounce_secs: default_debounce_secs(),
            backpressure: BackpressureConfig::default(),
            debounce_memory_bytes: default_debounce_memory_bytes(),
        }
    }
}

fn default_debounce_memory_bytes() -> usize {
    16 * 1024 * 1024 // 16 MiB
}

#[derive(Deserialize, Serialize)]
pub struct ArchiveConfig {
    /// Enable the Parquet archive consumer. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Root directory for parquet archive output. Default: "archive".
    #[serde(default = "default_archive_path")]
    pub path: String,
    /// Flush buffered rows every N seconds. Default: 10.
    #[serde(default = "default_archive_flush_interval_seconds")]
    pub flush_interval_seconds: u64,
    /// Flush immediately when the in-memory batch reaches this size. Default: 1000.
    #[serde(default = "default_archive_max_batch_rows")]
    pub max_batch_rows: usize,
    /// ZSTD compression level for Parquet files. 1 = fastest, 22 = best. Default: 12.
    #[serde(default = "default_archive_compression_level")]
    pub compression_level: u32,
    /// Close idle partition writers after this many seconds of inactivity. Default: 7200 (2h).
    #[serde(default = "default_archive_writer_max_idle_secs")]
    pub writer_max_idle_secs: u64,
    /// Force-rotate partition writers older than this many seconds regardless of idle state.
    /// Ensures at least one closed Parquet file per interval even during continuous ingest. Default: 3600 (1h).
    #[serde(default = "default_archive_max_file_age_secs")]
    pub max_file_age_seconds: u64,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_archive_path(),
            flush_interval_seconds: default_archive_flush_interval_seconds(),
            max_batch_rows: default_archive_max_batch_rows(),
            compression_level: default_archive_compression_level(),
            writer_max_idle_secs: default_archive_writer_max_idle_secs(),
            max_file_age_seconds: default_archive_max_file_age_secs(),
        }
    }
}

/// Disk-usage guard for the archive and graph database directories.
#[derive(Deserialize, Serialize, Clone)]
pub struct StorageConfig {
    /// Maximum bytes the archive directory may use before aggressive retention kicks in.
    /// 0 = unlimited. Default: 10 GB.
    #[serde(default = "default_max_archive_bytes")]
    pub max_archive_bytes: u64,
    /// Maximum bytes the graph database directory may use.
    /// 0 = unlimited. Default: 5 GB.
    #[serde(default = "default_max_graph_bytes")]
    pub max_graph_bytes: u64,
    /// How often (seconds) to check disk usage. Default: 300 (5 min).
    #[serde(default = "default_disk_check_interval_secs")]
    pub check_interval_secs: u64,
    /// Log a warning when usage exceeds this percentage of the configured max. Default: 80.
    #[serde(default = "default_warn_threshold_pct")]
    pub warn_threshold_pct: u8,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_archive_bytes: default_max_archive_bytes(),
            max_graph_bytes: default_max_graph_bytes(),
            check_interval_secs: default_disk_check_interval_secs(),
            warn_threshold_pct: default_warn_threshold_pct(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct CredentialsConfig {
    /// Directory containing vault.age and metadata.json. Default: "bonsai-credentials".
    #[serde(default = "default_credentials_path")]
    pub path: String,
    /// Environment variable containing the vault passphrase for this backend slice.
    #[serde(default = "default_credentials_passphrase_env")]
    pub passphrase_env: String,
}

impl Default for CredentialsConfig {
    fn default() -> Self {
        Self {
            path: default_credentials_path(),
            passphrase_env: default_credentials_passphrase_env(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct LayeredIngestionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_layered_ingestion_store_path")]
    pub config_store_path: String,
    #[serde(default = "default_credentials_passphrase_env")]
    pub config_store_passphrase_env: String,
    #[serde(default = "default_change_detection_schedule_interval_secs")]
    pub change_detection_schedule_interval_secs: u64,
    #[serde(default = "default_change_detection_reparse_interval_secs")]
    pub change_detection_reparse_interval_secs: u64,
    #[serde(default = "default_change_detection_history_limit")]
    pub history_limit: usize,
    #[serde(default)]
    pub default_gnmi_get_paths: Vec<String>,
    #[serde(default)]
    pub parser_chain: ParserChainConfig,
    #[serde(default = "default_gnmi_known_issues_path")]
    pub gnmi_known_issues_path: String,
    #[serde(default = "default_syslog_patterns_path")]
    pub syslog_patterns_path: String,
}

impl Default for LayeredIngestionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            config_store_path: default_layered_ingestion_store_path(),
            config_store_passphrase_env: default_credentials_passphrase_env(),
            change_detection_schedule_interval_secs:
                default_change_detection_schedule_interval_secs(),
            change_detection_reparse_interval_secs: default_change_detection_reparse_interval_secs(
            ),
            history_limit: default_change_detection_history_limit(),
            default_gnmi_get_paths: Vec::new(),
            parser_chain: ParserChainConfig::default(),
            gnmi_known_issues_path: default_gnmi_known_issues_path(),
            syslog_patterns_path: default_syslog_patterns_path(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct YangConfig {
    #[serde(default = "default_yang_library_root")]
    pub library_root: String,
    #[serde(default = "default_yang_cache_root")]
    pub cache_root: String,
    #[serde(default = "default_yang_bundle_key_env")]
    pub bundle_key_env: String,
}

impl Default for YangConfig {
    fn default() -> Self {
        Self {
            library_root: default_yang_library_root(),
            cache_root: default_yang_cache_root(),
            bundle_key_env: default_yang_bundle_key_env(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct ParserChainConfig {
    #[serde(default)]
    pub sidecars: ParserSidecarConfig,
    /// Key format: "<vendor>::<command_pattern>"
    #[serde(default)]
    pub priorities: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub consensus_mode: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ParserSidecarConfig {
    #[serde(default = "default_pyats_sidecar_url")]
    pub pyats_url: String,
    #[serde(default = "default_native_parser_url")]
    pub native_url: String,
}

impl Default for ParserSidecarConfig {
    fn default() -> Self {
        Self {
            pyats_url: default_pyats_sidecar_url(),
            native_url: default_native_parser_url(),
        }
    }
}

// ── Graph database ────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct GraphConfig {
    /// LadybugDB buffer pool size in bytes.
    /// Default: min(2 GiB, 25 % of system RAM) for core; min(256 MiB, 10 %) for collector.
    /// Set explicitly in [graph] to override the auto-detected default.
    pub buffer_pool_bytes: Option<u64>,
}

/// Detect physical RAM size by reading /proc/meminfo on Linux; returns 8 GiB fallback elsewhere.
fn detect_system_ram_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
            for line in content.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:")
                    && let Some(kb_str) = rest.split_whitespace().next()
                    && let Ok(kb) = kb_str.parse::<u64>()
                    && kb > 0
                {
                    return kb * 1024;
                }
            }
        }
    }
    8 * 1024 * 1024 * 1024
}

/// Return the buffer pool size to pass to LadybugDB for the core (graph-writer) process.
/// Uses the operator-configured value if set; otherwise min(2 GiB, 25 % of RAM).
pub fn resolve_buffer_pool_core(configured: Option<u64>) -> u64 {
    configured.unwrap_or_else(|| {
        const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;
        std::cmp::min(TWO_GIB, detect_system_ram_bytes() / 4)
    })
}

/// Return the buffer pool size for the collector (smaller graph, much less data).
/// Uses the operator-configured value if set; otherwise min(256 MiB, 10 % of RAM).
pub fn resolve_buffer_pool_collector(configured: Option<u64>) -> u64 {
    configured.unwrap_or_else(|| {
        const MB_256: u64 = 256 * 1024 * 1024;
        std::cmp::min(MB_256, detect_system_ram_bytes() / 10)
    })
}

fn default_retention_enabled() -> bool {
    true
}

fn default_retention_hours() -> u64 {
    24
}

fn default_max_state_change_events() -> u64 {
    10_000
}

fn default_bus_capacity() -> usize {
    512
}

fn default_debounce_secs() -> u64 {
    10
}

fn default_archive_path() -> String {
    "archive".to_string()
}

fn default_archive_flush_interval_seconds() -> u64 {
    10
}

fn default_archive_max_batch_rows() -> usize {
    1000
}

fn default_archive_compression_level() -> u32 {
    12
}

fn default_archive_writer_max_idle_secs() -> u64 {
    7200
}

fn default_archive_max_file_age_secs() -> u64 {
    3600
}

fn default_max_archive_bytes() -> u64 {
    10 * 1024 * 1024 * 1024 // 10 GB
}

fn default_max_graph_bytes() -> u64 {
    5 * 1024 * 1024 * 1024 // 5 GB
}

fn default_disk_check_interval_secs() -> u64 {
    300
}

fn default_warn_threshold_pct() -> u8 {
    80
}

fn default_credentials_path() -> String {
    "bonsai-credentials".to_string()
}

fn default_credentials_passphrase_env() -> String {
    "BONSAI_VAULT_PASSPHRASE".to_string()
}

fn default_layered_ingestion_store_path() -> String {
    "runtime/config-store".to_string()
}

fn default_change_detection_schedule_interval_secs() -> u64 {
    3600
}

fn default_change_detection_reparse_interval_secs() -> u64 {
    7 * 24 * 3600
}

fn default_change_detection_history_limit() -> usize {
    25
}

fn default_gnmi_known_issues_path() -> String {
    "config/gnmi_known_issues/default.yaml".to_string()
}

fn default_syslog_patterns_path() -> String {
    "config/syslog_patterns".to_string()
}

fn default_pyats_sidecar_url() -> String {
    "http://127.0.0.1:9101".to_string()
}

fn default_native_parser_url() -> String {
    "http://127.0.0.1:9102".to_string()
}

fn default_yang_library_root() -> String {
    "runtime/yang_catalogue".to_string()
}

fn default_yang_cache_root() -> String {
    "runtime/yang_cache".to_string()
}

fn default_yang_bundle_key_env() -> String {
    "BONSAI_YANG_BUNDLE_KEY".to_string()
}

fn default_counter_forward_mode() -> String {
    "debounced".to_string()
}

fn default_counter_window_secs() -> u64 {
    60
}

fn default_counter_flush_idle_secs() -> u64 {
    70
}

fn default_collector_queue_path() -> String {
    "runtime/collector-queue".to_string()
}

fn default_collector_graph_path() -> String {
    "runtime/collector.db".to_string()
}

fn default_collector_queue_max_bytes() -> u64 {
    1_073_741_824
}

fn default_collector_queue_max_age_hours() -> u64 {
    24
}

fn default_collector_queue_drain_batch_size() -> usize {
    1_000
}

fn default_collector_queue_log_interval_seconds() -> u64 {
    30
}

fn default_runtime_mode() -> String {
    "all".to_string()
}

fn default_collector_id() -> String {
    "local".to_string()
}

fn default_core_ingest_endpoint() -> String {
    "http://[::1]:50051".to_string()
}

fn default_target_enabled() -> bool {
    true
}

fn default_api_addr() -> String {
    "[::1]:50051".to_string()
}

fn default_http_addr() -> String {
    "0.0.0.0:3000".to_string()
}

fn default_metrics_addr() -> String {
    "[::1]:9090".to_string()
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct TargetConfig {
    pub address: String,
    /// Whether the runtime subscriber should be running for this target.
    #[serde(default = "default_target_enabled")]
    pub enabled: bool,
    /// TLS server name (SNI). Required when ca_cert is set.
    pub tls_domain: Option<String>,
    /// Path to PEM CA cert. Enables TLS for this target.
    pub ca_cert: Option<String>,
    /// Override vendor detection. If absent, Capabilities RPC auto-detects.
    pub vendor: Option<String>,
    /// Alias into the local encrypted credential vault.
    pub credential_alias: Option<String>,
    /// Env var name whose value is the username. Takes precedence over `username`.
    pub username_env: Option<String>,
    /// Env var name whose value is the password. Takes precedence over `password`.
    pub password_env: Option<String>,
    /// Inline username — lab use only; bonsai.toml must not be committed with real creds.
    pub username: Option<String>,
    /// Inline password — lab use only.
    pub password: Option<String>,
    /// Human-readable device hostname for graph indexing (e.g. "srl1").
    /// Used to match LLDP system-name when building CONNECTED_TO edges.
    pub hostname: Option<String>,
    /// Logical role hint for future onboarding/profile selection (e.g. "leaf", "spine", "pe").
    pub role: Option<String>,
    /// Site label for future topology grouping and TSDB/graph enrichment.
    pub site: Option<String>,
    /// The stable ID of the collector responsible for this device.
    pub collector_id: Option<String>,
    /// Operator-selected subscription paths from onboarding discovery.
    #[serde(default)]
    pub selected_paths: Vec<SelectedSubscriptionPath>,
    /// Flat list of gNMI paths for this device (used by SQLite store and replication).
    #[serde(default)]
    pub paths: Vec<String>,
    /// Whether this device is optional (absence of subscription not fatal).
    #[serde(default)]
    pub optional: bool,
    /// Audit metadata for runtime-managed devices. Seed/config-driven targets may leave these unset.
    #[serde(default)]
    pub created_at_ns: i64,
    #[serde(default)]
    pub updated_at_ns: i64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub updated_by: String,
    #[serde(default)]
    pub last_operator_action: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
pub struct SelectedSubscriptionPath {
    pub path: String,
    #[serde(default)]
    pub origin: String,
    pub mode: String,
    #[serde(default)]
    pub sample_interval_ns: u64,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub optional: bool,
}

impl TargetConfig {
    pub fn resolved_username(&self) -> Option<String> {
        if let Some(ref key) = self.username_env {
            return std::env::var(key).ok();
        }
        self.username.clone()
    }

    pub fn resolved_password(&self) -> Option<String> {
        if let Some(ref key) = self.password_env {
            return std::env::var(key).ok();
        }
        self.password.clone()
    }

    pub fn uses_tls(&self) -> bool {
        self.ca_cert.is_some()
    }
}

// ── D4-7 T5: Env var consolidation helpers ──────────────────────────────────

/// Resolve a secret value from the credential vault, falling back to env var.
/// Priority: vault alias → env var → None.
/// Use for: API keys, admin passwords, LDAP bind password, SNMP keys, JWT secret.
pub fn resolve_secret(
    vault: &crate::credentials::CredentialVault,
    vault_alias: &str,
    env_var: &str,
) -> Option<String> {
    // Try vault first
    if !vault_alias.is_empty() {
        if let Ok(cred) = vault.resolve(vault_alias, crate::credentials::ResolvePurpose::Internal) {
            return Some(cred.password);
        }
    }
    // Fall back to env var
    if !env_var.is_empty() {
        if let Ok(val) = std::env::var(env_var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Resolve a non-secret config value from DB ConfigItem, falling back to env var.
/// Priority: DB (runtime_config:{key}) → env var → default.
/// Use for: BONSAI_ADMIN_USER, BONSAI_REQUIRE_AUTH, BONSAI_OPERATOR, etc.
pub fn resolve_config_or_env(
    db_items: &[crate::graph::ConfigItemRecord],
    config_key: &str,
    env_var: &str,
    default: &str,
) -> String {
    // Try DB
    if let Some(item) = db_items.iter().find(|i| i.name == config_key && i.enabled) {
        let val: String = serde_json::from_str(&item.content_json).unwrap_or(item.content_json.clone());
        if !val.is_empty() {
            return val;
        }
    }
    // Fall back to env var
    if !env_var.is_empty() {
        if let Ok(val) = std::env::var(env_var) {
            if !val.is_empty() {
                return val;
            }
        }
    }
    default.to_string()
}

pub async fn load(path: &str) -> Result<Config> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("cannot read config '{path}' — copy bonsai.toml.example to bonsai.toml and fill in your targets"))?;
    toml::from_str(&text).context("TOML parse error in config file")
}

#[cfg(test)]
mod tests {
    use super::{Config, RuntimeConfig, RuntimeMode};

    #[test]
    fn runtime_mode_accepts_the_three_supported_modes() {
        for (mode, expected) in [
            ("all", RuntimeMode::All),
            ("core", RuntimeMode::Core),
            ("collector", RuntimeMode::Collector),
        ] {
            let cfg = RuntimeConfig {
                mode: mode.to_string(),
                ..Default::default()
            };
            assert_eq!(cfg.parsed_mode().unwrap(), expected);
        }
    }

    #[test]
    fn runtime_mode_rejects_unknown_values() {
        let cfg = RuntimeConfig {
            mode: "sidecar".to_string(),
            ..Default::default()
        };
        assert!(cfg.parsed_mode().is_err());
    }

    #[test]
    fn runtime_tls_config_deserializes_under_runtime() {
        let cfg: Config = toml::from_str(
            r#"
graph_path = "bonsai.db"

[runtime]
mode = "collector"
core_ingest_endpoint = "https://127.0.0.1:50051"

[runtime.tls]
enabled = true
ca_cert = "config/tls/ca.pem"
cert = "config/tls/collector.pem"
key = "config/tls/collector-key.pem"
server_name = "bonsai-core.local"

[[target]]
address = "127.0.0.1:57400"
"#,
        )
        .unwrap();

        assert!(cfg.runtime.tls.enabled);
        assert_eq!(
            cfg.runtime.tls.server_name.as_deref(),
            Some("bonsai-core.local")
        );
    }
}

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};

use crate::config::{SyslogConfig, TargetConfig};
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyslogCategory {
    Auth,
    Hardware,
    Software,
    Protocol,
    License,
    Custom,
}

impl SyslogCategory {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Protocol => "protocol",
            Self::License => "license",
            Self::Custom => "custom",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auth" => Some(Self::Auth),
            "hardware" => Some(Self::Hardware),
            "software" => Some(Self::Software),
            "protocol" => Some(Self::Protocol),
            "license" => Some(Self::License),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyslogEvent {
    pub timestamp_ns: i64,
    pub priority: Option<u8>,
    pub facility: Option<u8>,
    pub severity: Option<u8>,
    pub hostname: String,
    pub app_name: String,
    pub proc_id: String,
    pub msg_id: String,
    pub category: SyslogCategory,
    pub message: String,
    pub raw: String,
    pub transport: String,
    pub peer_addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyslogFact {
    pub timestamp_ns: i64,
    pub fact_type: String,
    pub category: String,
    pub hostname: String,
    pub source_vendor: String,
    pub message: String,
    pub raw: String,
    pub transport: String,
    pub peer_addr: String,
    #[serde(default)]
    pub field_schema: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

pub async fn run_syslog_receiver(
    cfg: SyslogConfig,
    pattern_dir: String,
    targets: Vec<TargetConfig>,
    bus: Arc<InProcessBus>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let archive = SyslogArchive::open(&cfg.archive_path).await?;
    let fact_extractor = Arc::new(SyslogFactExtractor::load_from_dir(&pattern_dir));
    let target_map = Arc::new(SyslogTargetMap::new(&targets));
    let mut tasks = Vec::new();

    if !cfg.udp_addr.trim().is_empty() {
        let socket = UdpSocket::bind(&cfg.udp_addr)
            .await
            .with_context(|| format!("bind syslog UDP listener at {}", cfg.udp_addr))?;
        info!(addr = %cfg.udp_addr, "syslog UDP listener started");
        tasks.push(tokio::spawn(run_udp(
            socket,
            Arc::clone(&bus),
            archive.clone(),
            Arc::clone(&fact_extractor),
            Arc::clone(&target_map),
            cfg.max_frame_bytes,
            shutdown.clone(),
        )));
    }

    if !cfg.tcp_addr.trim().is_empty() {
        let listener = TcpListener::bind(&cfg.tcp_addr)
            .await
            .with_context(|| format!("bind syslog TCP listener at {}", cfg.tcp_addr))?;
        info!(addr = %cfg.tcp_addr, "syslog TCP listener started");
        tasks.push(tokio::spawn(run_tcp(
            listener,
            Arc::clone(&bus),
            archive.clone(),
            Arc::clone(&fact_extractor),
            Arc::clone(&target_map),
            cfg.max_frame_bytes,
            shutdown.clone(),
        )));
    }

    if tasks.is_empty() {
        warn!("syslog receiver enabled but both udp_addr and tcp_addr are empty");
        return Ok(());
    }

    for task in tasks {
        if let Err(error) = task.await {
            warn!(%error, "syslog receiver task panicked");
        }
    }

    Ok(())
}

async fn run_udp(
    socket: UdpSocket,
    bus: Arc<InProcessBus>,
    archive: SyslogArchive,
    fact_extractor: Arc<SyslogFactExtractor>,
    target_map: Arc<SyslogTargetMap>,
    max_frame_bytes: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut buf = vec![0_u8; max_frame_bytes.max(1)];
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("syslog UDP listener stopping");
                break;
            }
            recv = socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, peer)) => {
                        let raw = String::from_utf8_lossy(&buf[..n]).trim_end().to_string();
                        handle_frame(
                            raw,
                            "udp",
                            peer.to_string(),
                            &bus,
                            &archive,
                            &fact_extractor,
                            &target_map,
                        )
                        .await;
                    }
                    Err(error) => warn!(%error, "syslog UDP receive failed"),
                }
            }
        }
    }
}

async fn run_tcp(
    listener: TcpListener,
    bus: Arc<InProcessBus>,
    archive: SyslogArchive,
    fact_extractor: Arc<SyslogFactExtractor>,
    target_map: Arc<SyslogTargetMap>,
    max_frame_bytes: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("syslog TCP listener stopping");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        let bus = Arc::clone(&bus);
                        let archive = archive.clone();
                        let fact_extractor = Arc::clone(&fact_extractor);
                        let target_map = Arc::clone(&target_map);
                        tokio::spawn(async move {
                            let mut reader = BufReader::new(stream);
                            let mut line = String::new();
                            loop {
                                line.clear();
                                match reader.read_line(&mut line).await {
                                    Ok(0) => break,
                                    Ok(n) if n > max_frame_bytes => {
                                        warn!(peer = %peer, bytes = n, "syslog TCP frame exceeded limit");
                                        break;
                                    }
                                    Ok(_) => {
                                        handle_frame(
                                            line.trim_end().to_string(),
                                            "tcp",
                                            peer.to_string(),
                                            &bus,
                                            &archive,
                                            &fact_extractor,
                                            &target_map,
                                        ).await;
                                    }
                                    Err(error) => {
                                        warn!(%error, peer = %peer, "syslog TCP read failed");
                                        break;
                                    }
                                }
                            }
                        });
                    }
                    Err(error) => warn!(%error, "syslog TCP accept failed"),
                }
            }
        }
    }
}

async fn handle_frame(
    raw: String,
    transport: &str,
    peer_addr: String,
    bus: &Arc<InProcessBus>,
    archive: &SyslogArchive,
    fact_extractor: &SyslogFactExtractor,
    target_map: &SyslogTargetMap,
) {
    if raw.is_empty() {
        return;
    }

    let event = parse_syslog(&raw, transport, &peer_addr, now_ns());
    metrics::counter!(
        "bonsai_syslog_events_total",
        "category" => event.category.as_str(),
        "transport" => transport.to_string()
    )
    .increment(1);

    if let Err(error) = archive.append(&event).await {
        warn!(%error, "failed to archive syslog event");
        metrics::counter!("bonsai_syslog_archive_errors_total").increment(1);
    }

    let target = target_map.resolve(&event);
    let facts = fact_extractor.extract(&event, &target.vendor);
    let target_address = target.address.clone();
    let target_vendor = target.vendor.clone();
    let target_hostname = target.hostname.clone();
    let target_role = target.role.clone();
    let target_site = target.site.clone();

    bus.publish(TelemetryUpdate {
        target: target_address.clone(),
        vendor: target_vendor.clone(),
        hostname: target_hostname.clone(),
        role: target_role.clone(),
        site: target_site.clone(),
        timestamp_ns: event.timestamp_ns,
        path: format!("signals/syslog/{}", event.category.as_str()),
        value: serde_json::to_value(&event).unwrap_or_else(|_| json!({ "raw": raw })),
    });

    for fact in facts {
        bus.publish(TelemetryUpdate {
            target: target_address.clone(),
            vendor: target_vendor.clone(),
            hostname: target_hostname.clone(),
            role: target_role.clone(),
            site: target_site.clone(),
            timestamp_ns: fact.timestamp_ns,
            path: format!("signals/syslog_fact/{}", fact.fact_type),
            value: serde_json::to_value(&fact)
                .unwrap_or_else(|_| json!({ "message": event.message, "raw": raw })),
        });
    }
}

#[derive(Clone)]
struct SyslogArchive {
    file: Option<Arc<Mutex<tokio::fs::File>>>,
}

#[derive(Clone, Default)]
struct SyslogTargetMap {
    entries: Vec<SyslogTargetEntry>,
}

#[derive(Clone)]
struct SyslogTargetEntry {
    hostname: String,
    address: String,
    vendor: String,
    role: String,
    site: String,
}

#[derive(Clone)]
struct ResolvedTarget {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SyslogPatternCatalog {
    #[serde(default)]
    vendor: String,
    #[serde(default)]
    facts: Vec<SyslogFactPattern>,
}

#[derive(Debug, Clone, Deserialize)]
struct SyslogFactPattern {
    #[serde(default)]
    fact_type: String,
    #[serde(default)]
    category: String,
    regex: String,
    #[serde(default)]
    field_schema: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
struct SyslogFactExtractor {
    patterns: Vec<CompiledSyslogFactPattern>,
}

#[derive(Clone)]
struct CompiledSyslogFactPattern {
    vendor: String,
    fact_type: String,
    category: Option<SyslogCategory>,
    regex: Regex,
    field_schema: BTreeMap<String, String>,
}

impl SyslogArchive {
    async fn open(path: &str) -> Result<Self> {
        if path.trim().is_empty() {
            return Ok(Self { file: None });
        }
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create syslog archive directory {}", parent.display()))?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("open syslog archive {}", path))?;
        Ok(Self {
            file: Some(Arc::new(Mutex::new(file))),
        })
    }

    async fn append(&self, event: &SyslogEvent) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let mut line = serde_json::to_vec(event).context("serialize syslog event")?;
        line.push(b'\n');
        let mut file = file.lock().await;
        file.write_all(&line)
            .await
            .context("write syslog archive line")?;
        Ok(())
    }
}

impl SyslogTargetMap {
    fn new(targets: &[TargetConfig]) -> Self {
        let entries = targets
            .iter()
            .filter_map(|target| {
                let hostname = target.hostname.as_ref()?.trim();
                if hostname.is_empty() {
                    return None;
                }
                Some(SyslogTargetEntry {
                    hostname: hostname.to_ascii_lowercase(),
                    address: target
                        .address
                        .split(':')
                        .next()
                        .unwrap_or(&target.address)
                        .to_string(),
                    vendor: target.vendor.clone().unwrap_or_default(),
                    role: target.role.clone().unwrap_or_default(),
                    site: target.site.clone().unwrap_or_default(),
                })
            })
            .collect();
        Self { entries }
    }

    fn resolve(&self, event: &SyslogEvent) -> ResolvedTarget {
        let hostname = event.hostname.trim().to_ascii_lowercase();
        if let Some(entry) = self.entries.iter().find(|entry| entry.hostname == hostname) {
            return ResolvedTarget {
                address: entry.address.clone(),
                hostname: entry.hostname.clone(),
                vendor: entry.vendor.clone(),
                role: entry.role.clone(),
                site: entry.site.clone(),
            };
        }
        ResolvedTarget {
            address: if hostname.is_empty() {
                event.peer_addr.clone()
            } else {
                event.hostname.clone()
            },
            hostname: event.hostname.clone(),
            vendor: String::new(),
            role: String::new(),
            site: String::new(),
        }
    }
}

impl SyslogFactExtractor {
    fn load_from_dir(dir: &str) -> Self {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self::default();
        };
        let mut patterns = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(catalog) = serde_yaml::from_str::<SyslogPatternCatalog>(&raw) else {
                continue;
            };
            for fact in catalog.facts {
                if fact.fact_type.trim().is_empty() || fact.regex.trim().is_empty() {
                    continue;
                }
                let regex = match Regex::new(&fact.regex) {
                    Ok(regex) => regex,
                    Err(error) => {
                        warn!(
                            path = %path.display(),
                            fact_type = %fact.fact_type,
                            %error,
                            "invalid syslog fact regex"
                        );
                        continue;
                    }
                };
                let category = if fact.category.trim().is_empty() {
                    None
                } else {
                    SyslogCategory::from_str(&fact.category)
                };
                patterns.push(CompiledSyslogFactPattern {
                    vendor: catalog.vendor.to_ascii_lowercase(),
                    fact_type: fact.fact_type,
                    category,
                    regex,
                    field_schema: fact.field_schema,
                });
            }
        }
        Self { patterns }
    }

    fn extract(&self, event: &SyslogEvent, vendor: &str) -> Vec<SyslogFact> {
        let vendor = vendor.to_ascii_lowercase();
        self.patterns
            .iter()
            .filter(|pattern| pattern.vendor.is_empty() || vendor.contains(&pattern.vendor))
            .filter(|pattern| {
                pattern
                    .category
                    .as_ref()
                    .is_none_or(|cat| cat == &event.category)
            })
            .filter_map(|pattern| {
                let captures = pattern.regex.captures(&event.message)?;
                let mut fields = BTreeMap::new();
                for name in pattern.regex.capture_names().flatten() {
                    if let Some(value) = captures.name(name) {
                        let value = value.as_str().trim();
                        if !value.is_empty() {
                            fields.insert(name.to_string(), value.to_string());
                        }
                    }
                }
                if fields.is_empty() {
                    return None;
                }
                Some(SyslogFact {
                    timestamp_ns: event.timestamp_ns,
                    fact_type: pattern.fact_type.clone(),
                    category: event.category.as_str().to_string(),
                    hostname: event.hostname.clone(),
                    source_vendor: vendor.clone(),
                    message: event.message.clone(),
                    raw: event.raw.clone(),
                    transport: event.transport.clone(),
                    peer_addr: event.peer_addr.clone(),
                    field_schema: pattern.field_schema.clone(),
                    fields,
                })
            })
            .collect()
    }
}

pub fn parse_syslog(raw: &str, transport: &str, peer_addr: &str, timestamp_ns: i64) -> SyslogEvent {
    let (priority, rest) = parse_priority(raw);
    let (hostname, app_name, proc_id, msg_id, message) = if rest.starts_with("1 ") {
        parse_rfc5424(rest).unwrap_or_else(|| parse_legacy(rest))
    } else {
        parse_legacy(rest)
    };
    let category = classify_message(&message);
    let facility = priority.map(|pri| pri / 8);
    let severity = priority.map(|pri| pri % 8);

    SyslogEvent {
        timestamp_ns,
        priority,
        facility,
        severity,
        hostname,
        app_name,
        proc_id,
        msg_id,
        category,
        message,
        raw: raw.to_string(),
        transport: transport.to_string(),
        peer_addr: peer_addr.to_string(),
    }
}

fn parse_priority(raw: &str) -> (Option<u8>, &str) {
    let Some(after_lt) = raw.strip_prefix('<') else {
        return (None, raw);
    };
    let Some(end) = after_lt.find('>') else {
        return (None, raw);
    };
    let pri = after_lt[..end].parse::<u8>().ok();
    (pri, &after_lt[end + 1..])
}

fn parse_rfc5424(rest: &str) -> Option<(String, String, String, String, String)> {
    let mut parts = rest.splitn(8, ' ');
    let version = parts.next()?;
    if version != "1" {
        return None;
    }
    let _timestamp = parts.next()?;
    let hostname = normalise_nil(parts.next()?);
    let app_name = normalise_nil(parts.next()?);
    let proc_id = normalise_nil(parts.next()?);
    let msg_id = normalise_nil(parts.next()?);
    let structured_or_message = parts.next().unwrap_or("");
    let message_tail = parts.next().unwrap_or("");
    let message = if structured_or_message.starts_with('[') || structured_or_message == "-" {
        message_tail
    } else if message_tail.is_empty() {
        structured_or_message
    } else {
        return Some((
            hostname,
            app_name,
            proc_id,
            msg_id,
            format!("{structured_or_message} {message_tail}"),
        ));
    };
    Some((hostname, app_name, proc_id, msg_id, message.to_string()))
}

fn parse_legacy(rest: &str) -> (String, String, String, String, String) {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() >= 5
        && tokens[0].len() == 3
        && tokens[1].parse::<u8>().is_ok()
        && tokens[2].contains(':')
    {
        let hostname = tokens[3].to_string();
        let message = tokens[4..].join(" ");
        return (
            hostname,
            String::new(),
            String::new(),
            String::new(),
            message,
        );
    }
    if tokens.len() >= 2 {
        return (
            tokens[0].to_string(),
            String::new(),
            String::new(),
            String::new(),
            tokens[1..].join(" "),
        );
    }
    (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        rest.to_string(),
    )
}

fn normalise_nil(value: &str) -> String {
    if value == "-" {
        String::new()
    } else {
        value.to_string()
    }
}

fn classify_message(message: &str) -> SyslogCategory {
    let text = message.to_ascii_lowercase();
    if contains_any(
        &text,
        &["auth", "login failed", "failed password", "aaa", "ssh"],
    ) {
        SyslogCategory::Auth
    } else if contains_any(
        &text,
        &["fan", "psu", "power", "temperature", "thermal", "fru"],
    ) {
        SyslogCategory::Hardware
    } else if contains_any(
        &text,
        &["crash", "panic", "core", "process restart", "restarted"],
    ) {
        SyslogCategory::Software
    } else if contains_any(
        &text,
        &["bgp", "bfd", "ospf", "isis", "is-is", "lldp", "stp"],
    ) {
        SyslogCategory::Protocol
    } else if contains_any(&text, &["license", "licence", "subscription expired"]) {
        SyslogCategory::License
    } else {
        SyslogCategory::Custom
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc5424_auth_failure() {
        let event = parse_syslog(
            "<34>1 2026-05-09T12:00:00Z leaf1 sshd 123 ID47 - Failed password for admin",
            "udp",
            "127.0.0.1:5514",
            42,
        );
        assert_eq!(event.priority, Some(34));
        assert_eq!(event.facility, Some(4));
        assert_eq!(event.severity, Some(2));
        assert_eq!(event.hostname, "leaf1");
        assert_eq!(event.app_name, "sshd");
        assert_eq!(event.category, SyslogCategory::Auth);
        assert_eq!(event.timestamp_ns, 42);
    }

    #[test]
    fn parses_legacy_protocol_message() {
        let event = parse_syslog(
            "<165>May  9 12:00:00 srl-leaf1 BGP neighbor 10.1.0.1 down",
            "tcp",
            "127.0.0.1:6514",
            99,
        );
        assert_eq!(event.hostname, "srl-leaf1");
        assert_eq!(event.severity, Some(5));
        assert_eq!(event.category, SyslogCategory::Protocol);
        assert!(event.message.contains("BGP neighbor"));
    }

    #[test]
    fn classifies_hardware_and_license_messages() {
        assert_eq!(
            classify_message("PSU failure detected"),
            SyslogCategory::Hardware
        );
        assert_eq!(
            classify_message("license will expire soon"),
            SyslogCategory::License
        );
    }

    #[test]
    fn resolves_syslog_hostname_to_managed_target() {
        let targets = vec![TargetConfig {
            address: "10.0.0.5:57400".to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some("nokia_srl".to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: Some("srl-leaf1".to_string()),
            role: Some("leaf".to_string()),
            site: Some("dc-a".to_string()),
            collector_id: None,
            selected_paths: Vec::new(),
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
        }];
        let map = SyslogTargetMap::new(&targets);
        let event = parse_syslog(
            "<165>May  9 12:00:00 srl-leaf1 BGP neighbor 10.1.0.1 down",
            "udp",
            "127.0.0.1:5514",
            1,
        );
        let resolved = map.resolve(&event);
        assert_eq!(resolved.address, "10.0.0.5");
        assert_eq!(resolved.vendor, "nokia_srl");
        assert_eq!(resolved.role, "leaf");
        assert_eq!(resolved.site, "dc-a");
    }

    #[test]
    fn extracts_named_syslog_fact_fields() {
        let extractor = SyslogFactExtractor {
            patterns: vec![CompiledSyslogFactPattern {
                vendor: "nokia".to_string(),
                fact_type: "bgp_neighbor".to_string(),
                category: Some(SyslogCategory::Protocol),
                regex: Regex::new(
                    r"(?i)bgp neighbor (?P<peer_address>\d+\.\d+\.\d+\.\d+) (?P<new_state>down|up|established|idle)",
                )
                .expect("regex"),
                field_schema: BTreeMap::from([
                    ("peer_address".to_string(), "string".to_string()),
                    ("new_state".to_string(), "string".to_string()),
                ]),
            }],
        };
        let event = parse_syslog(
            "<165>May  9 12:00:00 srl-leaf1 BGP neighbor 10.1.0.1 down",
            "udp",
            "127.0.0.1:5514",
            5,
        );

        let facts = extractor.extract(&event, "nokia_srl");

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, "bgp_neighbor");
        assert_eq!(
            facts[0].fields.get("peer_address").map(String::as_str),
            Some("10.1.0.1")
        );
        assert_eq!(
            facts[0].fields.get("new_state").map(String::as_str),
            Some("down")
        );
        assert_eq!(
            facts[0]
                .field_schema
                .get("peer_address")
                .map(String::as_str),
            Some("string")
        );
    }
}

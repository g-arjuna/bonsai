use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use hex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tracing::{debug, info, warn};

use crate::config::{SnmpConfig, TargetConfig};
use crate::event_bus::InProcessBus;
use crate::receiver_supervisor::SharedReceiverSupervisor;
use crate::resource_governor::GovernorHandle;
use crate::streaming::netflow::{encode_key_component, resolve_flow_identity};
use crate::telemetry::TelemetryUpdate;
use crate::telemetry_identity::{IdentityResolution, TelemetrySourceIdentity};

const OID_SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";
const OID_SNMP_TRAP: &str = "1.3.6.1.6.3.1.1.4.1.0";
const OID_COLD_START: &str = "1.3.6.1.6.3.1.1.5.1";
const OID_WARM_START: &str = "1.3.6.1.6.3.1.1.5.2";
const OID_LINK_DOWN: &str = "1.3.6.1.6.3.1.1.5.3";
const OID_LINK_UP: &str = "1.3.6.1.6.3.1.1.5.4";
const OID_AUTH_FAILURE: &str = "1.3.6.1.6.3.1.1.5.5";

#[derive(Debug, Clone, Serialize)]
pub struct SnmpVarBind {
    pub oid: String,
    pub value_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnmpTrapEvent {
    pub timestamp_ns: i64,
    pub peer_addr: String,
    pub version: String,
    pub community: String,
    pub event_type: String,
    pub trap_oid: String,
    pub category: String,
    pub message: String,
    pub enterprise_oid: String,
    pub generic_trap: Option<i64>,
    pub specific_trap: Option<i64>,
    pub uptime_ticks: Option<u64>,
    pub varbinds: Vec<SnmpVarBind>,
    pub parse_error: String,
    pub raw_hex: String,
    pub raw_len: usize,
    /// SNMPv3 USM security name (user name). Empty for v1/v2c.
    pub usm_security_name: String,
    /// SNMPv3 auth protocol used by the matched USM user. Empty for v1/v2c or unknown user.
    pub usm_auth_protocol: String,
    /// SNMPv3 authoritative engine ID (hex) — self-reported engine identity
    /// per RFC 3411; evidence for source attribution, not proof of device.
    #[serde(default)]
    pub engine_id: String,
    /// SNMPv1 agent-addr field — self-reported agent identity from inside
    /// the PDU. Evidence only; never authenticated. Empty for v2c/v3.
    #[serde(default)]
    pub agent_address: String,
}

/// A structured fact extracted from SNMP varbinds using an OID pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnmpFact {
    pub timestamp_ns: i64,
    pub fact_type: String,
    pub trap_oid: String,
    pub peer_addr: String,
    pub enterprise_oid: String,
    #[serde(default)]
    pub field_schema: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

// ── OID pattern config (loaded from config/snmp_oid_patterns/*.yaml) ──────────

#[derive(Debug, Clone, Deserialize)]
struct SnmpOidPatternFile {
    patterns: Vec<SnmpOidPattern>,
}

#[derive(Debug, Clone, Deserialize)]
struct SnmpOidPattern {
    fact_type: String,
    trap_oid: String,
    #[serde(default)]
    fields: BTreeMap<String, String>,
    #[serde(default)]
    field_schema: BTreeMap<String, String>,
    #[serde(default)]
    index_suffix_fields: Vec<IndexSuffixField>,
}

/// Describes how to extract a field value from the OID instance suffix.
/// For example, Nokia TIMETRA-BGP-MIB encodes the BGP peer IPv4 address
/// as the last 4 octets of the trap OID suffix.
#[derive(Debug, Clone, Deserialize)]
struct IndexSuffixField {
    field: String,
    /// Negative = count from end. E.g. -4 means last 4 components.
    byte_offset: i32,
    /// "ipv4" | "string" | "integer"
    #[serde(default = "default_suffix_type")]
    suffix_type: String,
}

fn default_suffix_type() -> String {
    "ipv4".to_string()
}

#[derive(Clone, Default)]
pub struct SnmpFactExtractor {
    patterns: Vec<SnmpOidPattern>,
}

impl SnmpFactExtractor {
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn load_from_dir(dir: &str) -> Self {
        if dir.trim().is_empty() {
            return Self::default();
        }
        let mut patterns: Vec<SnmpOidPattern> = Vec::new();
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) => {
                warn!(dir, error = %e, "snmp_oid_patterns dir not readable; fact extraction disabled");
                return Self::default();
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => match serde_yaml::from_str::<SnmpOidPatternFile>(&text) {
                    Ok(file) => {
                        info!(file = %path.display(), count = file.patterns.len(), "loaded snmp OID patterns");
                        patterns.extend(file.patterns);
                    }
                    Err(e) => {
                        warn!(file = %path.display(), error = %e, "failed to parse snmp OID pattern file")
                    }
                },
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "failed to read snmp OID pattern file")
                }
            }
        }
        Self { patterns }
    }

    /// Load patterns from in-memory YAML strings (typically from ConfigItem DB).
    /// Each entry is (name, yaml_content). Falls back to `load_from_dir` if empty.
    pub fn load_from_yaml_strings(items: &[(String, String)], fallback_dir: &str) -> Self {
        if items.is_empty() {
            return Self::load_from_dir(fallback_dir);
        }
        let mut patterns: Vec<SnmpOidPattern> = Vec::new();
        for (name, raw) in items {
            match serde_yaml::from_str::<SnmpOidPatternFile>(raw) {
                Ok(file) => {
                    info!(name = %name, count = file.patterns.len(), "loaded snmp OID patterns from DB");
                    patterns.extend(file.patterns);
                }
                Err(e) => {
                    warn!(name = %name, error = %e, "failed to parse snmp OID pattern from DB")
                }
            }
        }
        Self { patterns }
    }

    /// Try to match a parsed trap against OID patterns. Returns `Some(SnmpFact)` on first match.
    pub fn extract(&self, event: &SnmpTrapEvent) -> Option<SnmpFact> {
        let trap_oid = event.trap_oid.trim_end_matches(".0");
        let pattern = self.patterns.iter().find(|p| {
            let pat = p.trap_oid.trim_end_matches(".0");
            trap_oid == pat || trap_oid.starts_with(&format!("{pat}."))
        })?;

        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for vb in &event.varbinds {
            let oid = vb.oid.trim_end_matches(".0");
            for (field_oid, field_name) in &pattern.fields {
                let field_oid = field_oid.trim_end_matches(".0");
                if oid == field_oid || oid.starts_with(&format!("{field_oid}.")) {
                    fields.insert(field_name.clone(), vb.value.clone());
                }
            }
        }

        // OID index-suffix extraction: parse fields encoded in the trap OID itself.
        // Nokia TIMETRA-BGP-MIB encodes peer IPv4 as last 4 OID components.
        if !pattern.index_suffix_fields.is_empty() {
            let pat_trimmed = pattern.trap_oid.trim_end_matches(".0");
            let suffix = trap_oid
                .strip_prefix(pat_trimmed)
                .and_then(|s| s.strip_prefix('.'))
                .unwrap_or("");
            if !suffix.is_empty() {
                let components: Vec<&str> = suffix.split('.').collect();
                for isf in &pattern.index_suffix_fields {
                    if let Some(value) = parse_index_suffix(&components, isf) {
                        fields.insert(isf.field.clone(), value);
                    }
                }
            }
        }

        debug!(
            fact_type = %pattern.fact_type,
            trap_oid = %event.trap_oid,
            fields = ?fields,
            "snmp OID fact extracted"
        );

        Some(SnmpFact {
            timestamp_ns: event.timestamp_ns,
            fact_type: pattern.fact_type.clone(),
            trap_oid: event.trap_oid.clone(),
            peer_addr: event.peer_addr.clone(),
            enterprise_oid: event.enterprise_oid.clone(),
            field_schema: pattern.field_schema.clone(),
            fields,
        })
    }
}

/// Parse a field value from OID suffix components using an IndexSuffixField rule.
fn parse_index_suffix(components: &[&str], isf: &IndexSuffixField) -> Option<String> {
    let len = components.len() as i32;
    let count = isf.byte_offset.unsigned_abs() as usize;

    // Determine the slice of components to use.
    let slice = if isf.byte_offset < 0 {
        // Negative: take last N components.
        if (len as usize) < count {
            return None;
        }
        &components[components.len() - count..]
    } else {
        // Positive: take N components starting from offset.
        let start = isf.byte_offset as usize;
        if start + count > components.len() {
            return None;
        }
        &components[start..start + count]
    };

    match isf.suffix_type.as_str() {
        "ipv4" => {
            if slice.len() != 4 {
                return None;
            }
            let octets: Vec<u8> = slice.iter().filter_map(|s| s.parse::<u8>().ok()).collect();
            if octets.len() != 4 {
                return None;
            }
            Some(format!(
                "{}.{}.{}.{}",
                octets[0], octets[1], octets[2], octets[3]
            ))
        }
        "integer" => slice
            .first()
            .and_then(|s| s.parse::<i64>().ok())
            .map(|v| v.to_string()),
        _ => {
            // "string" — join components with dots.
            Some(slice.join("."))
        }
    }
}

/// BN-021e: per-source provisional identity context for SNMP traps.
///
/// The SNMP trap receiver has no authenticated ingress: the observed UDP
/// transport endpoint is evidence only, and every identity claim inside
/// the packet (v1 agent-addr, v3 engine ID / context engine ID / USM
/// security name, community string) is self-reported. A configured
/// polling target is not automatically the authenticated sender of a
/// trap. The provisional `snmpsrc/...` key scopes source-level
/// correlation without ever collapsing unrelated exporters.
#[derive(Clone)]
struct SnmpSource {
    /// Collision-resistant provisional key:
    /// `snmpsrc/{collector}/udp/{peer_ip}:{peer_port}`.
    source_key: String,
    /// Observed transport endpoint `ip:port`, evidence only.
    peer_endpoint: String,
    collector_id: String,
}

impl SnmpSource {
    fn udp(peer_endpoint: &str, collector_id: &str) -> Self {
        Self {
            source_key: format!(
                "snmpsrc/{}/udp/{}",
                encode_key_component(collector_id),
                peer_endpoint
            ),
            peer_endpoint: peer_endpoint.to_string(),
            collector_id: collector_id.to_string(),
        }
    }

    /// Provisional identity envelope. Plain SNMP/UDP can never produce a
    /// transport-attested `VerifiedIngressPrincipal`, so resolution is
    /// always provisional — bindings cannot claim this observation.
    fn identity(&self, event: &SnmpTrapEvent) -> TelemetrySourceIdentity {
        let mut identity = TelemetrySourceIdentity::observed(self.peer_endpoint.clone())
            .with_protocol("snmp")
            .with_exporter(self.source_key.clone())
            .with_collector(self.collector_id.clone());
        // Self-reported claims are preserved as *reported* evidence —
        // never as verified identity.
        if !event.agent_address.trim().is_empty() {
            identity = identity.with_reported(format!("agent:{}", event.agent_address));
        }
        identity
    }
}

/// How a configured polling target relates to the observed trap sender.
/// This is a *hypothesis* for evidence annotation and read-only graph
/// probes — never authorization for canonical device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnmpCandidateStatus {
    /// Exactly one configured target shares the observed source IP.
    Unique,
    /// Multiple configured targets share the observed source IP
    /// (e.g. duplicate private IPs across tenants) — no arbitrary
    /// selection is permitted.
    Ambiguous,
    /// No configured target matches; the sender is unknown.
    Unresolved,
}

#[derive(Clone)]
struct ResolvedTarget {
    /// Candidate device address — populated only for `Unique` matches.
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
    candidate_status: SnmpCandidateStatus,
    /// All matching configured addresses — populated for `Ambiguous`.
    candidate_addresses: Vec<String>,
}

impl ResolvedTarget {
    /// Vendor used for fact-pattern selection. Only a unique target
    /// match may steer pattern choice; ambiguous/unresolved stay empty.
    fn candidate_vendor(&self) -> &str {
        match self.candidate_status {
            SnmpCandidateStatus::Unique => &self.vendor,
            _ => "",
        }
    }

    fn candidate_role(&self) -> &str {
        match self.candidate_status {
            SnmpCandidateStatus::Unique => &self.role,
            _ => "",
        }
    }

    fn candidate_site(&self) -> &str {
        match self.candidate_status {
            SnmpCandidateStatus::Unique => &self.site,
            _ => "",
        }
    }
}

/// Dedup trust domains keep provisional and future verified observations
/// separate: an unauthenticated packet must never suppress an
/// independently authenticated observation with the same trap OID.
const SNMP_DEDUP_DOMAIN_PROVISIONAL: &str = "provisional";
const SNMP_DEDUP_WINDOW_NS: i64 = 5_000_000_000;
const SNMP_DEDUP_MAX_ENTRIES: usize = 8192;
const SNMP_DEDUP_SWEEP_NS: i64 = 30_000_000_000;

/// Map a verified-ingress principal fingerprint to its dedup domain.
/// There is no authenticated SNMP ingress today, so every observation is
/// `provisional`; the parameter exists so a future verified path gets an
/// isolated dedup domain by construction.
fn snmp_dedup_domain(verified_principal: Option<&str>) -> String {
    match verified_principal {
        Some(fingerprint) => format!("verified:{fingerprint}"),
        None => SNMP_DEDUP_DOMAIN_PROVISIONAL.to_string(),
    }
}

/// Bounded dedup store keyed by (trust domain, source key, trap OID).
/// The source key already scopes collector + transport endpoint, so
/// senders sharing a NAT IP but using different source ports cannot
/// collide; a spoofed identical endpoint is indistinguishable at the
/// transport layer and remains a documented UDP limitation.
#[derive(Default)]
struct SnmpDedup {
    map: HashMap<(String, String, String), i64>,
    last_sweep_ns: i64,
}

impl SnmpDedup {
    /// Returns true when this observation was already seen within the
    /// dedup window for the same trust domain. Stale entries are swept
    /// periodically and the map is hard-capped.
    fn should_suppress(
        &mut self,
        domain: &str,
        source_key: &str,
        trap_oid: &str,
        timestamp_ns: i64,
    ) -> bool {
        let key = (
            domain.to_string(),
            source_key.to_string(),
            trap_oid.to_string(),
        );
        if let Some(&prev_ns) = self.map.get(&key)
            && timestamp_ns - prev_ns < SNMP_DEDUP_WINDOW_NS
        {
            return true;
        }
        if timestamp_ns - self.last_sweep_ns > SNMP_DEDUP_SWEEP_NS {
            self.map
                .retain(|_, ts| timestamp_ns - *ts < SNMP_DEDUP_WINDOW_NS);
            self.last_sweep_ns = timestamp_ns;
        }
        if self.map.len() >= SNMP_DEDUP_MAX_ENTRIES {
            // Hard bound: drop the oldest half rather than grow unbounded.
            let mut entries: Vec<((String, String, String), i64)> = self.map.drain().collect();
            entries.sort_by_key(|(_, ts)| *ts);
            let keep_from = entries.len() / 2;
            for (k, ts) in entries.into_iter().skip(keep_from) {
                self.map.insert(k, ts);
            }
            metrics::counter!("bonsai_snmp_dedup_cap_evictions_total").increment(1);
        }
        self.map.insert(key, timestamp_ns);
        false
    }
}

/// Community allowlist semantics (v1/v2c only).
///
/// A configured allowlist must not be bypassed by an empty, missing or
/// malformed community value: when the allowlist is non-empty, a v1/v2c
/// trap is accepted only if its community string is present AND listed.
/// SNMPv3 carries no community (it is cleared after USM parsing) and is
/// governed by the USM security-name check instead. Parse failures
/// (`version == "unknown"`, empty community) fail closed under an
/// allowlist. A matching community is still only "accepted for parsing"
/// — never authenticated canonical identity.
fn community_permits(allowlist: &[String], event: &SnmpTrapEvent) -> bool {
    if allowlist.is_empty() || event.version == "v3" {
        return true;
    }
    !event.community.is_empty() && allowlist.iter().any(|c| c == &event.community)
}

pub async fn run_snmp_receiver(
    cfg: SnmpConfig,
    targets: Vec<TargetConfig>,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
    governor: Option<Arc<GovernorHandle>>,
    mut pattern_rx: Option<watch::Receiver<Arc<SnmpFactExtractor>>>,
    supervisor: Option<SharedReceiverSupervisor>,
) -> Result<()> {
    let archive = SnmpArchive::open(&cfg.archive_path).await?;
    let target_map = SnmpTargetMap::new(&targets);
    let mut fact_extractor = Arc::new(SnmpFactExtractor::load_from_dir(
        cfg.oid_pattern_dir
            .as_deref()
            .unwrap_or("config/snmp_oid_patterns"),
    ));
    let community_allowlist: Vec<String> = cfg.community_allowlist.clone();
    let v3_users: Vec<crate::config::SnmpV3User> = cfg.v3_users.clone();
    let collector_id = cfg.collector_id.clone();

    if cfg.udp_addr.trim().is_empty() {
        warn!("snmp receiver enabled but udp_addr empty");
        return Ok(());
    }

    let socket = UdpSocket::bind(&cfg.udp_addr)
        .await
        .with_context(|| format!("bind snmp UDP listener at {}", cfg.udp_addr))?;
    info!(addr = %cfg.udp_addr, "snmp UDP listener started");

    // BN-021e: dedup is keyed by (trust domain, provisional source key,
    // trap OID) and evaluated only AFTER community/USM filtering, so an
    // unauthenticated packet can never suppress a later observation that
    // passed the same filters. There is no authenticated SNMP ingress
    // today — every observation is in the `provisional` domain.
    let mut dedup = SnmpDedup::default();
    let dedup_domain = snmp_dedup_domain(None);

    let mut buf = vec![0_u8; cfg.max_frame_bytes.max(1)];
    loop {
        let pattern_changed = async {
            match pattern_rx.as_mut() {
                Some(rx) => {
                    rx.changed().await.ok();
                    true
                }
                None => std::future::pending::<bool>().await,
            }
        };
        tokio::select! {
            _ = shutdown.changed() => {
                info!("snmp UDP listener stopping");
                break;
            }
            _ = pattern_changed => {
                if let Some(rx) = pattern_rx.as_ref() {
                    fact_extractor = rx.borrow().clone();
                    info!("snmp: hot-reloaded OID pattern extractor");
                }
            }
            recv = socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, peer)) => {
                        if let Some(ref sup) = supervisor {
                            if let Ok(mut s) = sup.try_write() {
                                s.record_packet("snmp");
                            }
                        }
                        let peer_addr = peer.to_string();
                        let peer_ip = peer.ip().to_string();
                        let timestamp_ns = now_ns();
                        let raw = &buf[..n];
                        let event = parse_snmp_message(raw, &peer_addr, timestamp_ns)
                            .unwrap_or_else(|error| SnmpTrapEvent {
                                timestamp_ns,
                                peer_addr: peer_addr.clone(),
                                version: "unknown".to_string(),
                                community: String::new(),
                                event_type: "snmp_raw".to_string(),
                                trap_oid: String::new(),
                                category: "raw".to_string(),
                                message: "unparsed snmp trap".to_string(),
                                enterprise_oid: String::new(),
                                generic_trap: None,
                                specific_trap: None,
                                uptime_ticks: None,
                                varbinds: Vec::new(),
                                parse_error: error.to_string(),
                                raw_hex: hex::encode(raw),
                                raw_len: n,
                                usm_security_name: String::new(),
                                usm_auth_protocol: String::new(),
                                engine_id: String::new(),
                                agent_address: String::new(),
                            });

                        metrics::counter!(
                            "bonsai_snmp_traps_total",
                            "event_type" => event.event_type.clone()
                        )
                        .increment(1);

                        if let Err(error) = archive.append(&event).await {
                            warn!(%error, "failed to archive snmp trap");
                            metrics::counter!("bonsai_snmp_archive_errors_total").increment(1);
                        }

                        // Community allowlist filtering (v1/v2c): drop traps
                        // from unknown, missing or empty communities.
                        // Archive write above is intentionally before this
                        // check so every trap (including blocked ones) is
                        // preserved for forensic review. Never log the
                        // community string itself.
                        if !community_permits(&community_allowlist, &event) {
                            warn!(
                                peer = %peer_ip,
                                version = %event.version,
                                "snmp trap community not permitted — dropping (not in allowlist)"
                            );
                            metrics::counter!("bonsai_snmp_community_blocked_total").increment(1);
                            continue;
                        }

                        // D4-1 T3: SNMPv3 USM security-name validation.
                        // A *recognized* username is NOT authentication:
                        // this receiver does not verify the USM auth
                        // digest, so a v3 packet carrying an allowed
                        // security_name is still only a provisional claim.
                        if event.version == "v3" && !v3_users.is_empty() {
                            let matched = v3_users.iter().find(|u| u.security_name == event.usm_security_name);
                            if matched.is_none() {
                                warn!(
                                    peer = %peer_ip,
                                    "snmpv3 trap from unknown USM security_name — dropping"
                                );
                                metrics::counter!("bonsai_snmpv3_usm_blocked_total").increment(1);
                                continue;
                            }
                        }

                        // BN-021e: dedup runs only after community/USM
                        // filtering and is scoped to the provisional source
                        // key — filtered-out packets cannot poison dedup.
                        let src = SnmpSource::udp(&peer_addr, &collector_id);
                        if dedup.should_suppress(
                            &dedup_domain,
                            &src.source_key,
                            &event.trap_oid,
                            timestamp_ns,
                        ) {
                            metrics::counter!("bonsai_snmp_dedup_suppressed_total").increment(1);
                            continue;
                        }

                        // D4-21 T5: Under rate shedding or memory pressure, skip bus
                        // publish. Archived data above is preserved for forensic review.
                        if governor.as_deref().is_some_and(|g| g.should_shed()) {
                            metrics::counter!("bonsai_snmp_shed_total").increment(1);
                            continue;
                        }

                        let resolved = target_map.resolve(&peer_ip, &event);
                        let resolution =
                            resolve_flow_identity(&src.identity(&event), &[], None);
                        let identity_json =
                            provisional_identity_json(&src, &resolved, &resolution, &event);
                        let value = publish_value(&event, &identity_json);
                        bus.publish(TelemetryUpdate {
                            target: src.source_key.clone(),
                            vendor: resolved.candidate_vendor().to_string(),
                            hostname: event.agent_address.clone(),
                            role: resolved.candidate_role().to_string(),
                            site: resolved.candidate_site().to_string(),
                            timestamp_ns: event.timestamp_ns,
                            path: format!("signals/snmp/{}", event.event_type),
                            value,
                        });

                        // OID fact extraction — publish a second TelemetryUpdate on
                        // signals/snmp_fact/{fact_type} if an OID pattern matches.
                        if let Some(fact) = fact_extractor.extract(&event) {
                            let fact_path = format!("signals/snmp_fact/{}", fact.fact_type);
                            let fact_value = with_identity(
                                serde_json::to_value(&fact)
                                    .unwrap_or_else(|_| json!({"peer_addr": fact.peer_addr.clone()})),
                                &identity_json,
                            );
                            bus.publish(TelemetryUpdate {
                                target: src.source_key.clone(),
                                vendor: resolved.candidate_vendor().to_string(),
                                hostname: event.agent_address.clone(),
                                role: resolved.candidate_role().to_string(),
                                site: resolved.candidate_site().to_string(),
                                timestamp_ns: event.timestamp_ns,
                                path: fact_path,
                                value: fact_value,
                            });
                        }
                    }
                    Err(error) => warn!(%error, "snmp UDP receive failed"),
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct SnmpArchive {
    file: Option<Arc<Mutex<tokio::fs::File>>>,
}

#[derive(Clone, Default)]
struct SnmpTargetMap {
    entries: Vec<SnmpTargetEntry>,
}

#[derive(Clone)]
struct SnmpTargetEntry {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
}

impl SnmpArchive {
    async fn open(path: &str) -> Result<Self> {
        if path.trim().is_empty() {
            return Ok(Self { file: None });
        }
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create snmp archive directory {}", parent.display()))?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("open snmp archive {}", path))?;
        Ok(Self {
            file: Some(Arc::new(Mutex::new(file))),
        })
    }

    async fn append(&self, event: &SnmpTrapEvent) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let mut line = serde_json::to_vec(event).context("serialize snmp event")?;
        line.push(b'\n');
        let mut file = file.lock().await;
        file.write_all(&line)
            .await
            .context("write snmp archive line")?;
        // tokio::fs::File::write_all returns once bytes are accepted into
        // its internal buffer — the blocking write(2) may still be in
        // flight. flush() drains the pending operation so a returned Ok
        // means the line is actually in the file.
        file.flush().await.context("flush snmp archive line")?;
        Ok(())
    }
}

impl SnmpTargetMap {
    fn new(targets: &[TargetConfig]) -> Self {
        let entries = targets
            .iter()
            .map(|target| SnmpTargetEntry {
                address: target.address.clone(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                site: target.site.clone().unwrap_or_default(),
            })
            .collect();
        Self { entries }
    }

    /// Resolve the observed transport source IP against configured
    /// polling targets.
    ///
    /// BN-021e: this is candidate correlation, not identity resolution.
    /// A configured polling target is NOT automatically the authenticated
    /// sender of a trap — the match only produces a hypothesis for
    /// evidence annotation and read-only graph probes. Multiple targets
    /// sharing the observed IP (e.g. duplicate private addresses across
    /// tenants) resolve to `Ambiguous` with no arbitrary selection.
    fn resolve(&self, peer_ip: &str, _event: &SnmpTrapEvent) -> ResolvedTarget {
        let peer_base = crate::registry::strip_port(peer_ip);
        let matches: Vec<&SnmpTargetEntry> = self
            .entries
            .iter()
            .filter(|entry| crate::registry::strip_port(&entry.address) == peer_base)
            .collect();

        match matches.as_slice() {
            [entry] => ResolvedTarget {
                // Bare IP candidate — strip any gNMI port so a future
                // verified binding lands on the same Device node.
                address: crate::registry::strip_port(&entry.address).to_string(),
                hostname: if entry.hostname.is_empty() {
                    peer_base.to_string()
                } else {
                    entry.hostname.clone()
                },
                vendor: entry.vendor.clone(),
                role: entry.role.clone(),
                site: entry.site.clone(),
                candidate_status: SnmpCandidateStatus::Unique,
                candidate_addresses: Vec::new(),
            },
            [] => ResolvedTarget {
                address: String::new(),
                hostname: String::new(),
                vendor: String::new(),
                role: String::new(),
                site: String::new(),
                candidate_status: SnmpCandidateStatus::Unresolved,
                candidate_addresses: Vec::new(),
            },
            many => ResolvedTarget {
                address: String::new(),
                hostname: String::new(),
                vendor: String::new(),
                role: String::new(),
                site: String::new(),
                candidate_status: SnmpCandidateStatus::Ambiguous,
                candidate_addresses: many
                    .iter()
                    .map(|entry| crate::registry::strip_port(&entry.address).to_string())
                    .collect(),
            },
        }
    }
}

/// Merge the provisional identity envelope into a serialized SnmpFact
/// payload. All identity fields are evidence — the graph projection
/// treats them as hints for read-only probes only.
fn with_identity(mut value: serde_json::Value, identity: &serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut value {
        if let serde_json::Value::Object(id_map) = identity {
            for (k, v) in id_map {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    value
}

/// Serialize a SnmpTrapEvent for bus publication with credential material
/// redacted. The community string and raw packet hex remain available in
/// the restricted forensic archive (`SnmpArchive`) but must never reach
/// graph properties or LLM-visible event detail.
fn publish_value(event: &SnmpTrapEvent, identity: &serde_json::Value) -> serde_json::Value {
    let mut value =
        serde_json::to_value(event).unwrap_or_else(|_| json!({ "peer_addr": event.peer_addr }));
    if let serde_json::Value::Object(map) = &mut value {
        if !event.community.is_empty() {
            map.insert("community".to_string(), json!(""));
            map.insert("community_redacted".to_string(), json!(true));
        }
        if !event.raw_hex.is_empty() {
            map.insert("raw_hex".to_string(), json!(""));
            map.insert("raw_hex_redacted".to_string(), json!(true));
        }
        if let serde_json::Value::Object(id_map) = identity {
            for (k, v) in id_map {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    value
}

/// Build the provisional identity evidence object carried by every
/// published SNMP update. `candidate` records the deterministic
/// source-IP→target hypothesis — it is never an authoritative join.
/// Engine ID, agent address, USM security name and community presence
/// are preserved as separately typed self-reported claims.
fn provisional_identity_json(
    src: &SnmpSource,
    target: &ResolvedTarget,
    resolution: &IdentityResolution,
    event: &SnmpTrapEvent,
) -> serde_json::Value {
    let candidate = match target.candidate_status {
        SnmpCandidateStatus::Unique => json!({
            "status": "unique_target_match",
            "device_address": target.address,
            "hostname": target.hostname,
            "vendor": target.vendor,
            "role": target.role,
            "site": target.site,
        }),
        SnmpCandidateStatus::Ambiguous => json!({
            "status": "ambiguous_target_match",
            "alternatives": target.candidate_addresses,
        }),
        SnmpCandidateStatus::Unresolved => json!({
            "status": "unresolved",
        }),
    };
    json!({
        "source_key": src.source_key,
        "exporter_id": src.source_key,
        "identity_status": serde_json::to_value(resolution.status)
            .unwrap_or(serde_json::Value::Null),
        "device_id": resolution.device_id,
        "canonical_exporter_id": resolution.canonical_exporter_id,
        "identity_resolution": serde_json::to_value(resolution)
            .unwrap_or(serde_json::Value::Null),
        "transport_source": src.peer_endpoint,
        "transport_protocol": "udp",
        "collector_id": src.collector_id,
        "connection_id": serde_json::Value::Null,
        "reported_agent_address": event.agent_address,
        "reported_engine_id": event.engine_id,
        "usm_security_name": event.usm_security_name,
        "usm_auth_status": "unverified",
        "community_present": !event.community.is_empty(),
        "candidate": candidate,
        "authority": "unverified",
    })
}

pub fn parse_snmp_message(raw: &[u8], peer_addr: &str, timestamp_ns: i64) -> Result<SnmpTrapEvent> {
    let mut reader = BerReader::new(raw);
    let message = reader.read_expected(0x30)?;
    let mut msg_reader = BerReader::new(message);

    let version = msg_reader.read_integer()?;
    let version_name = match version {
        0 => "v1",
        1 => "v2c",
        3 => "v3",
        _ => "unknown",
    }
    .to_string();

    let (engine_id_raw, community, pdu_tag, pdu_content) = match version {
        0 | 1 => {
            let community = msg_reader.read_octet_string_text()?;
            let (tag, pdu) = msg_reader.read_tlv()?;
            (String::new(), community, tag, pdu.to_vec())
        }
        3 => {
            let _header = msg_reader.read_expected(0x30)?;
            // RFC 3412 §6.4: globalData (msgGlobalData) SEQUENCE
            // RFC 3414 §2.3: USM security parameters encoded as OCTET STRING wrapping SEQUENCE
            let usm_params_bytes = msg_reader.read_octet_string()?;
            let (usm_engine_id, usm_security_name) = parse_usm_security_name(usm_params_bytes);
            let (scoped_tag, scoped_content) = msg_reader.read_tlv()?;
            if scoped_tag != 0x30 {
                bail!("encrypted SNMPv3 scoped PDU (privProtocol != none) is not yet supported");
            }
            let mut scoped = BerReader::new(scoped_content);
            let context_engine_id = scoped.read_octet_string()?;
            let _context_name = scoped.read_octet_string()?;
            let (tag, pdu) = scoped.read_tlv()?;
            (
                // parse_usm_security_name already returns the USM engine ID
                // as a hex string; only the context engine ID needs encoding.
                format!("{}:{}", usm_engine_id, hex::encode(context_engine_id)),
                usm_security_name,
                tag,
                pdu.to_vec(),
            )
        }
        _ => bail!("unsupported snmp version {version}"),
    };

    let mut event = if version == 0 && pdu_tag == 0xA4 {
        parse_v1_trap(
            &pdu_content,
            peer_addr,
            timestamp_ns,
            &version_name,
            community,
        )?
    } else {
        parse_v2_trap(
            &pdu_content,
            peer_addr,
            timestamp_ns,
            &version_name,
            community,
            pdu_tag,
        )?
    };
    event.raw_hex = hex::encode(raw);
    event.raw_len = raw.len();
    if version == 3 {
        event.usm_security_name = event.community.clone();
        event.community = String::new();
        event.engine_id = engine_id_raw;
    }
    Ok(event)
}

/// Parse the USM security name from SNMPv3 UsmSecurityParameters (RFC 3414 §2.4).
/// UsmSecurityParameters ::= SEQUENCE {
///   msgAuthoritativeEngineID  OCTET STRING,
///   msgAuthoritativeEngineBoots INTEGER,
///   msgAuthoritativeEngineTime INTEGER,
///   msgUserName               OCTET STRING,   ← we want this
///   msgAuthenticationParameters OCTET STRING,
///   msgPrivacyParameters      OCTET STRING
/// }
fn parse_usm_security_name(params: &[u8]) -> (String, String) {
    let mut r = BerReader::new(params);
    if let Ok(seq) = r.read_expected(0x30) {
        let mut sr = BerReader::new(seq);
        let engine_id = sr.read_octet_string().unwrap_or_default();
        let _ = sr.read_tlv(); // engine boots (INTEGER)
        let _ = sr.read_tlv(); // engine time (INTEGER)
        if let Ok(name_bytes) = sr.read_octet_string() {
            return (
                hex::encode(engine_id),
                String::from_utf8_lossy(name_bytes).to_string(),
            );
        }
    }
    (String::new(), String::new())
}

fn parse_v1_trap(
    pdu: &[u8],
    peer_addr: &str,
    timestamp_ns: i64,
    version: &str,
    community: String,
) -> Result<SnmpTrapEvent> {
    let mut reader = BerReader::new(pdu);
    let enterprise_oid = reader.read_oid()?;
    let agent_addr_raw = reader.read_expected(0x40)?;
    let agent_address = if agent_addr_raw.len() == 4 {
        format!(
            "{}.{}.{}.{}",
            agent_addr_raw[0], agent_addr_raw[1], agent_addr_raw[2], agent_addr_raw[3]
        )
    } else {
        String::new()
    };
    let generic_trap = reader.read_integer()?;
    let specific_trap = reader.read_integer()?;
    let uptime_ticks = Some(reader.read_unsigned(0x43)?);
    let varbinds = parse_varbinds(reader.read_expected(0x30)?)?;
    let trap_oid = match generic_trap {
        0 => OID_COLD_START.to_string(),
        1 => OID_WARM_START.to_string(),
        2 => OID_LINK_DOWN.to_string(),
        3 => OID_LINK_UP.to_string(),
        4 => OID_AUTH_FAILURE.to_string(),
        6 => format!("{enterprise_oid}.0.{specific_trap}"),
        _ => enterprise_oid.clone(),
    };
    let (event_type, category, message) = classify_trap(
        &trap_oid,
        &enterprise_oid,
        Some(generic_trap),
        Some(specific_trap),
        &varbinds,
    );
    Ok(SnmpTrapEvent {
        timestamp_ns,
        peer_addr: peer_addr.to_string(),
        version: version.to_string(),
        community,
        event_type,
        trap_oid,
        category,
        message,
        enterprise_oid,
        generic_trap: Some(generic_trap),
        specific_trap: Some(specific_trap),
        uptime_ticks,
        varbinds,
        parse_error: String::new(),
        raw_hex: String::new(),
        raw_len: 0,
        usm_security_name: String::new(),
        usm_auth_protocol: String::new(),
        engine_id: String::new(),
        agent_address,
    })
}

fn parse_v2_trap(
    pdu: &[u8],
    peer_addr: &str,
    timestamp_ns: i64,
    version: &str,
    community: String,
    pdu_tag: u8,
) -> Result<SnmpTrapEvent> {
    if !(0xA0..=0xA8).contains(&pdu_tag) {
        bail!("unexpected snmp pdu tag 0x{pdu_tag:02x}");
    }
    let mut reader = BerReader::new(pdu);
    let _request_id = reader.read_integer()?;
    let _error_status = reader.read_integer()?;
    let _error_index = reader.read_integer()?;
    let varbinds = parse_varbinds(reader.read_expected(0x30)?)?;
    let trap_oid = find_varbind_oid(&varbinds, OID_SNMP_TRAP).unwrap_or_default();
    let enterprise_oid = trap_oid
        .split(".0.")
        .next()
        .unwrap_or(&trap_oid)
        .to_string();
    let uptime_ticks = find_varbind_u64(&varbinds, OID_SYS_UPTIME);
    let (event_type, category, message) =
        classify_trap(&trap_oid, &enterprise_oid, None, None, &varbinds);
    Ok(SnmpTrapEvent {
        timestamp_ns,
        peer_addr: peer_addr.to_string(),
        version: version.to_string(),
        community,
        event_type,
        trap_oid,
        category,
        message,
        enterprise_oid,
        generic_trap: None,
        specific_trap: None,
        uptime_ticks,
        varbinds,
        parse_error: String::new(),
        raw_hex: String::new(),
        raw_len: 0,
        usm_security_name: String::new(),
        usm_auth_protocol: String::new(),
        engine_id: String::new(),
        agent_address: String::new(),
    })
}

fn parse_varbinds(data: &[u8]) -> Result<Vec<SnmpVarBind>> {
    let mut reader = BerReader::new(data);
    let mut varbinds = Vec::new();
    while !reader.is_eof() {
        let varbind = reader.read_expected(0x30)?;
        let mut vb = BerReader::new(varbind);
        let oid = vb.read_oid()?;
        let (tag, value) = vb.read_tlv()?;
        varbinds.push(SnmpVarBind {
            oid,
            value_type: ber_type_name(tag).to_string(),
            value: decode_value(tag, value)?,
        });
    }
    Ok(varbinds)
}

fn classify_trap(
    trap_oid: &str,
    enterprise_oid: &str,
    generic_trap: Option<i64>,
    specific_trap: Option<i64>,
    varbinds: &[SnmpVarBind],
) -> (String, String, String) {
    let text = varbinds
        .iter()
        .map(|vb| format!("{} {}", vb.oid, vb.value))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    let message = if !text.trim().is_empty() {
        text.clone()
    } else if !trap_oid.is_empty() {
        format!("trap oid {trap_oid}")
    } else {
        "snmp trap".to_string()
    };

    if trap_oid == OID_COLD_START || generic_trap == Some(0) {
        return (
            "snmp_cold_start".to_string(),
            "startup".to_string(),
            "device coldStart trap received".to_string(),
        );
    }
    if trap_oid == OID_WARM_START || generic_trap == Some(1) {
        return (
            "snmp_warm_start".to_string(),
            "startup".to_string(),
            "device warmStart trap received".to_string(),
        );
    }
    if trap_oid == OID_LINK_DOWN || generic_trap == Some(2) {
        return (
            "snmp_link_down".to_string(),
            "interface".to_string(),
            format!("linkDown trap received: {message}"),
        );
    }
    if trap_oid == OID_LINK_UP || generic_trap == Some(3) {
        return (
            "snmp_link_up".to_string(),
            "interface".to_string(),
            format!("linkUp trap received: {message}"),
        );
    }
    if trap_oid == OID_AUTH_FAILURE || generic_trap == Some(4) || text.contains("auth") {
        return (
            "snmp_auth_failure".to_string(),
            "auth".to_string(),
            format!("authenticationFailure trap received: {message}"),
        );
    }
    if contains_any(
        &text,
        &[
            "psu",
            "power",
            "temperature",
            "thermal",
            "fan",
            "voltage",
            "env",
        ],
    ) {
        return (
            "snmp_environmental".to_string(),
            "environmental".to_string(),
            format!("environmental trap received: {message}"),
        );
    }
    if contains_any(
        &text,
        &[
            "fru",
            "linecard",
            "line card",
            "fabric",
            "module",
            "chassis",
        ],
    ) {
        return (
            "snmp_fru_failure".to_string(),
            "fru".to_string(),
            format!("fru trap received: {message}"),
        );
    }
    if generic_trap == Some(6) || trap_oid.starts_with(enterprise_oid) {
        let suffix = specific_trap.map(|v| format!(".{v}")).unwrap_or_default();
        return (
            "snmp_enterprise_specific".to_string(),
            "enterprise".to_string(),
            format!("enterprise-specific trap {enterprise_oid}{suffix}: {message}"),
        );
    }
    ("snmp_raw".to_string(), "raw".to_string(), message)
}

fn find_varbind_oid(varbinds: &[SnmpVarBind], oid: &str) -> Option<String> {
    varbinds
        .iter()
        .find(|vb| vb.oid == oid)
        .map(|vb| vb.value.clone())
}

fn find_varbind_u64(varbinds: &[SnmpVarBind], oid: &str) -> Option<u64> {
    varbinds
        .iter()
        .find(|vb| vb.oid == oid)
        .and_then(|vb| vb.value.parse::<u64>().ok())
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn ber_type_name(tag: u8) -> &'static str {
    match tag {
        0x02 => "integer",
        0x04 => "octet_string",
        0x05 => "null",
        0x06 => "object_identifier",
        0x40 => "ip_address",
        0x41 => "counter32",
        0x42 => "gauge32",
        0x43 => "timeticks",
        0x46 => "counter64",
        _ => "unknown",
    }
}

fn decode_value(tag: u8, value: &[u8]) -> Result<String> {
    match tag {
        0x02 => Ok(decode_signed(value)?.to_string()),
        0x04 => Ok(String::from_utf8_lossy(value).to_string()),
        0x05 => Ok("null".to_string()),
        0x06 => Ok(decode_oid(value)?),
        0x40 => {
            if value.len() == 4 {
                Ok(format!(
                    "{}.{}.{}.{}",
                    value[0], value[1], value[2], value[3]
                ))
            } else {
                Ok(hex::encode(value))
            }
        }
        0x41 | 0x42 | 0x43 | 0x46 => Ok(decode_unsigned(value)?.to_string()),
        _ => Ok(hex::encode(value)),
    }
}

fn decode_signed(bytes: &[u8]) -> Result<i64> {
    if bytes.is_empty() || bytes.len() > 8 {
        bail!("invalid signed integer length {}", bytes.len());
    }
    let sign_fill = if bytes[0] & 0x80 != 0 { 0xff } else { 0x00 };
    let mut buf = [sign_fill; 8];
    let start = 8 - bytes.len();
    buf[start..].copy_from_slice(bytes);
    Ok(i64::from_be_bytes(buf))
}

fn decode_unsigned(bytes: &[u8]) -> Result<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        bail!("invalid unsigned integer length {}", bytes.len());
    }
    let mut buf = [0u8; 8];
    let start = 8 - bytes.len();
    buf[start..].copy_from_slice(bytes);
    Ok(u64::from_be_bytes(buf))
}

fn decode_oid(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() {
        bail!("empty oid");
    }
    let first = bytes[0];
    let mut parts = vec![(first / 40) as u32, (first % 40) as u32];
    let mut current = 0u32;
    for &byte in &bytes[1..] {
        current = (current << 7) | (byte & 0x7f) as u32;
        if byte & 0x80 == 0 {
            parts.push(current);
            current = 0;
        }
    }
    if current != 0 {
        bail!("unterminated oid");
    }
    Ok(parts
        .into_iter()
        .map(|part| part.to_string())
        .collect::<Vec<_>>()
        .join("."))
}

struct BerReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BerReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn read_tlv(&mut self) -> Result<(u8, &'a [u8])> {
        let tag = *self
            .data
            .get(self.pos)
            .ok_or_else(|| anyhow!("unexpected end of ber buffer"))?;
        self.pos += 1;
        let length = self.read_length()?;
        let end = self
            .pos
            .checked_add(length)
            .ok_or_else(|| anyhow!("ber length overflow"))?;
        let value = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| anyhow!("ber length exceeds buffer"))?;
        self.pos = end;
        Ok((tag, value))
    }

    fn read_expected(&mut self, expected_tag: u8) -> Result<&'a [u8]> {
        let (tag, value) = self.read_tlv()?;
        if tag != expected_tag {
            bail!("expected ber tag 0x{expected_tag:02x}, got 0x{tag:02x}");
        }
        Ok(value)
    }

    fn read_length(&mut self) -> Result<usize> {
        let first = *self
            .data
            .get(self.pos)
            .ok_or_else(|| anyhow!("unexpected end of ber length"))?;
        self.pos += 1;
        if first & 0x80 == 0 {
            return Ok(first as usize);
        }
        let width = (first & 0x7f) as usize;
        if width == 0 || width > 4 {
            bail!("unsupported ber length width {width}");
        }
        let end = self
            .pos
            .checked_add(width)
            .ok_or_else(|| anyhow!("ber length overflow"))?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| anyhow!("ber length truncated"))?;
        self.pos = end;
        let mut length = 0usize;
        for &byte in bytes {
            length = (length << 8) | byte as usize;
        }
        Ok(length)
    }

    fn read_integer(&mut self) -> Result<i64> {
        let value = self.read_expected(0x02)?;
        decode_signed(value)
    }

    fn read_unsigned(&mut self, tag: u8) -> Result<u64> {
        let value = self.read_expected(tag)?;
        decode_unsigned(value)
    }

    fn read_octet_string(&mut self) -> Result<&'a [u8]> {
        self.read_expected(0x04)
    }

    fn read_octet_string_text(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(self.read_octet_string()?).to_string())
    }

    fn read_oid(&mut self) -> Result<String> {
        let value = self.read_expected(0x06)?;
        decode_oid(value)
    }
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

    #[tokio::test]
    async fn snmp_archive_open_and_append() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snmp.jsonl").to_string_lossy().to_string();
        let archive = SnmpArchive::open(&path).await.unwrap();
        let event = SnmpTrapEvent {
            timestamp_ns: 1,
            peer_addr: "1.2.3.4:12345".to_string(),
            version: "v2c".to_string(),
            community: "public".to_string(),
            event_type: "snmp_link_down".to_string(),
            trap_oid: OID_LINK_DOWN.to_string(),
            category: "interface".to_string(),
            message: "linkDown trap".to_string(),
            enterprise_oid: "1.3.6.1.6.3.1.1.5".to_string(),
            generic_trap: None,
            specific_trap: None,
            uptime_ticks: Some(100),
            varbinds: vec![],
            parse_error: String::new(),
            raw_hex: "deadbeef".to_string(),
            raw_len: 4,
            usm_security_name: String::new(),
            usm_auth_protocol: String::new(),
            engine_id: String::new(),
            agent_address: String::new(),
        };
        archive.append(&event).await.unwrap();
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(contents.contains("snmp_link_down"));
    }

    #[test]
    fn parses_v2c_cold_start_trap() {
        let packet = encode_sequence(&[
            encode_integer(1),
            encode_octet_string(b"public"),
            encode_tag(
                0xA7,
                &[
                    encode_integer(1),
                    encode_integer(0),
                    encode_integer(0),
                    encode_sequence(&[
                        encode_varbind(OID_SYS_UPTIME, encode_tag(0x43, &[0x00, 0x00, 0x00, 0x64])),
                        encode_varbind(OID_SNMP_TRAP, encode_oid(OID_COLD_START)),
                    ]),
                ]
                .concat(),
            ),
        ]);

        let event = parse_snmp_message(&packet, "192.0.2.10:162", 42).unwrap();
        assert_eq!(event.version, "v2c");
        assert_eq!(event.event_type, "snmp_cold_start");
        assert_eq!(event.trap_oid, OID_COLD_START);
        assert_eq!(event.uptime_ticks, Some(100));
    }

    #[test]
    fn parses_v1_enterprise_specific_environmental_trap() {
        let packet = encode_sequence(&[
            encode_integer(0),
            encode_octet_string(b"public"),
            encode_tag(
                0xA4,
                &[
                    encode_oid("1.3.6.1.4.1.9"),
                    encode_tag(0x40, &[192, 0, 2, 44]),
                    encode_integer(6),
                    encode_integer(42),
                    encode_tag(0x43, &[0x00, 0x00, 0x01, 0x00]),
                    encode_sequence(&[encode_varbind(
                        "1.3.6.1.4.1.9.9.13.3.1.3.1",
                        encode_octet_string(b"PSU failure alarm asserted"),
                    )]),
                ]
                .concat(),
            ),
        ]);

        let event = parse_snmp_message(&packet, "192.0.2.44:162", 7).unwrap();
        assert_eq!(event.version, "v1");
        assert_eq!(event.event_type, "snmp_environmental");
        assert_eq!(event.generic_trap, Some(6));
        assert_eq!(event.specific_trap, Some(42));
    }

    #[test]
    fn resolves_target_by_source_ip() {
        let targets = vec![TargetConfig {
            extra_ips: Vec::new(),
            optional: false,
            paths: Vec::new(),
            address: "192.0.2.44:57400".to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some("cisco".to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: Some("xrd-pe1".to_string()),
            role: Some("pe".to_string()),
            site: Some("lab".to_string()),
            collector_id: None,
            selected_paths: vec![],
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
            external_id: None,
            inventory_only: false,
        }];
        let map = SnmpTargetMap::new(&targets);
        let resolved = map.resolve(
            "192.0.2.44",
            &SnmpTrapEvent {
                timestamp_ns: 1,
                peer_addr: "192.0.2.44:162".to_string(),
                version: "v2c".to_string(),
                community: String::new(),
                event_type: "snmp_link_down".to_string(),
                trap_oid: OID_LINK_DOWN.to_string(),
                category: "interface".to_string(),
                message: String::new(),
                enterprise_oid: String::new(),
                generic_trap: None,
                specific_trap: None,
                uptime_ticks: None,
                varbinds: vec![],
                parse_error: String::new(),
                raw_hex: String::new(),
                raw_len: 0,
                usm_security_name: String::new(),
                usm_auth_protocol: String::new(),
                engine_id: String::new(),
                agent_address: String::new(),
            },
        );
        assert_eq!(resolved.address, "192.0.2.44");
        assert_eq!(resolved.hostname, "xrd-pe1");
        assert_eq!(resolved.vendor, "cisco");
        // BN-021e: a source-IP match is a *candidate hypothesis*, never
        // canonical attribution.
        assert_eq!(resolved.candidate_status, SnmpCandidateStatus::Unique);
        assert_eq!(resolved.candidate_vendor(), "cisco");
    }

    #[test]
    fn duplicate_target_ips_are_ambiguous_not_arbitrary() {
        let mk = |address: &str, site: &str| TargetConfig {
            extra_ips: Vec::new(),
            optional: false,
            paths: Vec::new(),
            address: address.to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some("cisco".to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: Some(format!("dev-{site}")),
            role: Some("pe".to_string()),
            site: Some(site.to_string()),
            collector_id: None,
            selected_paths: vec![],
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
            external_id: None,
            inventory_only: false,
        };
        // Two tenants reuse the same private IP.
        let map = SnmpTargetMap::new(&[
            mk("10.0.0.1:57400", "site-a"),
            mk("10.0.0.1:57401", "site-b"),
        ]);
        let event = SnmpTrapEvent {
            timestamp_ns: 1,
            peer_addr: "10.0.0.1:55555".to_string(),
            version: "v2c".to_string(),
            community: String::new(),
            event_type: "snmp_link_down".to_string(),
            trap_oid: OID_LINK_DOWN.to_string(),
            category: "interface".to_string(),
            message: String::new(),
            enterprise_oid: String::new(),
            generic_trap: None,
            specific_trap: None,
            uptime_ticks: None,
            varbinds: vec![],
            parse_error: String::new(),
            raw_hex: String::new(),
            raw_len: 0,
            usm_security_name: String::new(),
            usm_auth_protocol: String::new(),
            engine_id: String::new(),
            agent_address: String::new(),
        };
        let resolved = map.resolve("10.0.0.1", &event);
        assert_eq!(resolved.candidate_status, SnmpCandidateStatus::Ambiguous);
        assert!(resolved.address.is_empty());
        assert_eq!(resolved.candidate_addresses.len(), 2);
        // Ambiguous matches must not steer vendor selection either.
        assert_eq!(resolved.candidate_vendor(), "");
    }

    #[test]
    fn community_allowlist_rejects_empty_and_missing() {
        let allow = vec!["private-community".to_string()];
        let mk_event = |version: &str, community: &str| SnmpTrapEvent {
            timestamp_ns: 1,
            peer_addr: "10.0.0.1:162".to_string(),
            version: version.to_string(),
            community: community.to_string(),
            event_type: "snmp_link_down".to_string(),
            trap_oid: OID_LINK_DOWN.to_string(),
            category: "interface".to_string(),
            message: String::new(),
            enterprise_oid: String::new(),
            generic_trap: None,
            specific_trap: None,
            uptime_ticks: None,
            varbinds: vec![],
            parse_error: String::new(),
            raw_hex: String::new(),
            raw_len: 0,
            usm_security_name: String::new(),
            usm_auth_protocol: String::new(),
            engine_id: String::new(),
            agent_address: String::new(),
        };
        // Listed community accepted for parsing.
        assert!(community_permits(
            &allow,
            &mk_event("v2c", "private-community")
        ));
        // Unknown, empty and missing communities all rejected.
        assert!(!community_permits(&allow, &mk_event("v2c", "public")));
        assert!(!community_permits(&allow, &mk_event("v2c", "")));
        assert!(!community_permits(&allow, &mk_event("unknown", "")));
        // v3 has no community — governed by the USM check instead.
        assert!(community_permits(&allow, &mk_event("v3", "")));
        // Empty allowlist accepts everything.
        assert!(community_permits(&[], &mk_event("v2c", "anything")));
    }

    #[test]
    fn dedup_domains_isolate_provisional_from_verified() {
        assert_eq!(snmp_dedup_domain(None), "provisional");
        assert_eq!(snmp_dedup_domain(Some("sha256:abc")), "verified:sha256:abc");
        assert_ne!(
            snmp_dedup_domain(None),
            snmp_dedup_domain(Some("sha256:abc"))
        );
    }

    #[test]
    fn dedup_store_suppresses_only_same_domain_key() {
        let mut dedup = SnmpDedup::default();
        let t = 1_000_000_000i64;
        // First observation recorded.
        assert!(!dedup.should_suppress(
            "provisional",
            "snmpsrc/c/udp/10.0.0.1:162",
            "1.3.6.1.6.3.1.1.5.3",
            t
        ));
        // Same domain+key+OID within window → suppressed.
        assert!(dedup.should_suppress(
            "provisional",
            "snmpsrc/c/udp/10.0.0.1:162",
            "1.3.6.1.6.3.1.1.5.3",
            t + 1
        ));
        // Different source key (e.g. another sender port) → not suppressed.
        assert!(!dedup.should_suppress(
            "provisional",
            "snmpsrc/c/udp/10.0.0.1:55555",
            "1.3.6.1.6.3.1.1.5.3",
            t + 1
        ));
        // Verified domain is never suppressed by a prior provisional entry.
        assert!(!dedup.should_suppress(
            "verified:sha256:x",
            "snmpsrc/c/udp/10.0.0.1:162",
            "1.3.6.1.6.3.1.1.5.3",
            t + 1
        ));
        // Outside the window → not suppressed.
        assert!(!dedup.should_suppress(
            "provisional",
            "snmpsrc/c/udp/10.0.0.1:162",
            "1.3.6.1.6.3.1.1.5.3",
            t + SNMP_DEDUP_WINDOW_NS + 1
        ));
    }

    #[test]
    fn v1_agent_address_is_decoded_as_claim() {
        let packet = encode_sequence(&[
            encode_integer(0),
            encode_octet_string(b"public"),
            encode_tag(
                0xA4,
                &[
                    encode_oid("1.3.6.1.4.1.9"),
                    encode_tag(0x40, &[192, 0, 2, 44]),
                    encode_integer(6),
                    encode_integer(1),
                    encode_tag(0x43, &[0x00, 0x00, 0x01, 0x00]),
                    encode_sequence(&[]),
                ]
                .concat(),
            ),
        ]);
        let event = parse_snmp_message(&packet, "10.1.2.3:162", 7).unwrap();
        // Self-reported claim preserved as evidence.
        assert_eq!(event.agent_address, "192.0.2.44");
        assert_eq!(event.version, "v1");
    }

    #[test]
    fn extracts_peer_address_from_oid_index_suffix() {
        let pattern = SnmpOidPattern {
            fact_type: "bgp_peer_backward_transition".to_string(),
            trap_oid: "1.3.6.1.4.1.6527.3.1.3.14.0.7".to_string(),
            fields: BTreeMap::new(),
            field_schema: BTreeMap::from([("peer_address".to_string(), "ip".to_string())]),
            index_suffix_fields: vec![IndexSuffixField {
                field: "peer_address".to_string(),
                byte_offset: -4,
                suffix_type: "ipv4".to_string(),
            }],
        };
        let extractor = SnmpFactExtractor {
            patterns: vec![pattern],
        };

        // Nokia trap OID with peer 10.0.0.1 encoded as suffix: .1.10.0.0.1
        let event = SnmpTrapEvent {
            timestamp_ns: 42,
            peer_addr: "192.168.1.100:162".to_string(),
            version: "v2c".to_string(),
            community: "public".to_string(),
            event_type: "snmp_enterprise_specific".to_string(),
            trap_oid: "1.3.6.1.4.1.6527.3.1.3.14.0.7.1.10.0.0.1".to_string(),
            category: "enterprise".to_string(),
            message: String::new(),
            enterprise_oid: "1.3.6.1.4.1.6527".to_string(),
            generic_trap: None,
            specific_trap: None,
            uptime_ticks: None,
            varbinds: vec![],
            parse_error: String::new(),
            raw_hex: String::new(),
            raw_len: 0,
            usm_security_name: String::new(),
            usm_auth_protocol: String::new(),
            engine_id: String::new(),
            agent_address: String::new(),
        };

        let fact = extractor.extract(&event).expect("should extract fact");
        assert_eq!(fact.fact_type, "bgp_peer_backward_transition");
        assert_eq!(fact.fields.get("peer_address").unwrap(), "10.0.0.1");
    }

    #[test]
    fn oid_suffix_too_short_returns_none_for_field() {
        let components: Vec<&str> = vec!["10", "0", "0"];
        let isf = IndexSuffixField {
            field: "peer_address".to_string(),
            byte_offset: -4,
            suffix_type: "ipv4".to_string(),
        };
        assert!(parse_index_suffix(&components, &isf).is_none());
    }

    fn encode_varbind(oid: &str, value_tlv: Vec<u8>) -> Vec<u8> {
        encode_sequence(&[encode_oid(oid), value_tlv])
    }

    fn encode_sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        encode_tag(0x30, &parts.concat())
    }

    fn encode_integer(value: i64) -> Vec<u8> {
        let mut bytes = value.to_be_bytes().to_vec();
        while bytes.len() > 1
            && ((bytes[0] == 0x00 && bytes[1] & 0x80 == 0)
                || (bytes[0] == 0xff && bytes[1] & 0x80 == 0x80))
        {
            bytes.remove(0);
        }
        encode_tag(0x02, &bytes)
    }

    fn encode_octet_string(value: &[u8]) -> Vec<u8> {
        encode_tag(0x04, value)
    }

    fn encode_oid(oid: &str) -> Vec<u8> {
        let parts = oid
            .split('.')
            .map(|part| part.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        let mut encoded = vec![(parts[0] * 40 + parts[1]) as u8];
        for part in parts.iter().skip(2) {
            let mut stack = vec![(part & 0x7f) as u8];
            let mut value = *part >> 7;
            while value > 0 {
                stack.push(((value & 0x7f) as u8) | 0x80);
                value >>= 7;
            }
            stack.reverse();
            encoded.extend(stack);
        }
        encode_tag(0x06, &encoded)
    }

    fn encode_tag(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend(encode_length(value.len()));
        out.extend_from_slice(value);
        out
    }

    fn encode_length(len: usize) -> Vec<u8> {
        if len < 0x80 {
            return vec![len as u8];
        }
        if len <= 0xff {
            return vec![0x81, len as u8];
        }
        vec![0x82, ((len >> 8) & 0xff) as u8, (len & 0xff) as u8]
    }
}

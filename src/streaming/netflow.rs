//! NetFlow v9 / IPFIX (v10) UDP flow receiver (BN-021d).
//!
//! Identity trust boundary:
//! - The observed UDP transport endpoint (`SocketAddr`) is *evidence*,
//!   never proof of exporter ownership. Two exporters behind one NAT
//!   address are separated by their transport tuple (IP + source port),
//!   protocol version, collector/ingress scope, and observation domain —
//!   but that separation is *observational*, not cryptographic.
//! - Every published flow carries a provisional source key
//!   (`flowsrc/...`) as `TelemetryUpdate.target`. The bare transport IP
//!   remains available as `exporter_address` for compatibility.
//! - Plain UDP observations NEVER acquire a canonical `device_id`:
//!   [`resolve_flow_identity`] only consults credential-pinned trusted
//!   bindings through a transport-attested [`VerifiedIngressPrincipal`],
//!   which a UDP packet can never produce.
//!
//! Template lifecycle (RFC 3954 §for v9, RFC 7011 §8.4 for IPFIX/UDP):
//! - Templates are scoped to (collector, transport endpoint, protocol
//!   version, observation domain, template id).
//! - All cache dimensions are bounded; eviction is oldest-first.
//! - Templates expire after `template_ttl_secs` without refresh and must
//!   not decode data until re-received.
//! - NetFlow v9 exporter restarts are detected via `sys_uptime`
//!   regression; IPFIX via `export_time` regression. Detection clears the
//!   affected domain's templates (fail closed).
//! - IPFIX template withdrawals (Template Records with Field Count = 0)
//!   are IGNORED over UDP per RFC 7011 §8.4 — UDP has no reliable
//!   withdrawal semantics. TCP/SCTP withdrawal handling is explicitly
//!   out of scope and documented as future work.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use metrics::{counter, gauge};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::NetflowConfig;
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;
use crate::telemetry_identity::{
    IdentityResolution, IdentityResolver, IdentityStatus, TelemetrySourceIdentity, TrustedBinding,
    VerifiedIngressPrincipal,
};

// ── Netflow v9 field type IDs (RFC 3954) ────────────────────────────────────

const FIELD_IN_BYTES: u16 = 1;
const FIELD_IN_PKTS: u16 = 2;
const FIELD_PROTOCOL: u16 = 4;
const FIELD_TCP_FLAGS: u16 = 6;
const FIELD_SRC_PORT: u16 = 7;
const FIELD_SRC_ADDR: u16 = 8;
const FIELD_DST_PORT: u16 = 11;
const FIELD_DST_ADDR: u16 = 12;
const FIELD_INPUT_SNMP: u16 = 10;
const FIELD_OUTPUT_SNMP: u16 = 14;
const FIELD_FIRST_SWITCHED: u16 = 22;
const FIELD_LAST_SWITCHED: u16 = 21;
const FIELD_BGP_SRC_AS: u16 = 16;
const FIELD_BGP_DST_AS: u16 = 17;
const FIELD_DIRECTION: u16 = 61;

// IPFIX (v10) uses the same field IDs for the basic IPv4 5-tuple.

const MAX_UDP_PAYLOAD: usize = 65535;

// NetFlow v9 flowset ids.
const V9_TEMPLATE_FLOWSET: u16 = 0;
const V9_OPTIONS_FLOWSET: u16 = 1;

// IPFIX set ids (RFC 7011 §3.3.2 / §8).
const IPFIX_TEMPLATE_SET: u16 = 2;
const IPFIX_OPTIONS_TEMPLATE_SET: u16 = 3;
const IPFIX_MIN_TEMPLATE_ID: u16 = 256;

/// IPFIX field length sentinel values meaning "variable length" (RFC 7011 §3.2).
const IPFIX_VARLEN_ZERO: u16 = 0;
const IPFIX_VARLEN_MAX: u16 = u16::MAX;

/// Approximate per-template heap overhead used for cache-byte accounting.
const TEMPLATE_OVERHEAD_BYTES: usize = 160;

// ── Bounded template cache ──────────────────────────────────────────────────

/// Configurable bounds for the flow template cache. Every dimension is
/// capped so a flood of small UDP datagrams cannot exhaust memory.
#[derive(Debug, Clone)]
pub struct FlowCacheLimits {
    /// Maximum (collector, transport endpoint, protocol) contexts.
    pub max_transport_contexts: usize,
    /// Maximum observation domains (source-id / ODID) per context.
    pub max_domains_per_context: usize,
    /// Maximum template definitions per observation domain.
    pub max_templates_per_domain: usize,
    /// Maximum field specifiers accepted in one template.
    pub max_fields_per_template: usize,
    /// Maximum total cached template definitions across all contexts.
    pub max_total_templates: usize,
    /// Approximate total cache memory budget in bytes.
    pub max_cache_bytes: usize,
    /// How long a template may decode data without being refreshed.
    pub template_ttl: Duration,
    /// How long an idle transport context is retained before eviction.
    pub context_idle_ttl: Duration,
}

impl FlowCacheLimits {
    fn from_config(cfg: &NetflowConfig) -> Self {
        Self {
            max_transport_contexts: cfg.max_flow_contexts.max(1),
            max_domains_per_context: cfg.max_flow_domains_per_context.max(1),
            max_templates_per_domain: cfg.max_flow_templates_per_domain.max(1),
            max_fields_per_template: cfg.max_flow_fields_per_template.max(1),
            max_total_templates: cfg.max_flow_total_templates.max(1),
            max_cache_bytes: cfg.max_flow_cache_bytes.max(1024),
            template_ttl: Duration::from_secs(cfg.flow_template_ttl_secs.max(1)),
            context_idle_ttl: Duration::from_secs(cfg.flow_context_idle_secs.max(1)),
        }
    }
}

/// A single IPFIX/v9 Information Element specifier as stored in a template.
/// `pen` preserves the enterprise-number distinction: a field with
/// `pen.is_some()` is an enterprise-specific IE and MUST NOT be decoded
/// using the standard IANA field definition for the same numeric id.
#[derive(Debug, Clone, Copy)]
struct FlowField {
    /// Information element id with the enterprise bit masked off.
    ie: u16,
    len: u16,
    /// Enterprise number (PEN) when the enterprise bit was set.
    pen: Option<u32>,
}

impl FlowField {
    fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

/// One cached template definition.
#[derive(Debug)]
struct TemplateDef {
    fields: Vec<FlowField>,
    received_at: Instant,
    approx_bytes: usize,
    /// False when the template cannot be used to frame data records
    /// (e.g. it declares variable-length fields we do not support).
    decodable: bool,
}

impl TemplateDef {
    fn new(fields: Vec<FlowField>, decodable: bool) -> Self {
        let approx_bytes =
            TEMPLATE_OVERHEAD_BYTES + fields.iter().map(|f| f.approx_bytes()).sum::<usize>();
        Self {
            fields,
            received_at: Instant::now(),
            approx_bytes,
            decodable,
        }
    }
}

/// Templates for one observation domain inside one transport context.
#[derive(Debug)]
struct DomainTemplates {
    last_seen: Instant,
    templates: HashMap<u16, TemplateDef>,
    approx_bytes: usize,
    /// Bumped when a probable exporter restart is detected; fences stale
    /// template generations (surfaced as `session_epoch` evidence).
    generation: u64,
    // ── NetFlow v9 restart detection (per observation domain) ──
    last_uptime_ms: Option<u64>,
    // ── IPFIX restart/gap detection (per observation domain) ──
    last_export_time: Option<u64>,
    last_seq: Option<u32>,
}

impl DomainTemplates {
    fn new(now: Instant) -> Self {
        Self {
            last_seen: now,
            templates: HashMap::new(),
            approx_bytes: 0,
            generation: 0,
            last_uptime_ms: None,
            last_export_time: None,
            last_seq: None,
        }
    }
}

/// One observed transport endpoint + protocol version.
#[derive(Debug)]
struct TransportContext {
    last_seen: Instant,
    domains: HashMap<u32, DomainTemplates>,
    approx_bytes: usize,
    template_count: usize,
}

impl TransportContext {
    fn new(now: Instant) -> Self {
        Self {
            last_seen: now,
            domains: HashMap::new(),
            approx_bytes: 0,
            template_count: 0,
        }
    }
}

/// Cache key: (collector/ingress scope, UDP transport endpoint, protocol).
/// The source port is part of the key, so a port change is a NEW context
/// and can never inherit the old context's templates.
type ContextKey = (String, SocketAddr, u16);

/// Result of a template lookup, kept explicit so callers can count the
/// correct diagnostic and never guess a layout.
#[derive(Debug)]
enum TemplateLookup {
    Live(TemplateDef),
    Expired,
    Unsupported,
    Unknown,
}

struct FlowTemplateCache {
    collector_id: String,
    limits: FlowCacheLimits,
    contexts: HashMap<ContextKey, TransportContext>,
    total_templates: usize,
    total_bytes: usize,
    packets_since_sweep: u32,
}

impl FlowTemplateCache {
    fn new(collector_id: String, limits: FlowCacheLimits) -> Self {
        Self {
            collector_id,
            limits,
            contexts: HashMap::new(),
            total_templates: 0,
            total_bytes: 0,
            packets_since_sweep: 0,
        }
    }

    fn context_key(&self, peer: SocketAddr, version: u16) -> ContextKey {
        (self.collector_id.clone(), peer, version)
    }

    /// Ensure the transport context exists, evicting the oldest context
    /// when at capacity. Also runs the periodic idle-context sweep.
    fn ensure_context(&mut self, key: &ContextKey, now: Instant) {
        self.packets_since_sweep += 1;
        if self.packets_since_sweep >= 256 {
            self.packets_since_sweep = 0;
            self.sweep_idle_contexts(now);
        }
        if self.contexts.contains_key(key) {
            return;
        }
        while self.contexts.len() >= self.limits.max_transport_contexts {
            if !self.evict_oldest_context() {
                break;
            }
        }
        self.contexts
            .insert(key.clone(), TransportContext::new(now));
    }

    fn sweep_idle_contexts(&mut self, now: Instant) {
        let idle = self.limits.context_idle_ttl;
        let expired: Vec<ContextKey> = self
            .contexts
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_seen) > idle)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            if let Some(ctx) = self.contexts.remove(&key) {
                self.total_templates = self.total_templates.saturating_sub(ctx.template_count);
                self.total_bytes = self.total_bytes.saturating_sub(ctx.approx_bytes);
                counter!("bonsai_netflow_context_evicted_total", "reason" => "idle").increment(1);
            }
        }
    }

    /// Remove the context with the oldest `last_seen`. Returns false when
    /// the cache is empty.
    fn evict_oldest_context(&mut self) -> bool {
        let oldest = self
            .contexts
            .iter()
            .min_by_key(|(_, c)| c.last_seen)
            .map(|(k, _)| k.clone());
        let Some(key) = oldest else { return false };
        if let Some(ctx) = self.contexts.remove(&key) {
            self.total_templates = self.total_templates.saturating_sub(ctx.template_count);
            self.total_bytes = self.total_bytes.saturating_sub(ctx.approx_bytes);
            counter!("bonsai_netflow_context_evicted_total", "reason" => "capacity").increment(1);
            gauge!("bonsai_netflow_template_cache_bytes").set(self.total_bytes as f64);
            true
        } else {
            false
        }
    }

    /// Get (creating if needed) the domain entry inside an existing
    /// context, enforcing the per-context domain bound. Returns the
    /// domain plus accounting for any domains evicted to make room, so
    /// the caller can keep global totals exact.
    fn domain_mut(
        ctx: &mut TransportContext,
        domain_id: u32,
        now: Instant,
        max_domains: usize,
    ) -> (&mut DomainTemplates, usize, usize) {
        let mut evicted_templates = 0usize;
        let mut evicted_bytes = 0usize;
        if !ctx.domains.contains_key(&domain_id) {
            while ctx.domains.len() >= max_domains {
                // Evict the domain with the oldest activity.
                let oldest = ctx
                    .domains
                    .iter()
                    .min_by_key(|(_, d)| d.last_seen)
                    .map(|(k, _)| *k);
                let Some(oldest_id) = oldest else { break };
                if let Some(dom) = ctx.domains.remove(&oldest_id) {
                    evicted_templates += dom.templates.len();
                    evicted_bytes += dom.approx_bytes;
                    ctx.approx_bytes = ctx.approx_bytes.saturating_sub(dom.approx_bytes);
                    ctx.template_count = ctx.template_count.saturating_sub(dom.templates.len());
                    counter!("bonsai_netflow_domain_evicted_total").increment(1);
                }
            }
            ctx.domains.insert(domain_id, DomainTemplates::new(now));
        }
        let dom = ctx
            .domains
            .get_mut(&domain_id)
            .expect("domain just ensured");
        dom.last_seen = now;
        (dom, evicted_templates, evicted_bytes)
    }

    /// Record packet-level liveness and restart indicators for a domain,
    /// returning the current template generation. A detected restart
    /// clears that domain's templates so stale definitions cannot decode
    /// the new generation's data.
    fn note_packet(
        &mut self,
        key: &ContextKey,
        domain_id: u32,
        now: Instant,
        uptime_ms: Option<u64>,
        export_time: Option<u64>,
        seq: Option<u32>,
    ) -> u64 {
        let (restarted, generation) = {
            let Some(ctx) = self.contexts.get_mut(key) else {
                return 0;
            };
            ctx.last_seen = now;
            // Ensure domain exists (bounded).
            let (dom, evicted_t, evicted_b) =
                Self::domain_mut(ctx, domain_id, now, self.limits.max_domains_per_context);
            self.total_templates = self.total_templates.saturating_sub(evicted_t);
            self.total_bytes = self.total_bytes.saturating_sub(evicted_b);

            // Restart detection — conservative, per protocol:
            //  - NetFlow v9: sys_uptime is milliseconds since exporter boot;
            //    a decrease can only mean a reboot.
            //  - IPFIX: export_time regression indicates the exporting clock
            //    moved backwards (e.g. reboot before NTP). Sequence numbers
            //    are NOT used: UDP reordering/loss makes them unreliable.
            let restarted = if let (Some(new), Some(old)) = (uptime_ms, dom.last_uptime_ms) {
                new < old
            } else if let (Some(new), Some(old)) = (export_time, dom.last_export_time) {
                new < old
            } else {
                false
            };

            if let Some(u) = uptime_ms {
                dom.last_uptime_ms = Some(u);
            }
            if let Some(t) = export_time {
                dom.last_export_time = Some(t);
            }
            if let Some(s) = seq {
                if let Some(last) = dom.last_seq {
                    // Forward distance on the u32 sequence space. Large
                    // forward jumps are gaps (loss/reorder); a backwards
                    // move is treated as reorder/loss evidence only — it
                    // does NOT invalidate templates (see restart policy).
                    let fwd = s.wrapping_sub(last);
                    if fwd > 1 && fwd < (1 << 31) {
                        counter!("bonsai_netflow_sequence_gap_total").increment(1);
                    }
                }
                dom.last_seq = Some(s);
            }
            if restarted {
                dom.generation = dom.generation.saturating_add(1);
            }
            (restarted, dom.generation)
        };
        if restarted {
            // Clear AFTER releasing the domain borrow.
            self.clear_domain_templates(key, domain_id);
            counter!("bonsai_netflow_restart_detected_total").increment(1);
        }
        generation
    }

    /// Remove every template in one domain, fixing all accounting.
    fn clear_domain_templates(&mut self, key: &ContextKey, domain_id: u32) {
        let Some(ctx) = self.contexts.get_mut(key) else {
            return;
        };
        let Some(dom) = ctx.domains.get_mut(&domain_id) else {
            return;
        };
        let bytes = dom.approx_bytes;
        let count = dom.templates.len();
        dom.templates.clear();
        dom.approx_bytes = 0;
        ctx.approx_bytes = ctx.approx_bytes.saturating_sub(bytes);
        ctx.template_count = ctx.template_count.saturating_sub(count);
        self.total_templates = self.total_templates.saturating_sub(count);
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
    }

    /// Insert parsed template definitions for one domain, enforcing the
    /// per-domain and global bounds. Re-defining an existing template id
    /// replaces the old definition (RFC 7011 §8.4 UDP semantics).
    fn insert_templates(
        &mut self,
        key: &ContextKey,
        domain_id: u32,
        defs: Vec<(u16, TemplateDef)>,
    ) {
        if defs.is_empty() {
            return;
        }
        {
            let Some(ctx) = self.contexts.get_mut(key) else {
                return;
            };
            let Some(dom) = ctx.domains.get_mut(&domain_id) else {
                return;
            };
            for (tid, new_def) in defs {
                let new_bytes = new_def.approx_bytes;
                // Per-domain bound: evict oldest-received templates first.
                // A re-definition of an existing id never triggers eviction.
                while dom.templates.len() >= self.limits.max_templates_per_domain
                    && !dom.templates.contains_key(&tid)
                {
                    let oldest = dom
                        .templates
                        .iter()
                        .min_by_key(|(_, d)| d.received_at)
                        .map(|(k, _)| *k);
                    let Some(old_tid) = oldest else { break };
                    if let Some(old) = dom.templates.remove(&old_tid) {
                        dom.approx_bytes = dom.approx_bytes.saturating_sub(old.approx_bytes);
                        ctx.approx_bytes = ctx.approx_bytes.saturating_sub(old.approx_bytes);
                        ctx.template_count = ctx.template_count.saturating_sub(1);
                        self.total_templates = self.total_templates.saturating_sub(1);
                        self.total_bytes = self.total_bytes.saturating_sub(old.approx_bytes);
                        counter!("bonsai_netflow_template_evicted_total", "reason" => "domain_cap")
                            .increment(1);
                    }
                }
                match dom.templates.insert(tid, new_def) {
                    Some(old) => {
                        // Replacement: count unchanged, bytes swap.
                        dom.approx_bytes = dom.approx_bytes.saturating_sub(old.approx_bytes);
                        ctx.approx_bytes = ctx.approx_bytes.saturating_sub(old.approx_bytes);
                        self.total_bytes = self.total_bytes.saturating_sub(old.approx_bytes);
                    }
                    None => {
                        ctx.template_count += 1;
                        self.total_templates += 1;
                    }
                }
                // `new_def` was moved into the map; its byte cost was
                // captured before the insert.
                dom.approx_bytes += new_bytes;
                ctx.approx_bytes += new_bytes;
                self.total_bytes += new_bytes;
            }
        }
        gauge!("bonsai_netflow_template_cache_entries").set(self.total_templates as f64);
        gauge!("bonsai_netflow_template_cache_bytes").set(self.total_bytes as f64);
        self.enforce_global_limits();
    }

    /// Global bounds: total template count and approximate bytes. Evicts
    /// globally-oldest templates first, in batches down to 90% of the
    /// limit so a sustained flood cannot cause O(n²) eviction scans.
    fn enforce_global_limits(&mut self) {
        let over_count = self.total_templates > self.limits.max_total_templates;
        let over_bytes = self.total_bytes > self.limits.max_cache_bytes;
        if !over_count && !over_bytes {
            return;
        }
        // Gather every cached template with its age.
        let mut all: Vec<(Instant, ContextKey, u32, u16)> = Vec::new();
        for (ck, ctx) in &self.contexts {
            for (did, dom) in &ctx.domains {
                for tid in dom.templates.keys() {
                    all.push((
                        *dom.templates.get(tid).map(|d| &d.received_at).unwrap(),
                        ck.clone(),
                        *did,
                        *tid,
                    ));
                }
            }
        }
        all.sort_by_key(|e| e.0);
        let target_templates = self.limits.max_total_templates * 9 / 10;
        let target_bytes = self.limits.max_cache_bytes * 9 / 10;
        let mut removed = 0usize;
        for (_, ck, did, tid) in all {
            if self.total_templates <= target_templates && self.total_bytes <= target_bytes {
                break;
            }
            if self.remove_template(&ck, did, tid) {
                removed += 1;
            }
        }
        if removed > 0 {
            counter!("bonsai_netflow_template_evicted_total", "reason" => "global_cap")
                .increment(removed as u64);
            gauge!("bonsai_netflow_template_cache_entries").set(self.total_templates as f64);
            gauge!("bonsai_netflow_template_cache_bytes").set(self.total_bytes as f64);
        }
    }

    fn remove_template(&mut self, key: &ContextKey, domain_id: u32, tid: u16) -> bool {
        let Some(ctx) = self.contexts.get_mut(key) else {
            return false;
        };
        let Some(dom) = ctx.domains.get_mut(&domain_id) else {
            return false;
        };
        let Some(def) = dom.templates.remove(&tid) else {
            return false;
        };
        dom.approx_bytes = dom.approx_bytes.saturating_sub(def.approx_bytes);
        ctx.approx_bytes = ctx.approx_bytes.saturating_sub(def.approx_bytes);
        ctx.template_count = ctx.template_count.saturating_sub(1);
        self.total_templates = self.total_templates.saturating_sub(1);
        self.total_bytes = self.total_bytes.saturating_sub(def.approx_bytes);
        true
    }

    /// Look up a template for decoding. Expired templates are treated as
    /// absent: they must not decode data until refreshed (RFC 7011 §8.4
    /// collector-side lifetime; RFC 3954 template refresh for v9).
    fn lookup_template(
        &self,
        key: &ContextKey,
        domain_id: u32,
        template_id: u16,
        now: Instant,
    ) -> TemplateLookup {
        let Some(ctx) = self.contexts.get(key) else {
            return TemplateLookup::Unknown;
        };
        let Some(dom) = ctx.domains.get(&domain_id) else {
            return TemplateLookup::Unknown;
        };
        let Some(def) = dom.templates.get(&template_id) else {
            return TemplateLookup::Unknown;
        };
        if now.duration_since(def.received_at) > self.limits.template_ttl {
            return TemplateLookup::Expired;
        }
        if !def.decodable {
            return TemplateLookup::Unsupported;
        }
        // Clone so callers can decode without holding the cache borrow.
        TemplateLookup::Live(TemplateDef {
            fields: def.fields.clone(),
            received_at: def.received_at,
            approx_bytes: def.approx_bytes,
            decodable: def.decodable,
        })
    }

    /// Current template generation for a domain (0 when absent).
    fn generation(&self, key: &ContextKey, domain_id: u32) -> u64 {
        self.contexts
            .get(key)
            .and_then(|c| c.domains.get(&domain_id))
            .map(|d| d.generation)
            .unwrap_or(0)
    }
}

// ── Protocol identity ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowProtocol {
    NetflowV9,
    Ipfix,
}

impl FlowProtocol {
    fn from_version(version: u16) -> Option<Self> {
        match version {
            9 => Some(Self::NetflowV9),
            10 => Some(Self::Ipfix),
            _ => None,
        }
    }
    /// Short tag used inside the provisional source key.
    fn key_tag(self) -> &'static str {
        match self {
            Self::NetflowV9 => "nf9",
            Self::Ipfix => "ipfix",
        }
    }
    /// Protocol label used in `TelemetrySourceIdentity.protocol`.
    fn identity_protocol(self) -> &'static str {
        match self {
            Self::NetflowV9 => "netflow9",
            Self::Ipfix => "ipfix",
        }
    }
    /// Domain selector label: NetFlow source-id vs IPFIX ODID.
    fn domain_kind(self) -> &'static str {
        match self {
            Self::NetflowV9 => "source_id",
            Self::Ipfix => "observation_domain_id",
        }
    }
    fn version(self) -> u16 {
        match self {
            Self::NetflowV9 => 9,
            Self::Ipfix => 10,
        }
    }
    fn domain_key_tag(self) -> &'static str {
        match self {
            Self::NetflowV9 => "sid",
            Self::Ipfix => "odid",
        }
    }
}

/// Percent-encode a collector/ingress scope component so it can never
/// inject key delimiters. Only `[A-Za-z0-9._~-]` pass through.
fn encode_key_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'~' | b'-' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Collision-resistant, unambiguous provisional source key.
///
/// Format: `flowsrc/{collector}/{proto}/{transport}/{domain_kind}{id}`
/// e.g. `flowsrc/site-a/nf9/10.0.0.1:49152/sid7`.
///
/// Every component is delimiter-free: the collector is percent-encoded,
/// the transport endpoint uses canonical `SocketAddr` display, and the
/// protocol/domain tokens are fixed. Two exporters sharing a NAT address
/// but using different UDP source ports (or domains, or protocols, or
/// collectors) get distinct keys.
///
/// IMPORTANT: uniqueness of this key is *observational* separation, not
/// proof of canonical exporter ownership. See [`resolve_flow_identity`].
fn provisional_source_key(
    collector_id: &str,
    proto: FlowProtocol,
    peer: SocketAddr,
    domain_id: u32,
) -> String {
    format!(
        "flowsrc/{}/{}/{}/{}{}",
        encode_key_component(collector_id),
        proto.key_tag(),
        peer,
        proto.domain_key_tag(),
        domain_id
    )
}

/// Identity context for one (transport endpoint, protocol, domain)
/// observed within a single packet.
struct FlowSourceCtx {
    source_key: String,
    exporter_ip: String,
    exporter_endpoint: String,
    collector_id: String,
    proto: FlowProtocol,
    domain_id: u32,
    generation: u64,
}

impl FlowSourceCtx {
    fn build(
        cache: &FlowTemplateCache,
        key: &ContextKey,
        proto: FlowProtocol,
        peer: SocketAddr,
        domain_id: u32,
    ) -> Self {
        let collector_id = cache.collector_id.clone();
        let generation = cache.generation(key, domain_id);
        Self {
            source_key: provisional_source_key(&collector_id, proto, peer, domain_id),
            exporter_ip: peer.ip().to_string(),
            exporter_endpoint: peer.to_string(),
            collector_id,
            proto,
            domain_id,
            generation,
        }
    }

    /// The provisional identity envelope carried by every published flow.
    /// `device_id`/`verified_ingress` are never populated from UDP data.
    fn observed_identity(&self) -> TelemetrySourceIdentity {
        TelemetrySourceIdentity::observed(self.exporter_endpoint.clone())
            .with_protocol(self.proto.identity_protocol())
            .with_exporter(self.source_key.clone())
            .with_domain(self.domain_id)
            .with_collector(self.collector_id.clone())
            .with_session_epoch(self.generation)
    }
}

/// Resolve a flow observation against trusted bindings.
///
/// This is the smallest reusable contract for future authenticated flow
/// ingress (e.g. a TLS/DTLS-terminating collector relay that can produce
/// a transport-attested [`VerifiedIngressPrincipal`]).
///
/// - `verified_ingress = Some(attested principal)` → resolution goes
///   through [`IdentityResolver::resolve_verified_ingress`], which only
///   lets credential-pinned bindings claim the observation.
/// - `verified_ingress = None` (plain UDP, or any unattested transport)
///   → the observation stays `Provisional`/`Unknown`. Operator bindings
///   matched on transport IP, port, ODID, or exporter claim can NEVER
///   confer `device_id` here — those fields are spoofable UDP evidence.
pub fn resolve_flow_identity(
    observed: &TelemetrySourceIdentity,
    bindings: &[TrustedBinding],
    verified_ingress: Option<&VerifiedIngressPrincipal>,
) -> IdentityResolution {
    match verified_ingress {
        Some(principal) => {
            IdentityResolver::resolve_verified_ingress(observed, bindings, principal)
        }
        None => IdentityResolution {
            status: if observed.exporter_id.is_some() || observed.reported_identity.is_some() {
                IdentityStatus::Provisional
            } else {
                IdentityStatus::Unknown
            },
            device_id: None,
            canonical_exporter_id: None,
            binding_id: None,
            provenance: None,
            observed: observed.clone(),
        },
    }
}

// ── Receiver entry point ────────────────────────────────────────────────────

pub async fn run_netflow_receiver(
    cfg: NetflowConfig,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let socket = UdpSocket::bind(&cfg.udp_addr).await?;
    info!(addr = %cfg.udp_addr, "Netflow/IPFIX UDP receiver listening");

    let collector_id = if cfg.collector_id.trim().is_empty() {
        "default".to_string()
    } else {
        cfg.collector_id.trim().to_string()
    };
    let limits = FlowCacheLimits::from_config(&cfg);
    let mut cache = FlowTemplateCache::new(collector_id, limits);

    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, peer)) => {
                        let pkt = &buf[..len];
                        if let Err(e) = handle_packet(pkt, peer, &mut cache, &bus) {
                            debug!(%peer, error = %e, "netflow parse error");
                        }
                    }
                    Err(e) => warn!(error = %e, "netflow recv error"),
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }
    Ok(())
}

fn handle_packet(
    pkt: &[u8],
    peer: SocketAddr,
    cache: &mut FlowTemplateCache,
    bus: &Arc<InProcessBus>,
) -> Result<()> {
    if pkt.len() < 4 {
        bail!("packet too short");
    }
    let version = u16::from_be_bytes([pkt[0], pkt[1]]);
    let Some(proto) = FlowProtocol::from_version(version) else {
        bail!("unsupported netflow version {version}");
    };
    let now = Instant::now();
    let key = cache.context_key(peer, version);
    cache.ensure_context(&key, now);

    match proto {
        FlowProtocol::NetflowV9 => parse_v9(pkt, peer, &key, cache, bus, now),
        FlowProtocol::Ipfix => parse_ipfix(pkt, peer, &key, cache, bus, now),
    }
}

// ── Netflow v9 ────────────────────────────────────────────────────────────────

fn parse_v9(
    pkt: &[u8],
    peer: SocketAddr,
    key: &ContextKey,
    cache: &mut FlowTemplateCache,
    bus: &Arc<InProcessBus>,
    now: Instant,
) -> Result<()> {
    // v9 header: version(2) count(2) sys_uptime(4) unix_secs(4) seq(4) source_id(4) = 20 bytes
    if pkt.len() < 20 {
        bail!("v9 header too short");
    }
    let sys_uptime_ms = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as u64;
    let unix_secs = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]) as u64;
    let seq = u32::from_be_bytes([pkt[12], pkt[13], pkt[14], pkt[15]]);
    let source_id = u32::from_be_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]);

    cache.note_packet(key, source_id, now, Some(sys_uptime_ms), None, Some(seq));
    let src = FlowSourceCtx::build(cache, key, FlowProtocol::NetflowV9, peer, source_id);
    let observed = src.observed_identity();
    // Plain UDP: no trusted ingress attestation exists, so resolution is
    // always provisional — bindings can never claim this observation.
    let resolution = resolve_flow_identity(&observed, &[], None);

    let mut offset = 20usize;
    while offset + 4 <= pkt.len() {
        let flowset_id = u16::from_be_bytes([pkt[offset], pkt[offset + 1]]);
        let length = u16::from_be_bytes([pkt[offset + 2], pkt[offset + 3]]) as usize;
        if length < 4 || offset + length > pkt.len() {
            counter!("bonsai_netflow_malformed_packet_total", "version" => "9").increment(1);
            bail!("malformed v9 flowset at offset {offset}");
        }
        let flowset_data = &pkt[offset + 4..offset + length];

        match flowset_id {
            V9_TEMPLATE_FLOWSET => {
                let defs = parse_v9_template_set(flowset_data, &cache.limits);
                cache.insert_templates(key, source_id, defs);
            }
            V9_OPTIONS_FLOWSET => {
                // Options templates are not decoded; ignored safely.
            }
            template_id => {
                // Data FlowSet — template_id is the flowset id.
                match cache.lookup_template(key, source_id, template_id, now) {
                    TemplateLookup::Live(def) => {
                        emit_flows_v9(
                            flowset_data,
                            &def.fields,
                            sys_uptime_ms,
                            unix_secs,
                            &src,
                            &resolution,
                            bus,
                        );
                    }
                    TemplateLookup::Expired => {
                        counter!("bonsai_netflow_template_expired_drop_total", "version" => "9")
                            .increment(1);
                    }
                    TemplateLookup::Unsupported => {
                        counter!("bonsai_netflow_template_unsupported_drop_total", "version" => "9")
                            .increment(1);
                    }
                    TemplateLookup::Unknown => {
                        counter!("bonsai_netflow_unknown_template_drop_total", "version" => "9")
                            .increment(1);
                    }
                }
            }
        }
        offset += length;
    }
    Ok(())
}

/// Parse a v9 Template FlowSet into pending definitions. Returns only
/// fully-validated records; a truncated final record ends parsing but
/// never produces a partial template.
fn parse_v9_template_set(data: &[u8], limits: &FlowCacheLimits) -> Vec<(u16, TemplateDef)> {
    let mut defs = Vec::new();
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let template_id = u16::from_be_bytes([data[i], data[i + 1]]);
        let field_count = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if field_count > limits.max_fields_per_template {
            // Oversized declaration: skip this record if we can still
            // locate its end, otherwise stop (fail closed for the set).
            match i.checked_add(field_count.saturating_mul(4)) {
                Some(end) if end <= data.len() => {
                    i = end;
                    counter!("bonsai_netflow_template_rejected_total", "reason" => "field_count")
                        .increment(1);
                    continue;
                }
                _ => break,
            }
        }
        if i + field_count * 4 > data.len() {
            // Truncated record — stop; earlier complete records stand.
            break;
        }
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let ftype = u16::from_be_bytes([data[i], data[i + 1]]);
            let flen = u16::from_be_bytes([data[i + 2], data[i + 3]]);
            fields.push(FlowField {
                ie: ftype,
                len: flen,
                pen: None,
            });
            i += 4;
        }
        defs.push((template_id, TemplateDef::new(fields, true)));
    }
    defs
}

fn emit_flows_v9(
    data: &[u8],
    fields: &[FlowField],
    sys_uptime_ms: u64,
    unix_secs: u64,
    src: &FlowSourceCtx,
    resolution: &IdentityResolution,
    bus: &Arc<InProcessBus>,
) {
    let record_len: usize = fields.iter().map(|f| f.len as usize).sum();
    if record_len == 0 {
        return;
    }
    let mut offset = 0usize;
    while offset + record_len <= data.len() {
        let record = &data[offset..offset + record_len];
        if let Some(flow) = decode_record(record, fields, sys_uptime_ms, unix_secs) {
            publish_flow(flow, bus, src, resolution);
        }
        offset += record_len;
    }
}

// ── IPFIX (v10) ───────────────────────────────────────────────────────────────

fn parse_ipfix(
    pkt: &[u8],
    peer: SocketAddr,
    key: &ContextKey,
    cache: &mut FlowTemplateCache,
    bus: &Arc<InProcessBus>,
    now: Instant,
) -> Result<()> {
    // IPFIX header: version(2) length(2) export_time(4) seq(4) domain_id(4) = 16 bytes
    if pkt.len() < 16 {
        bail!("IPFIX header too short");
    }
    let declared_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    // RFC 7011 §3.1: the message Length covers the whole IPFIX message.
    // For UDP it must fit inside the datagram; any trailing bytes must be
    // zero padding, otherwise the datagram is malformed.
    if declared_len < 16 || declared_len > pkt.len() {
        counter!("bonsai_netflow_malformed_packet_total", "version" => "10").increment(1);
        bail!(
            "IPFIX message length {declared_len} invalid for datagram {}",
            pkt.len()
        );
    }
    if pkt[declared_len..].iter().any(|&b| b != 0) {
        counter!("bonsai_netflow_malformed_packet_total", "version" => "10").increment(1);
        bail!("IPFIX datagram has non-zero trailing bytes beyond message length");
    }
    let msg_end = declared_len;

    let export_time = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as u64;
    let seq = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);
    let domain_id = u32::from_be_bytes([pkt[12], pkt[13], pkt[14], pkt[15]]);

    cache.note_packet(key, domain_id, now, None, Some(export_time), Some(seq));
    let src = FlowSourceCtx::build(cache, key, FlowProtocol::Ipfix, peer, domain_id);
    let observed = src.observed_identity();
    let resolution = resolve_flow_identity(&observed, &[], None);

    let mut offset = 16usize;
    while offset + 4 <= msg_end {
        let set_id = u16::from_be_bytes([pkt[offset], pkt[offset + 1]]);
        let length = u16::from_be_bytes([pkt[offset + 2], pkt[offset + 3]]) as usize;
        if length < 4 || offset + length > msg_end {
            counter!("bonsai_netflow_malformed_packet_total", "version" => "10").increment(1);
            bail!("malformed IPFIX set at offset {offset}");
        }
        let set_data = &pkt[offset + 4..offset + length];

        match set_id {
            IPFIX_TEMPLATE_SET => {
                let defs = parse_ipfix_template_set(set_data, &cache.limits);
                cache.insert_templates(key, domain_id, defs);
            }
            IPFIX_OPTIONS_TEMPLATE_SET => {
                // Options templates (incl. withdrawals) are not decoded.
                // Over UDP, withdrawal records (Field Count = 0) inside a
                // Template Set are ignored per RFC 7011 §8.4 — see
                // parse_ipfix_template_set.
            }
            template_id if template_id >= IPFIX_MIN_TEMPLATE_ID => {
                match cache.lookup_template(key, domain_id, template_id, now) {
                    TemplateLookup::Live(def) => {
                        // IPFIX uses export_time as unix seconds (no uptime offset).
                        emit_flows_v9(
                            set_data,
                            &def.fields,
                            0,
                            export_time,
                            &src,
                            &resolution,
                            bus,
                        );
                    }
                    TemplateLookup::Expired => {
                        counter!("bonsai_netflow_template_expired_drop_total", "version" => "10")
                            .increment(1);
                    }
                    TemplateLookup::Unsupported => {
                        counter!("bonsai_netflow_template_unsupported_drop_total", "version" => "10")
                            .increment(1);
                    }
                    TemplateLookup::Unknown => {
                        counter!("bonsai_netflow_unknown_template_drop_total", "version" => "10")
                            .increment(1);
                    }
                }
            }
            _ => {}
        }
        offset += length;
    }
    Ok(())
}

/// Parse an IPFIX Template Set (Set ID = 2) into pending definitions.
///
/// RFC 7011 §8.4 (UDP): a Template Record with Field Count = 0 is a
/// withdrawal — over UDP it MUST be ignored (no deletion). Only records
/// with Field Count > 0 produce/replace template definitions.
///
/// Every field specifier is bounds-checked before reading; enterprise
/// fields consume their 4-byte PEN and keep `pen = Some(pen)` so the
/// decoder never mistakes them for standard IEs.
fn parse_ipfix_template_set(data: &[u8], limits: &FlowCacheLimits) -> Vec<(u16, TemplateDef)> {
    let mut defs = Vec::new();
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let template_id = u16::from_be_bytes([data[i], data[i + 1]]);
        let field_count = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 4;

        if template_id < IPFIX_MIN_TEMPLATE_ID {
            // Template IDs 0-255 are reserved for set ids — malformed.
            break;
        }
        if field_count == 0 {
            // UDP template withdrawal → MUST be ignored (RFC 7011 §8.4).
            counter!("bonsai_netflow_udp_withdrawal_ignored_total").increment(1);
            continue;
        }
        if field_count > limits.max_fields_per_template {
            counter!("bonsai_netflow_template_rejected_total", "reason" => "field_count")
                .increment(1);
            break;
        }

        let mut fields = Vec::with_capacity(field_count);
        let mut decodable = true;
        let mut malformed = false;
        for _ in 0..field_count {
            // Field specifier: IE id(2) + length(2) [+ enterprise(4)].
            if i + 4 > data.len() {
                malformed = true;
                break;
            }
            let raw_type = u16::from_be_bytes([data[i], data[i + 1]]);
            let flen = u16::from_be_bytes([data[i + 2], data[i + 3]]);
            let enterprise = (raw_type & 0x8000) != 0;
            let ie = raw_type & 0x7FFF;
            i += 4;
            let pen = if enterprise {
                if i + 4 > data.len() {
                    malformed = true;
                    break;
                }
                let pen = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                i += 4;
                Some(pen)
            } else {
                None
            };
            // Variable-length fields cannot be framed by a fixed record
            // length — mark the template undecodable rather than guess.
            if flen == IPFIX_VARLEN_ZERO || flen == IPFIX_VARLEN_MAX {
                decodable = false;
            }
            fields.push(FlowField { ie, len: flen, pen });
        }
        if malformed {
            // Discard the whole pending set — a corrupt specifier means we
            // cannot trust record boundaries for anything after it.
            defs.clear();
            counter!("bonsai_netflow_template_rejected_total", "reason" => "truncated")
                .increment(1);
            break;
        }
        defs.push((template_id, TemplateDef::new(fields, decodable)));
    }
    defs
}

// ── Flow decoding ─────────────────────────────────────────────────────────────

struct FlowRecord {
    src_address: String,
    dst_address: String,
    src_port: u16,
    dst_port: u16,
    protocol: u8,
    bytes: u64,
    packets: u64,
    flow_start_ns: i64,
    flow_end_ns: i64,
    /// DS-1 T5: TCP flags byte (SYN=0x02, ACK=0x10, RST=0x04, FIN=0x01, URG=0x20, PSH=0x08)
    tcp_flags: u8,
    /// DS-1 T5: BGP source and destination AS numbers
    src_as: u32,
    dst_as: u32,
    /// DS-1 T5: ingress/egress SNMP ifIndex (links to Interface.if_index)
    input_snmp: u32,
    /// DS-1 T5: flow direction 0=ingress, 1=egress
    flow_direction: u8,
}

fn decode_record(
    data: &[u8],
    fields: &[FlowField],
    sys_uptime_ms: u64,
    unix_secs: u64,
) -> Option<FlowRecord> {
    let mut src_addr = [0u8; 4];
    let mut dst_addr = [0u8; 4];
    let mut src_port = 0u16;
    let mut dst_port = 0u16;
    let mut protocol = 0u8;
    let mut bytes = 0u64;
    let mut packets = 0u64;
    let mut first_ms = 0u64;
    let mut last_ms = 0u64;
    let mut tcp_flags = 0u8;
    let mut src_as = 0u32;
    let mut dst_as = 0u32;
    let mut input_snmp = 0u32;
    let mut flow_direction = 0u8;

    let mut offset = 0usize;
    for f in fields {
        let flen = f.len as usize;
        if offset + flen > data.len() {
            return None;
        }
        let field_data = &data[offset..offset + flen];
        // Enterprise-specific IEs are NEVER decoded with the standard
        // IANA definition — their bytes are consumed but ignored.
        if f.pen.is_none() {
            match f.ie {
                FIELD_SRC_ADDR if flen == 4 => src_addr.copy_from_slice(field_data),
                FIELD_DST_ADDR if flen == 4 => dst_addr.copy_from_slice(field_data),
                FIELD_SRC_PORT if flen == 2 => {
                    src_port = u16::from_be_bytes([field_data[0], field_data[1]]);
                }
                FIELD_DST_PORT if flen == 2 => {
                    dst_port = u16::from_be_bytes([field_data[0], field_data[1]]);
                }
                FIELD_PROTOCOL if flen == 1 => protocol = field_data[0],
                FIELD_IN_BYTES => bytes = read_variable_u64(field_data),
                FIELD_IN_PKTS => packets = read_variable_u64(field_data),
                FIELD_TCP_FLAGS if flen >= 1 => tcp_flags = field_data[flen - 1],
                FIELD_BGP_SRC_AS => src_as = read_variable_u64(field_data) as u32,
                FIELD_BGP_DST_AS => dst_as = read_variable_u64(field_data) as u32,
                FIELD_INPUT_SNMP => input_snmp = read_variable_u64(field_data) as u32,
                FIELD_OUTPUT_SNMP => {}
                FIELD_DIRECTION if flen >= 1 => flow_direction = field_data[flen - 1],
                FIELD_FIRST_SWITCHED if flen == 4 => {
                    first_ms = u32::from_be_bytes([
                        field_data[0],
                        field_data[1],
                        field_data[2],
                        field_data[3],
                    ]) as u64;
                }
                FIELD_LAST_SWITCHED if flen == 4 => {
                    last_ms = u32::from_be_bytes([
                        field_data[0],
                        field_data[1],
                        field_data[2],
                        field_data[3],
                    ]) as u64;
                }
                _ => {}
            }
        }
        offset += flen;
    }

    let src_address = Ipv4Addr::from(src_addr).to_string();
    let dst_address = Ipv4Addr::from(dst_addr).to_string();

    // Convert uptime-relative timestamps to absolute Unix ns.
    // sys_uptime_ms is the router uptime at export time; unix_secs is wall time at export.
    let (flow_start_ns, flow_end_ns) = if sys_uptime_ms > 0 {
        let unix_ns = unix_secs as i64 * 1_000_000_000;
        let uptime_ns = sys_uptime_ms as i64 * 1_000_000;
        let start_ns = unix_ns - uptime_ns + first_ms as i64 * 1_000_000;
        let end_ns = unix_ns - uptime_ns + last_ms as i64 * 1_000_000;
        (start_ns, end_ns)
    } else {
        // IPFIX absolute mode
        let end_ns = unix_secs as i64 * 1_000_000_000;
        (end_ns, end_ns)
    };

    let _proto_name = protocol_name(protocol);

    Some(FlowRecord {
        src_address,
        dst_address,
        src_port,
        dst_port,
        protocol,
        bytes,
        packets,
        flow_start_ns,
        flow_end_ns,
        tcp_flags,
        src_as,
        dst_as,
        input_snmp,
        flow_direction,
    })
}

fn publish_flow(
    flow: FlowRecord,
    bus: &Arc<InProcessBus>,
    src: &FlowSourceCtx,
    resolution: &IdentityResolution,
) {
    let duration_secs = {
        let dur = flow.flow_end_ns - flow.flow_start_ns;
        if dur <= 0 {
            1.0
        } else {
            dur as f64 / 1_000_000_000.0
        }
    };
    let bytes_per_sec = flow.bytes as f64 / duration_secs;
    let packets_per_sec = flow.packets as f64 / duration_secs;
    let proto_str = protocol_name(flow.protocol);

    let tcp_flags_pattern = classify_tcp_flags(flow.tcp_flags, flow.protocol);
    let amplification_vector = classify_amplification_vector(flow.protocol, flow.dst_port);

    // Identity fields: everything below `identity_status` is evidence or
    // resolver output — `device_id`/`canonical_exporter_id` stay null for
    // plain UDP and are only populated by a verified-ingress resolution.
    let identity_status =
        serde_json::to_value(resolution.status).unwrap_or(serde_json::Value::Null);
    let identity_resolution = serde_json::to_value(resolution).unwrap_or(serde_json::Value::Null);

    let value = serde_json::json!({
        "exporter_address": src.exporter_ip,
        "exporter_id": src.source_key,
        "exporter_endpoint": src.exporter_endpoint,
        "transport_source": src.exporter_endpoint,
        "collector_id": src.collector_id,
        "protocol_version": src.proto.version(),
        "observation_domain_id": src.domain_id,
        "observation_domain_kind": src.proto.domain_kind(),
        "session_epoch": src.generation,
        "identity_status": identity_status,
        "device_id": resolution.device_id,
        "canonical_exporter_id": resolution.canonical_exporter_id,
        "identity_resolution": identity_resolution,
        "src_address": flow.src_address,
        "dst_address": flow.dst_address,
        "src_port": flow.src_port,
        "dst_port": flow.dst_port,
        "protocol": proto_str,
        "bytes": flow.bytes,
        "packets": flow.packets,
        "bytes_per_sec": bytes_per_sec,
        "packets_per_sec": packets_per_sec,
        "flow_start_ns": flow.flow_start_ns,
        "flow_end_ns": flow.flow_end_ns,
        "tcp_flags": flow.tcp_flags,
        "tcp_flags_pattern": tcp_flags_pattern,
        "src_as": flow.src_as,
        "dst_as": flow.dst_as,
        "input_snmp": flow.input_snmp,
        "flow_direction": flow.flow_direction,
        "amplification_vector": amplification_vector,
    });

    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    bus.publish(TelemetryUpdate {
        target: src.source_key.clone(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: now_ns,
        path: "streaming/netflow/flow".to_string(),
        value,
    });
}

/// DS-1 T5: Classify TCP flags byte into a named pattern for DDoS detection.
/// Returns empty string for non-TCP protocols.
fn classify_tcp_flags(flags: u8, protocol: u8) -> &'static str {
    if protocol != 6 {
        return "";
    }
    match flags & 0x3F {
        0x02 => "SYN_ONLY",
        0x12 => "SYN_ACK",
        f if f & 0x04 != 0 && f & 0x02 == 0 => "RST_FLOOD",
        0x10 => "ACK_FLOOD",
        0x01 => "FIN_FLOOD",
        f if f & 0x02 != 0 => "SYN_MIX",
        _ => "ESTABLISHED",
    }
}

/// DS-1 T5/T6: Classify a UDP destination port as a known amplification vector.
/// Returns empty string when not a known amplification port.
pub fn classify_amplification_vector(protocol: u8, dst_port: u16) -> &'static str {
    if protocol != 17 {
        return "";
    }
    match dst_port {
        53 => "dns",
        123 => "ntp",
        1900 => "ssdp",
        11211 => "memcached",
        389 => "ldap",
        5353 => "mdns",
        19 => "chargen",
        17 => "qotd",
        520 | 521 => "rip",
        _ => "",
    }
}

fn read_variable_u64(data: &[u8]) -> u64 {
    match data.len() {
        1 => data[0] as u64,
        2 => u16::from_be_bytes([data[0], data[1]]) as u64,
        4 => u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64,
        8 => u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]),
        _ => 0,
    }
}

fn protocol_name(proto: u8) -> &'static str {
    match proto {
        1 => "icmp",
        6 => "tcp",
        17 => "udp",
        47 => "gre",
        50 => "esp",
        89 => "ospf",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_identity::{BindingProvenance, BindingStatus, IngressAuthMethod};

    fn flow_observed() -> TelemetrySourceIdentity {
        TelemetrySourceIdentity::observed("10.0.0.1:49152")
            .with_protocol("ipfix")
            .with_exporter("flowsrc/site-a/ipfix/10.0.0.1:49152/odid7")
            .with_domain(7)
            .with_collector("site-a")
    }

    fn credential_binding(fp: &str) -> TrustedBinding {
        TrustedBinding {
            binding_id: "b-cred".into(),
            scope: "default".into(),
            protocol: "ipfix".into(),
            exporter_id: "exp-a".into(),
            device_id: "dev-a".into(),
            match_transport: None,
            match_reported_identity: None,
            match_exporter_claim: None,
            match_observation_domain: Some(7),
            match_ingress_credential: Some(fp.to_string()),
            provenance: BindingProvenance::OperatorProvisioned,
            status: BindingStatus::Active,
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: "test".into(),
            updated_by: "test".into(),
        }
    }

    #[test]
    fn udp_path_stays_provisional_even_with_matching_binding() {
        // A non-credential binding that matches every observable field
        // must still NOT resolve: plain UDP has no verified ingress.
        let observed = flow_observed();
        let mut b = credential_binding("sha256:deadbeef");
        b.match_ingress_credential = None;
        b.match_exporter_claim = observed.exporter_id.clone();
        b.match_transport = Some(observed.transport_source.clone());
        b.match_observation_domain = observed.observation_domain_id;

        let res = resolve_flow_identity(&observed, &[b], None);
        assert_eq!(res.status, IdentityStatus::Provisional);
        assert!(res.device_id.is_none());
        assert!(res.canonical_exporter_id.is_none());
    }

    #[test]
    fn verified_ingress_resolves_only_with_attested_principal() {
        let observed = flow_observed();
        let fp = "sha256:cafe";
        let bindings = vec![credential_binding(fp)];

        // Attested principal (as produced by a transport handshake)
        // resolves canonically.
        let principal = VerifiedIngressPrincipal::from_transport_attestation(
            IngressAuthMethod::MutualTls,
            fp.to_string(),
        );
        let res = resolve_flow_identity(&observed, &bindings, Some(&principal));
        assert_eq!(res.status, IdentityStatus::Resolved);
        assert_eq!(res.device_id.as_deref(), Some("dev-a"));
        assert_eq!(res.canonical_exporter_id.as_deref(), Some("exp-a"));

        // A deserialized (unattested) principal — e.g. replayed from a
        // journal — must fail closed.
        let json = serde_json::to_string(&principal).unwrap();
        let restored: VerifiedIngressPrincipal = serde_json::from_str(&json).unwrap();
        assert!(!restored.is_transport_attested());
        let res2 = resolve_flow_identity(&observed, &bindings, Some(&restored));
        assert_ne!(res2.status, IdentityStatus::Resolved);
        assert!(res2.device_id.is_none());
    }

    #[test]
    fn provisional_source_key_is_unambiguous() {
        let peer: SocketAddr = "10.0.0.1:49152".parse().unwrap();
        let a = provisional_source_key("site-a", FlowProtocol::NetflowV9, peer, 7);
        let b = provisional_source_key("site-a", FlowProtocol::NetflowV9, peer, 8);
        let c = provisional_source_key("site-b", FlowProtocol::NetflowV9, peer, 7);
        let d = provisional_source_key(
            "site-a",
            FlowProtocol::NetflowV9,
            "10.0.0.1:49153".parse().unwrap(),
            7,
        );
        let e = provisional_source_key("site-a", FlowProtocol::Ipfix, peer, 7);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
        // Collector ids containing delimiters cannot collide.
        let f = provisional_source_key("a/b", FlowProtocol::NetflowV9, peer, 7);
        let g = provisional_source_key("a%2Fb", FlowProtocol::NetflowV9, peer, 7);
        assert_ne!(f, g);
        assert!(f.contains("a%2Fb"));
    }
}

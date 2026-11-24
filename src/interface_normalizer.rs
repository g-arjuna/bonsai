//! Native interface-state normalizer — first genuine BN-024 native
//! intelligence feature (BN-024-04/G1).
//!
//! Consumes a genuine, versioned device observation — the Cumulus Linux /
//! SONiC FRR path `vtysh -c "show int vrf all"` plus `ip -d address show`
//! (the exact command pair SuzieQ `interfaces.yml` declares for
//! `cumulus`/`sonic`, `version: all`) — extracts per-interface records
//! with SuzieQ `service.py::merge_results` first-wins/`_2nd` conflict
//! semantics, applies the `_entry_cumulus_sonic_cleaner` /
//! `_clean_sonic_data` normalization contract, and emits typed
//! `interface.state` [`TypedFact`]s on the BN-024-02 envelope.
//!
//! Source-informed by pinned upstream SuzieQ @ `75198b5…` (crosswalk
//! `suzieq.services.interfaces`, `suzieq.config.definitions`,
//! `suzieq.services.service`). License class: study_and_reimplement —
//! this is an independent Rust implementation; no upstream code is
//! copied. Documented deviations from upstream behavior:
//!
//! - Identity is namespace-aware from day one (`subject.network_namespace`);
//!   upstream collapses SONiC host/ASIC namespaces (crosswalk
//!   `known_limitations`).
//! - `speed` carries an explicit [`SpeedUnit`]; the FRR `show int`
//!   speed field's scale is not established for this format, so the
//!   raw magnitude is preserved with `unit: unknown` rather than
//!   guessed. The `0xFFFFFFFF` FRR "speed unknown" sentinel and `0`
//!   ("no speed") are surfaced as explicit unavailability, not values.
//! - Upstream's `state == "unknown" → "up"` loopback assumption
//!   (`_clean_linux_data`) is NOT ported: unknown stays unknown.
//! - Unknown/unparseable lines are counted in diagnostics, never
//!   silently dropped; an observation that yields zero interface
//!   records is a `PayloadState::Empty` fact, not success.
//!
//! Out of scope for this slice (recorded in coverage notes): VLAN id,
//! VNI, link up/down counters and status-change timestamps are extracted
//! for oracle comparison but have no typed payload field yet; IP
//! addresses ARE typed (`ip_addresses`/`ipv6_addresses`). No graph
//! write, no authority claim — `trust` stays `device_asserted`.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::interface_facts::{
    AdminState, FieldAvailability, InterfaceStatePayload, InterfaceType, ObservedField, OperState,
    SpeedObservation, SpeedUnit,
};
use crate::typed_facts::{
    Completeness, EvidenceRef, FactPayload, FactSource, FactSubject, Lifecycle, PayloadState,
    Quality, TrustClass, TypedFact,
};

/// Which open-network NOS family this observation came from. Both share
/// the FRR `show int` + `ip -d address` command pair upstream; they
/// differ in the sonic-only internal-interface marking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizerNos {
    /// Cumulus Linux (`_clean_cumulus_data`).
    Cumulus,
    /// SONiC (`_clean_sonic_data` = cumulus cleaner + internal marking).
    Sonic,
}

impl NormalizerNos {
    fn label(self) -> &'static str {
        match self {
            NormalizerNos::Cumulus => "cumulus",
            NormalizerNos::Sonic => "sonic",
        }
    }

    /// SONiC-only: interfaces upstream marks `internal` (host-control
    /// planes, not forwardable ports).
    fn internal_ifnames(self) -> &'static [&'static str] {
        match self {
            NormalizerNos::Sonic => &["BFDRX", "BFDTX", "CPU", "bcm0", "dummy"],
            NormalizerNos::Cumulus => &[],
        }
    }
}

/// Identity/provenance axes for the emitted facts. Mirrors
/// `TypedFact::subject`/`source` — the normalizer never invents them.
pub struct ObservationContext<'a> {
    /// Canonical device id (inventory identity, never a raw address).
    pub device_id: &'a str,
    /// Tenant/environment identity scope (`None` = default).
    pub scope: Option<&'a str>,
    /// Network/ASIC namespace this observation covers ("host", "asic-0",
    /// ...). `None` = unspecified — never assumed.
    pub network_namespace: Option<&'a str>,
    /// Collector/enricher instance id.
    pub collector_id: &'a str,
    /// Capture/session identifier for this observation batch.
    pub capture_id: &'a str,
    pub session_epoch: u64,
    /// Per-source sequence within the session epoch.
    pub sequence: u64,
    /// Observation time (RFC-3339) — device/capture time.
    pub observed_at: &'a str,
    /// Device NOS release string when known (e.g. "5.13.0-mlcu").
    /// Recorded as applicability evidence; upstream declares
    /// `version: all` for this feature so any release is in scope —
    /// an unknown release is reported, not hidden.
    pub release_raw: Option<&'a str>,
}

/// Versioned input: the two genuine command outputs plus context.
pub struct InterfaceObservation<'a> {
    pub nos: NormalizerNos,
    /// `vtysh -c "show int vrf all"` output (may be empty → all fields
    /// from the ip-addr source only).
    pub vtysh_output: &'a str,
    /// `ip -d address show` output.
    pub ip_addr_output: &'a str,
    pub context: ObservationContext<'a>,
}

/// Per-field coverage counters for the operator/diagnostic surface.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FieldCoverage {
    pub known: usize,
    pub unknown: usize,
    pub unsupported: usize,
    pub invalid: usize,
}

impl FieldCoverage {
    fn record(&mut self, f: &ObservedField<impl Sized>) {
        match f.state {
            FieldAvailability::Known => self.known += 1,
            FieldAvailability::Unknown => self.unknown += 1,
            FieldAvailability::Unsupported => self.unsupported += 1,
            FieldAvailability::Invalid => self.invalid += 1,
        }
    }
}

/// Honest coverage/diagnostics for one normalization run.
#[derive(Clone, Debug, Serialize)]
pub struct NormalizationReport {
    pub nos: &'static str,
    /// Interfaces seen across merged sources.
    pub interfaces_observed: usize,
    /// `interface.state` facts emitted (one per observed interface).
    pub facts_emitted: usize,
    /// Duplicate ifnames collapsed by merge (kept first, per upstream).
    pub duplicate_ifnames: usize,
    /// Non-empty `_2nd` conflicts recorded during merge.
    pub merge_conflicts: usize,
    /// Lines that matched no grammar rule (bounded sample kept).
    pub unparsed_lines: usize,
    /// Per-field availability tallies across emitted payloads.
    pub field_coverage: BTreeMap<String, FieldCoverage>,
    /// Explicit limitations of this slice (typed-payload gaps, scope).
    pub notes: Vec<String>,
}

pub struct NormalizationOutput {
    pub facts: Vec<TypedFact>,
    pub report: NormalizationReport,
    /// Per-interface raw extraction diagnostics (pre-typing). Used by the
    /// differential oracle and operator diagnostics; never authority.
    pub interfaces: Vec<InterfaceDiagnostic>,
}

/// Raw per-interface extraction state after merge, before typed-field
/// mapping. `*_raw` strings preserve exactly what the sources reported.
#[derive(Clone, Debug, Serialize)]
pub struct InterfaceDiagnostic {
    pub ifname: String,
    /// iproute2 `name@link` raw form when it differed from `ifname`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ifname_raw: Option<String>,
    pub vrf: Option<String>,
    pub ifindex: Option<u64>,
    pub admin_state_raw: Option<String>,
    pub oper_state_raw: Option<String>,
    /// Post-merge type with upstream `type-_2nd` preference applied,
    /// before vocabulary mapping.
    pub type_raw: Option<String>,
    pub mtu_raw: Option<String>,
    pub speed_raw: Option<String>,
    pub mac_raw: Option<String>,
    pub master_raw: Option<String>,
    pub vlan_raw: Option<String>,
    /// Selected IPv4 list (vtysh-first-wins semantics).
    pub ip_addresses: Vec<String>,
    /// Selected IPv6 list (ip-source-preferred, `::1/128` removed).
    pub ipv6_addresses: Vec<String>,
}

// ── Raw extracted records ────────────────────────────────────────────────

/// One `vtysh show int` interface block (pre-merge).
#[derive(Default)]
struct VtyshRecord {
    name: String,
    admin_state: Option<String>,
    oper_state: Option<String>,
    link_up_count: Option<u64>,
    link_up_ts: Option<String>,
    link_down_count: Option<u64>,
    link_down_ts: Option<String>,
    vrf: Option<String>,
    ifindex: Option<u64>,
    mtu: Option<u64>,
    speed: Option<u64>,
    macaddr: Option<String>,
    ip_addrs: Vec<String>,
    ip6_addrs: Vec<String>,
    vlan: Option<u64>,
    vni: Option<u64>,
    src_vtep: Option<String>,
    master: Option<String>,
    description: Option<String>,
    if_type: Option<String>,
}

/// One `ip -d address show` interface stanza.
#[derive(Default)]
struct IpAddrRecord {
    name: String,
    /// Full `name@link` form as printed by iproute2 (diagnostics only).
    name_raw: String,
    ifindex: Option<u64>,
    mtu: Option<u64>,
    flags: String,
    oper_state: Option<String>, // UP | DOWN | UNKNOWN from header
    link_type: Option<String>,  // link/ether -> "ether", ...
    macaddr: Option<String>,
    master: Option<String>,
    ip_addrs: Vec<String>,
    ip6_addrs: Vec<String>,
    vlan: Option<u64>,
    vni: Option<u64>,
    description: Option<String>,
}

/// Merged per-ifname field bag mirroring upstream `merge_results`
/// (first-wins; conflicts land in `<field>-_2nd`). Address lists keep
/// per-source attribution because upstream `merge_results` does NOT
/// concatenate them: the second source's list becomes `ipAddressList-
/// _2nd`/`ip6AddressList-_2nd`, and the cleaner prefers `_2nd` only for
/// IPv6.
#[derive(Default)]
struct MergedRecord {
    fields: BTreeMap<String, String>,
    second: BTreeMap<String, String>,
    ip4_vtysh: Vec<String>,
    ip4_ip: Vec<String>,
    ip6_vtysh: Vec<String>,
    ip6_ip: Vec<String>,
}

impl MergedRecord {
    fn get(&self, k: &str) -> Option<&str> {
        self.fields.get(k).map(String::as_str)
    }
}

// ── Parsers (bounded, purpose-built for the qualified formats) ───────────

const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_INTERFACES: usize = 4096;
const MAX_ADDRS_PER_IF: usize = 64;
const MAX_UNPARSED_SAMPLES: usize = 16;

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Parse `vtysh -c "show int vrf all"` output. Returns records plus a
/// count of unrecognized non-empty lines (bounded reporting only — the
/// parser never fails the whole observation on stray lines).
fn parse_vtysh_show_interfaces(
    text: &str,
    unparsed: &mut Vec<String>,
    unparsed_total: &mut usize,
) -> Vec<VtyshRecord> {
    let mut records: Vec<VtyshRecord> = Vec::new();
    let mut cur: Option<VtyshRecord> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Interface ") {
            if let Some(r) = cur.take() {
                records.push(r);
            }
            let mut r = VtyshRecord::default();
            // "Interface <name> is <admin>[, line protocol is <oper>]"
            if let Some((name, states)) = rest.split_once(" is ") {
                r.name = name.trim().to_string();
                let mut parts = states.split(',');
                if let Some(admin) = parts.next() {
                    r.admin_state = Some(admin.trim().to_string());
                }
                for p in parts {
                    if let Some(oper) = p.trim().strip_prefix("line protocol is ") {
                        r.oper_state = Some(oper.trim().to_string());
                    }
                }
            } else {
                r.name = rest.trim().to_string();
            }
            cur = Some(r);
            continue;
        }
        let Some(r) = cur.as_mut() else {
            *unparsed_total += 1;
            if unparsed.len() < MAX_UNPARSED_SAMPLES {
                unparsed.push(line.to_string());
            }
            continue;
        };
        let t = line.trim_start();
        if let Some(v) = t.strip_prefix("Link ups:") {
            // "Link ups:       1    last: 2018/10/16 17:52:36.06"
            let mut it = v.split_whitespace();
            if let Some(n) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                r.link_up_count = Some(n);
            }
            if let Some(ts) = t.split("last:").nth(1) {
                r.link_up_ts = Some(ts.trim().to_string());
            }
        } else if t.starts_with("Link downs:") {
            let v = &t["Link downs:".len()..];
            let mut it = v.split_whitespace();
            if let Some(n) = it.next().and_then(|s| s.parse::<u64>().ok()) {
                r.link_down_count = Some(n);
            }
            if let Some(ts) = t.split("last:").nth(1) {
                r.link_down_ts = Some(ts.trim().to_string());
            }
        } else if let Some(v) = t.strip_prefix("vrf:") {
            r.vrf = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("Description:") {
            r.description = Some(v.trim().to_string());
        } else if t.starts_with("index ") {
            // "index 20 metric 0 mtu 9000 speed 4294967295"
            let toks: Vec<&str> = t.split_whitespace().collect();
            for w in toks.windows(2) {
                match w[0] {
                    "index" => r.ifindex = w[1].parse().ok(),
                    "mtu" => r.mtu = w[1].parse().ok(),
                    "speed" => r.speed = w[1].parse().ok(),
                    _ => {}
                }
            }
        } else if let Some(v) = t.strip_prefix("HWaddr:") {
            r.macaddr = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("Interface Type") {
            // Upstream `${type}` Value is a fixed lowercase vocabulary
            // and the textfsm regex is case-sensitive: "Interface Type
            // Other/Vxlan/VRF/Ethernet" do NOT match upstream and leave
            // type unset. Mirror that exactly — no case folding, no
            // vocabulary extension.
            let v = v.trim();
            if TFSM_TYPE_VOCAB.contains(&v) {
                r.if_type = Some(v.to_string());
            }
        } else if let Some(v) = t.strip_prefix("inet6 ") {
            let addr = v.split_whitespace().next().unwrap_or("");
            if addr.contains('/') && r.ip6_addrs.len() < MAX_ADDRS_PER_IF {
                r.ip6_addrs.push(addr.to_string());
            }
        } else if let Some(v) = t.strip_prefix("inet ") {
            let addr = v.split_whitespace().next().unwrap_or("");
            if addr.contains('/') && r.ip_addrs.len() < MAX_ADDRS_PER_IF {
                r.ip_addrs.push(addr.to_string());
            }
        } else if let Some(v) = t.strip_prefix("VLAN Id ") {
            r.vlan = v.trim().split_whitespace().next().and_then(|s| s.parse().ok());
        } else if t.starts_with("VxLAN Id ") {
            // "VxLAN Id 100 VTEP IP: 10.0.0.1 Access VLAN Id 100"
            let toks: Vec<&str> = t.split_whitespace().collect();
            for w in toks.windows(2) {
                match w[0] {
                    "Id" if r.vni.is_none() => r.vni = w[1].parse().ok(),
                    "IP:" => r.src_vtep = Some(w[1].to_string()),
                    _ => {}
                }
            }
        } else if let Some(v) = t.strip_prefix("Master ") {
            // "Master (bridge) ifindex 22" or "Master bond0 ifindex 3"
            let name = v.split_whitespace().next().unwrap_or("");
            r.master = Some(name.trim_matches(|c| c == '(' || c == ')').to_string());
        } else if t.starts_with("PTM status:") || t.starts_with("flags:") {
            // Known vtysh fields outside the typed-payload contract.
        } else {
            *unparsed_total += 1;
            if unparsed.len() < MAX_UNPARSED_SAMPLES {
                unparsed.push(line.to_string());
            }
        }
    }
    if let Some(r) = cur.take() {
        records.push(r);
    }
    records
}

/// Parse `ip -d address show` output into per-interface stanzas.
fn parse_ip_addr_show(
    text: &str,
    unparsed: &mut Vec<String>,
    unparsed_total: &mut usize,
) -> Vec<IpAddrRecord> {
    let mut records: Vec<IpAddrRecord> = Vec::new();
    let mut cur: Option<IpAddrRecord> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        // Header: "<ifindex>: <name>: <<FLAGS,...> mtu <n> ... state <S> ..."
        if !line.starts_with(char::is_whitespace) {
            if let Some((idx_part, rest)) = line.split_once(':')
                && let Ok(ifindex) = idx_part.trim().parse::<u64>()
                && let Some((name, tail)) = rest.split_once(':')
            {
                if let Some(r) = cur.take() {
                    records.push(r);
                }
                // Upstream ifname regex `([^:@ ]+)` stops at '@': the
                // iproute2 `name@link` attachment suffix is NOT part of
                // interface identity upstream — the stanza merges into
                // the base name. Preserve the raw form for diagnostics.
                let raw_name = name.trim();
                let entity = raw_name.split('@').next().unwrap_or(raw_name);
                let mut r = IpAddrRecord {
                    name: entity.to_string(),
                    name_raw: raw_name.to_string(),
                    ifindex: Some(ifindex),
                    ..Default::default()
                };
                // flags <...>
                if let (Some(a), Some(b)) = (tail.find('<'), tail.find('>'))
                    && a < b
                {
                    r.flags = tail[a + 1..b].to_string();
                }
                // mtu N
                if let Some(pos) = tail.find(" mtu ") {
                    let after = &tail[pos + 5..];
                    r.mtu = after
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok());
                }
                // "master <name>" before "state"
                if let Some(mpos) = tail.find(" master ") {
                    let after = &tail[mpos + 8..];
                    let word = after.split_whitespace().next().unwrap_or("");
                    if !word.is_empty() && word != "state" {
                        r.master = Some(word.to_string());
                    }
                }
                // "state <WORD>"
                if let Some(spos) = tail.rfind(" state ") {
                    let word = tail[spos + 7..].split_whitespace().next().unwrap_or("");
                    r.oper_state = Some(word.to_string());
                }
                cur = Some(r);
                continue;
            }
        }
        let Some(r) = cur.as_mut() else {
            continue;
        };
        let t = line.trim();
        if let Some(v) = t.strip_prefix("link/") {
            // "link/ether aa:bb:.. brd .. promiscuity 0"
            let mut it = v.split_whitespace();
            r.link_type = it.next().map(|s| s.to_string());
            r.macaddr = it.next().map(|s| s.to_string());
        } else if let Some(v) = t.strip_prefix("inet6 ") {
            // Upstream template: the generic `inet6` rule precedes the
            // `scope global` rule, so EVERY inet6 line is captured
            // (link-local included); scope is not filtered here.
            if let Some(addr) = v.split_whitespace().next()
                && addr.contains('/')
                && r.ip6_addrs.len() < MAX_ADDRS_PER_IF
            {
                r.ip6_addrs.push(addr.to_string());
            }
        } else if let Some(v) = t.strip_prefix("inet ") {
            if let Some(addr) = v.split_whitespace().next()
                && addr.contains('/')
                && r.ip_addrs.len() < MAX_ADDRS_PER_IF
            {
                r.ip_addrs.push(addr.to_string());
            }
        } else if t.starts_with("vlan protocol") {
            // Upstream `^\s*${type}\s*protocol.*id\s+${vlan}` captures
            // BOTH type=vlan and the vlan id.
            r.link_type = Some("vlan".to_string());
            if let Some(pos) = t.rfind(" id ") {
                r.vlan = t[pos + 4..]
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok());
            }
        } else if let Some(tok) = ip_linkinfo_type(t) {
            // Upstream `${type} id|table|mode|protocol|state|
            // forward_delay` rules: the leading token IS the interface
            // type, and repeated captures overwrite (last wins) — e.g.
            // "bond_slave state ..." yields type=bond_slave even though
            // link/ said ether.
            r.link_type = Some(tok.to_string());
            if tok == "vxlan"
                && let Some(pos) = t.find(" id ")
                && r.vni.is_none()
            {
                r.vni = t[pos + 4..]
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok());
            }
        } else if t.starts_with("bond ")
            || t.starts_with("tun")
            || t.starts_with("valid_lft")
        {
            // Known attribute-blob continuations of the qualified
            // `ip -d address show` grammar; they carry no typed-payload
            // field and are not parse anomalies.
        } else if let Some(v) = t.strip_prefix("alias ") {
            // Upstream `^\s*alias\s*${description}` — iproute2 alias is
            // the description field for this source.
            r.description = Some(v.trim().to_string());
        } else {
            *unparsed_total += 1;
            if unparsed.len() < MAX_UNPARSED_SAMPLES {
                unparsed.push(line.to_string());
            }
        }
    }
    if let Some(r) = cur.take() {
        records.push(r);
    }
    records
}

/// iproute2 `-d` linkinfo type vocabulary — identical to the upstream
/// textfsm `type` Value. Returns the type token when the line matches
/// one of the upstream `${type} <keyword>` grammar shapes.
/// The upstream textfsm `type` Value vocabulary — shared by the vtysh
/// `Interface Type` rule and the iproute2 linkinfo `${type}` rules.
/// Case-sensitive, exactly as the pinned template declares it.
const TFSM_TYPE_VOCAB: &[&str] = &[
    "loopback", "ether", "vlan", "vxlan", "vrf", "bond", "bridge",
    "bridge_slave", "bond_slave", "macvlan",
];

fn ip_linkinfo_type(t: &str) -> Option<&'static str> {
    let (tok, rest) = t.split_once(char::is_whitespace)?;
    if !TFSM_TYPE_VOCAB.contains(&tok) {
        return None;
    }
    let rest = rest.trim_start();
    // Upstream keyword shapes following ${type}: "id", "table", "mode",
    // "protocol", "state ... mii_status", "forward_delay".
    let matched = rest.starts_with("id ")
        || rest.starts_with("table")
        || rest.starts_with("mode")
        || rest.starts_with("protocol")
        || (rest.starts_with("state") && rest.contains("mii_status"))
        || rest.starts_with("forward_delay");
    matched.then_some(match tok {
        "loopback" => "loopback",
        "ether" => "ether",
        "vlan" => "vlan",
        "vxlan" => "vxlan",
        "vrf" => "vrf",
        "bond" => "bond",
        "bridge" => "bridge",
        "bridge_slave" => "bridge_slave",
        "bond_slave" => "bond_slave",
        "macvlan" => "macvlan",
        _ => unreachable!(),
    })
}

// ── Merge (upstream merge_results semantics) ─────────────────────────────

fn merge_records(
    vtysh: Vec<VtyshRecord>,
    ipaddr: Vec<IpAddrRecord>,
) -> (BTreeMap<String, MergedRecord>, usize, usize) {
    let mut by_name: BTreeMap<String, MergedRecord> = BTreeMap::new();
    let mut duplicates = 0usize;
    let mut conflicts = 0usize;

    fn put(rec: &mut MergedRecord, k: &str, v: String, conflicts: &mut usize) {
        if v.is_empty() {
            return;
        }
        match rec.fields.get(k) {
            None => {
                rec.fields.insert(k.to_string(), v);
            }
            Some(existing) if *existing != v => {
                rec.second.insert(format!("{k}-_2nd"), v);
                *conflicts += 1;
            }
            _ => {}
        }
    }

    // Command order matters: vtysh first, ip -d addr second — identical
    // to upstream `command:` list order in interfaces.yml. Records merge
    // across sources by ifname exactly like upstream `merge_results`:
    // first-wins for scalar fields, conflicts recorded as `<f>-_2nd`.
    // `seen` is therefore NOT a cross-source skip flag; duplicate
    // detection is per-source only.
    let mut vtysh_names = std::collections::HashSet::new();
    for r in vtysh {
        if r.name.is_empty() || by_name.len() >= MAX_INTERFACES {
            continue;
        }
        if !vtysh_names.insert(r.name.clone()) {
            duplicates += 1;
        }
        let e = by_name.entry(r.name.clone()).or_default();
        if let Some(v) = r.admin_state {
            put(e, "adminState", v, &mut conflicts);
        }
        if let Some(v) = r.oper_state {
            put(e, "state", v, &mut conflicts);
        }
        if let Some(v) = r.link_up_count {
            put(e, "linkUpCnt", v.to_string(), &mut conflicts);
        }
        if let Some(v) = &r.link_up_ts {
            put(e, "linkUpTimestamp", v.clone(), &mut conflicts);
        }
        if let Some(v) = r.link_down_count {
            put(e, "linkDownCnt", v.to_string(), &mut conflicts);
        }
        if let Some(v) = &r.link_down_ts {
            put(e, "linkDownTimestamp", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.vrf {
            put(e, "vrf", v.clone(), &mut conflicts);
        }
        if let Some(v) = r.ifindex {
            put(e, "ifindex", v.to_string(), &mut conflicts);
        }
        if let Some(v) = r.mtu {
            put(e, "mtu", v.to_string(), &mut conflicts);
        }
        if let Some(v) = r.speed {
            put(e, "speed", v.to_string(), &mut conflicts);
        }
        if let Some(v) = &r.macaddr {
            put(e, "macaddr", v.clone(), &mut conflicts);
        }
        for a in r.ip_addrs {
            if e.ip4_vtysh.len() < MAX_ADDRS_PER_IF {
                e.ip4_vtysh.push(a);
            }
        }
        for a in r.ip6_addrs {
            if e.ip6_vtysh.len() < MAX_ADDRS_PER_IF {
                e.ip6_vtysh.push(a);
            }
        }
        if let Some(v) = r.vlan {
            put(e, "vlan", v.to_string(), &mut conflicts);
        }
        if let Some(v) = r.vni {
            put(e, "vni", v.to_string(), &mut conflicts);
        }
        if let Some(v) = &r.src_vtep {
            put(e, "srcVtepIp", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.master {
            put(e, "master", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.description {
            put(e, "description", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.if_type {
            put(e, "type", v.clone(), &mut conflicts);
        }
    }

    let mut ip_names = std::collections::HashSet::new();
    for r in ipaddr {
        if r.name.is_empty() || by_name.len() >= MAX_INTERFACES {
            continue;
        }
        if !ip_names.insert(r.name.clone()) {
            duplicates += 1;
        }
        let e = by_name.entry(r.name.clone()).or_default();
        if r.name_raw != r.name {
            put(e, "_ifname_raw", r.name_raw.clone(), &mut conflicts);
        }
        if let Some(v) = r.ifindex {
            put(e, "ifindex", v.to_string(), &mut conflicts);
        }
        if let Some(v) = r.mtu {
            put(e, "mtu", v.to_string(), &mut conflicts);
        }
        if !r.flags.is_empty() {
            put(e, "_flags", r.flags.clone(), &mut conflicts);
        }
        if let Some(v) = &r.oper_state {
            put(e, "ipState", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.link_type {
            put(e, "type", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.macaddr {
            put(e, "macaddr", v.clone(), &mut conflicts);
        }
        if let Some(v) = &r.master {
            put(e, "master", v.clone(), &mut conflicts);
        }
        for a in r.ip_addrs {
            if e.ip4_ip.len() < MAX_ADDRS_PER_IF {
                e.ip4_ip.push(a);
            }
        }
        for a in r.ip6_addrs {
            if e.ip6_ip.len() < MAX_ADDRS_PER_IF {
                e.ip6_ip.push(a);
            }
        }
        if let Some(v) = r.vlan {
            put(e, "vlan", v.to_string(), &mut conflicts);
        }
        if let Some(v) = r.vni {
            put(e, "vni", v.to_string(), &mut conflicts);
        }
        if let Some(v) = &r.description {
            put(e, "description", v.clone(), &mut conflicts);
        }
    }
    (by_name, duplicates, conflicts)
}

// ── Normalization (upstream cleaner contract, typed output) ──────────────

fn unavailable<T>(
    state: FieldAvailability,
    detail: Option<impl AsRef<str>>,
) -> ObservedField<T> {
    ObservedField::unavailable(
        state,
        detail.map(|d| d.as_ref().chars().take(200).collect::<String>()),
    )
}

fn admin_state_of(raw: Option<&str>) -> ObservedField<AdminState> {
    match raw.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("up") => ObservedField::known(AdminState::Up),
        Some("down") => ObservedField::known(AdminState::Down),
        Some(other) => unavailable(
            FieldAvailability::Invalid,
            Some(format!("unrecognized adminState '{other}'")),
        ),
        None => unavailable(FieldAvailability::Unknown, Some("adminState not observed")),
    }
}

fn oper_state_of(
    merged_state: Option<&str>,
    admin: &ObservedField<AdminState>,
) -> ObservedField<OperState> {
    // Upstream: adminState down forces oper state down.
    if matches!(admin.state, FieldAvailability::Known)
        && admin.value == Some(AdminState::Down)
    {
        return ObservedField::known(OperState::Down);
    }
    match merged_state.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("up") => ObservedField::known(OperState::Up),
        Some("down") => ObservedField::known(OperState::Down),
        Some(other) => unavailable(
            FieldAvailability::Invalid,
            Some(format!("unrecognized line-protocol state '{other}'")),
        ),
        None => unavailable(FieldAvailability::Unknown, Some("oper state not observed")),
    }
}

/// Upstream type vocabulary → typed classification. Unrecognized types
/// map to `Unknown`, never guessed (upstream would emit the raw string).
fn interface_type_of(raw: Option<&str>, nos: NormalizerNos, ifname: &str) -> ObservedField<InterfaceType> {
    // SONiC internal marking runs on the merged type-independent name list.
    let _ = nos;
    let Some(raw) = raw else {
        return unavailable(FieldAvailability::Unknown, Some("interface type not observed"));
    };
    let t = raw.to_ascii_lowercase();
    let mapped = match t.as_str() {
        "ether" | "ethernet" => Some(InterfaceType::Physical),
        "loopback" => Some(InterfaceType::Loopback),
        "vlan" => Some(InterfaceType::Virtual),
        "vxlan" => Some(InterfaceType::Tunnel),
        "bond" | "bond_slave" | "portchannel" | "port_channel" | "lag" => {
            Some(InterfaceType::PortChannel)
        }
        "bridge" | "bridge_slave" | "macvlan" | "vrf" | "internal" | "dummy" => {
            Some(InterfaceType::Virtual)
        }
        _ => None,
    };
    match mapped {
        Some(v) => ObservedField::known(v),
        None => unavailable(
            FieldAvailability::Unknown,
            Some(format!("unmapped interface type '{raw}' for '{ifname}'")),
        ),
    }
}

fn mtu_of(raw: Option<&str>) -> ObservedField<u32> {
    match raw.and_then(|s| s.trim().parse::<u64>().ok()) {
        Some(0) | None => unavailable(
            FieldAvailability::Unknown,
            Some("mtu absent or zero"),
        ),
        Some(v) if v <= u16::MAX as u64 => ObservedField::known(v as u32),
        Some(v) => unavailable(
            FieldAvailability::Invalid,
            Some(format!("mtu {v} out of range")),
        ),
    }
}

/// FRR `show int` speed field: the `0xFFFFFFFF` sentinel means "speed
/// unknown" upstream; `0` means "no speed" (NO_SPEED for vxlan etc.).
/// Any other bare integer is preserved with `unit: unknown` — the scale
/// of this field is not established for the qualified format.
fn speed_of(raw: Option<&str>) -> ObservedField<SpeedObservation> {
    const FRR_SPEED_UNKNOWN: u64 = u32::MAX as u64;
    match raw.and_then(|s| s.trim().parse::<u64>().ok()) {
        None => unavailable(FieldAvailability::Unknown, Some("speed not observed")),
        Some(FRR_SPEED_UNKNOWN) => unavailable(
            FieldAvailability::Unknown,
            Some("device reported speed-unknown sentinel (0xFFFFFFFF)"),
        ),
        Some(0) => unavailable(FieldAvailability::Unknown, Some("device reported no speed")),
        Some(v) => ObservedField::known(SpeedObservation {
            value: v,
            unit: SpeedUnit::Unknown,
        }),
    }
}

fn mac_of(raw: Option<&str>) -> ObservedField<String> {
    match raw {
        None => unavailable(FieldAvailability::Unknown, Some("macaddr not observed")),
        Some(v) => match crate::interface_facts::normalize_mac(v) {
            Some(n) => ObservedField::known(n),
            None => unavailable(
                FieldAvailability::Invalid,
                Some("macaddr failed validation"),
            ),
        },
    }
}

fn sanitize_bounded(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// Normalize merged records into typed `interface.state` facts.
pub fn normalize(obs: &InterfaceObservation) -> NormalizationOutput {
    if obs.vtysh_output.len() + obs.ip_addr_output.len() > MAX_INPUT_BYTES {
        // Input bound is enforced by callers via capture limits; defend
        // in depth here by refusing rather than truncating.
        return NormalizationOutput {
            facts: vec![],
            interfaces: vec![],
            report: NormalizationReport {
                nos: obs.nos.label(),
                interfaces_observed: 0,
                facts_emitted: 0,
                duplicate_ifnames: 0,
                merge_conflicts: 0,
                unparsed_lines: 0,
                field_coverage: BTreeMap::new(),
                notes: vec!["observation exceeded input byte bound; refused".into()],
            },
        };
    }

    let mut unparsed: Vec<String> = Vec::new();
    let mut unparsed_total = 0usize;
    let vtysh = parse_vtysh_show_interfaces(obs.vtysh_output, &mut unparsed, &mut unparsed_total);
    let ipaddr = parse_ip_addr_show(obs.ip_addr_output, &mut unparsed, &mut unparsed_total);
    let (merged, duplicates, conflicts) = merge_records(vtysh, ipaddr);

    let mut facts = Vec::new();
    let mut interfaces: Vec<InterfaceDiagnostic> = Vec::new();
    let mut coverage: BTreeMap<String, FieldCoverage> = BTreeMap::new();
    let unparsed_count = unparsed_total;

    let internal = obs.nos.internal_ifnames();
    for (name, m) in &merged {
        if facts.len() >= MAX_INTERFACES {
            break;
        }
        // SONiC internal marking (upstream _clean_sonic_data).
        // Upstream cleaner prefers `type-_2nd` (the ip-addr link type)
        // whenever the merge recorded one; mirror that preference.
        let mut if_type_raw = m
            .second
            .get("type-_2nd")
            .map(String::as_str)
            .or_else(|| m.get("type"))
            .map(|s| s.to_string());
        if obs.nos == NormalizerNos::Sonic
            && internal.iter().any(|i| i.eq_ignore_ascii_case(name))
        {
            if_type_raw = Some("internal".to_string());
        }

        let admin = admin_state_of(m.get("adminState"));
        let oper = oper_state_of(m.get("state"), &admin);
        let itype = interface_type_of(if_type_raw.as_deref(), obs.nos, name);
        let mtu = mtu_of(m.get("mtu"));
        // Upstream cleaner: vxlan interfaces carry NO_SPEED regardless of
        // any reported value.
        let speed = if if_type_raw
            .as_deref()
            .map(|t| t.eq_ignore_ascii_case("vxlan"))
            .unwrap_or(false)
        {
            unavailable::<SpeedObservation>(
                FieldAvailability::Unknown,
                Some("vxlan interfaces report no speed (upstream NO_SPEED rule)"),
            )
        } else {
            speed_of(m.get("speed"))
        };
        // Upstream prefers the ip-addr MAC only when vtysh lacks one —
        // merge already implements first-wins; macaddr-_2nd is the ip
        // value and is intentionally NOT preferred (vtysh HWaddr wins).
        let mac = mac_of(m.get("macaddr"));

        // master: strip parens already done at parse; lowercase per
        // upstream ("Lowercase the master value thanks to SoNIC").
        let member_of = m.get("master").map(|s| {
            let cleaned = s
                .trim_matches(|c| c == '(' || c == ')')
                .to_ascii_lowercase();
            sanitize_bounded(&cleaned, 255)
        });
        let member_of = member_of.filter(|s| !s.is_empty());

        let description = m.get("description").map(|d| sanitize_bounded(d, 240));

        // Address selection mirrors upstream merge+cleaner exactly:
        // - IPv4 `ipAddressList`: first source wins; the ip-addr list only
        //   fills when vtysh reported none (its `_2nd` copy is unused).
        // - IPv6 `ip6AddressList`: the cleaner prefers `ip6AddressList-
        //   _2nd` (the ip-addr list) whenever both sources reported;
        //   otherwise whichever list exists. Upstream then drops "::1/128".
        let ip6_src: &[String] = if !m.ip6_ip.is_empty() {
            &m.ip6_ip
        } else {
            &m.ip6_vtysh
        };
        let ip6_list: Vec<String> = ip6_src
            .iter()
            .filter(|a| a.as_str() != "::1/128")
            .cloned()
            .collect();
        let ip6 = if ip6_list.is_empty() {
            unavailable::<Vec<String>>(FieldAvailability::Unknown, Some("no ipv6 observed"))
        } else {
            ObservedField::known(ip6_list.clone())
        };
        let ip4_src: &[String] = if !m.ip4_vtysh.is_empty() {
            &m.ip4_vtysh
        } else {
            &m.ip4_ip
        };
        let ip4 = if ip4_src.is_empty() {
            unavailable::<Vec<String>>(FieldAvailability::Unknown, Some("no ipv4 observed"))
        } else {
            ObservedField::known(ip4_src.to_vec())
        };

        let payload = InterfaceStatePayload {
            interface_name: name.clone(),
            normalized_name: None,
            administrative_state: admin.clone(),
            operational_state: oper.clone(),
            interface_type: itype.clone(),
            mtu: mtu.clone(),
            speed: speed.clone(),
            mac_address: mac.clone(),
            description,
            parent_interface: None,
            member_of: member_of.clone(),
            ip_addresses: Some(ip4),
            ipv6_addresses: Some(ip6),
        };

        interfaces.push(InterfaceDiagnostic {
            ifname: name.clone(),
            ifname_raw: m.get("_ifname_raw").map(|s| s.to_string()),
            vrf: m.get("vrf").map(|s| s.to_string()),
            ifindex: m.get("ifindex").and_then(|s| s.parse().ok()),
            admin_state_raw: m.get("adminState").map(|s| s.to_string()),
            oper_state_raw: m.get("state").map(|s| s.to_string()),
            type_raw: if_type_raw.clone(),
            mtu_raw: m.get("mtu").map(|s| s.to_string()),
            speed_raw: m.get("speed").map(|s| s.to_string()),
            mac_raw: m.get("macaddr").map(|s| s.to_string()),
            master_raw: m.get("master").map(|s| s.to_string()),
            vlan_raw: m.get("vlan").map(|s| s.to_string()),
            ip_addresses: ip4_src.to_vec(),
            ipv6_addresses: ip6_list.clone(),
        });

        // Record coverage per named field.
        macro_rules! cov {
            ($map:expr, $name:literal, $field:expr) => {{
                let e = $map.entry($name.to_string()).or_default();
                e.record(&$field);
            }};
        }
        cov!(coverage, "administrative_state", admin);
        cov!(coverage, "operational_state", oper);
        cov!(coverage, "interface_type", itype);
        cov!(coverage, "mtu", mtu);
        cov!(coverage, "speed", speed);
        cov!(coverage, "mac_address", mac);
        cov!(coverage, "ip_addresses", payload.ip_addresses.clone().unwrap());
        cov!(coverage, "ipv6_addresses", payload.ipv6_addresses.clone().unwrap());

        // Envelope: subject axes come only from the observation context.
        let subject = FactSubject {
            device_id: obs.context.device_id.to_string(),
            scope: obs.context.scope.map(|s| s.to_string()),
            namespace: "interface".to_string(),
            network_namespace: obs.context.network_namespace.map(|s| s.to_string()),
            entity: Some(name.clone()),
            vrf: m.get("vrf").map(|s| s.to_string()),
            afi_safi: None,
            plane: Some("operational".to_string()),
        };
        let fields = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        let evidence = vec![
            EvidenceRef {
                kind: "raw_output_vtysh".to_string(),
                sha256: sha256_hex(obs.vtysh_output.as_bytes()),
                locator: None,
            },
            EvidenceRef {
                kind: "raw_output_ip_addr".to_string(),
                sha256: sha256_hex(obs.ip_addr_output.as_bytes()),
                locator: None,
            },
        ];
        let mut fact = TypedFact {
            schema_version: crate::typed_facts::SCHEMA_VERSION,
            fact_id: String::new(),
            fact_kind: crate::interface_facts::FACT_KIND_INTERFACE_STATE.to_string(),
            subject,
            source: FactSource {
                collector_id: obs.context.collector_id.to_string(),
                source_kind: "cli_ssh".to_string(),
                capture_id: obs.context.capture_id.to_string(),
                session_epoch: obs.context.session_epoch,
                sequence: obs.context.sequence,
                exporter_id: None,
            },
            observed_at: obs.context.observed_at.to_string(),
            ingested_at: None,
            valid_from: None,
            valid_until: None,
            payload: FactPayload {
                produced_by: format!("native_{}_frr", obs.nos.label()),
                parser_revision: "cl-iface-vtysh-ipaddr-2026.11".to_string(),
                mapping_revision: Some("bn024-iface-map-1".to_string()),
                fields: Some(fields),
                state: None,
            },
            payload_sha256: String::new(),
            evidence_sha256: String::new(),
            quality: Quality {
                completeness: Completeness::Complete,
                missing_fields: vec![],
                notes: vec![],
            },
            trust: TrustClass::DeviceAsserted,
            authority: None,
            lifecycle: Lifecycle::Active,
            withdrawn_reason: None,
            superseded_by: None,
            lifecycle_changed_at: None,
            evidence,
        };
        fact.seal();
        facts.push(fact);
    }

    // Zero interfaces is an explicit empty observation, not success.
    if facts.is_empty() {
        facts.push(empty_observation_fact(obs));
    }

    let mut notes = vec![
        "vlan/vni/link-counters/status-timestamps extracted for oracle comparison but not yet typed".to_string(),
        "speed unit unproven for this format; raw magnitude kept with unit=unknown".to_string(),
    ];
    if obs.context.release_raw.is_none() {
        notes.push("device release unknown; upstream declares version:all so scope is unaffected".into());
    }
    if obs.nos == NormalizerNos::Sonic {
        notes.push("network_namespace must name the observed namespace; ASIC namespaces require a STATE_DB path (not this slice)".into());
    }

    NormalizationOutput {
        interfaces,
        report: NormalizationReport {
            nos: obs.nos.label(),
            interfaces_observed: merged.len(),
            facts_emitted: facts.len(),
            duplicate_ifnames: duplicates,
            merge_conflicts: conflicts,
            unparsed_lines: unparsed_count,
            field_coverage: coverage,
            notes,
        },
        facts,
    }
}

fn empty_observation_fact(obs: &InterfaceObservation) -> TypedFact {
    let mut fact = TypedFact {
        schema_version: crate::typed_facts::SCHEMA_VERSION,
        fact_id: String::new(),
        fact_kind: crate::interface_facts::FACT_KIND_INTERFACE_STATE.to_string(),
        subject: FactSubject {
            device_id: obs.context.device_id.to_string(),
            scope: obs.context.scope.map(|s| s.to_string()),
            namespace: "interface".to_string(),
            network_namespace: obs.context.network_namespace.map(|s| s.to_string()),
            entity: None,
            vrf: None,
            afi_safi: None,
            plane: Some("operational".to_string()),
        },
        source: FactSource {
            collector_id: obs.context.collector_id.to_string(),
            source_kind: "cli_ssh".to_string(),
            capture_id: obs.context.capture_id.to_string(),
            session_epoch: obs.context.session_epoch,
            sequence: obs.context.sequence,
            exporter_id: None,
        },
        observed_at: obs.context.observed_at.to_string(),
        ingested_at: None,
        valid_from: None,
        valid_until: None,
        payload: FactPayload {
            produced_by: format!("native_{}_frr", obs.nos.label()),
            parser_revision: "cl-iface-vtysh-ipaddr-2026.11".to_string(),
            mapping_revision: Some("bn024-iface-map-1".to_string()),
            fields: None,
            state: Some(PayloadState::Empty),
        },
        payload_sha256: String::new(),
        evidence_sha256: String::new(),
        quality: Quality {
            completeness: Completeness::Unknown,
            missing_fields: vec![],
            notes: vec!["no interface records in observation".into()],
        },
        trust: TrustClass::DeviceAsserted,
        authority: None,
        lifecycle: Lifecycle::Active,
        withdrawn_reason: None,
        superseded_by: None,
        lifecycle_changed_at: None,
        evidence: vec![],
    };
    fact.seal();
    fact
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    pub(super) fn fixture(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bn024/interface-cumulus")
            .join(name);
        std::fs::read_to_string(&p).expect("fixture missing")
    }

    pub(super) fn ctx<'a>() -> ObservationContext<'a> {
        ObservationContext {
            device_id: "leaf01",
            scope: None,
            network_namespace: Some("host"),
            collector_id: "collector-a",
            capture_id: "cap-iface-1",
            session_epoch: 1,
            sequence: 7,
            observed_at: "2026-11-24T10:00:00Z",
            release_raw: None,
        }
    }

    pub(super) fn run(nos: NormalizerNos) -> NormalizationOutput {
        normalize(&InterfaceObservation {
            nos,
            vtysh_output: Box::leak(fixture("vtysh-show-int-vrf-all.txt").into_boxed_str()),
            ip_addr_output: Box::leak(fixture("ip-d-address-show.txt").into_boxed_str()),
            context: ctx(),
        })
    }

    fn fact_for<'a>(out: &'a NormalizationOutput, name: &str) -> &'a TypedFact {
        out.facts
            .iter()
            .find(|f| f.subject.entity.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no fact for {name}"))
    }

    fn payload_of(f: &TypedFact) -> InterfaceStatePayload {
        serde_json::from_value(f.payload.fields.clone().unwrap()).unwrap()
    }

    #[test]
    fn fixture_normalizes_and_validates() {
        let out = run(NormalizerNos::Cumulus);
        assert!(out.facts.iter().all(|f| f.validate().is_ok()));
        assert!(out.report.facts_emitted > 20);
        // bond01: admin+oper up, member of bridge, vrf default.
        let p = payload_of(fact_for(&out, "bond01"));
        assert_eq!(p.administrative_state.value, Some(AdminState::Up));
        assert_eq!(p.operational_state.value, Some(OperState::Up));
        assert_eq!(p.member_of.as_deref(), Some("bridge"));
        assert_eq!(p.mtu.value, Some(9000));
        // swp46: admin down forces oper down (upstream cleaner rule).
        let p = payload_of(fact_for(&out, "swp46"));
        assert_eq!(p.administrative_state.value, Some(AdminState::Down));
        assert_eq!(p.operational_state.value, Some(OperState::Down));
        // lo: loopback type, mac 00:00:00:00:00:00 normalized.
        let p = payload_of(fact_for(&out, "lo"));
        assert_eq!(p.interface_type.value, Some(InterfaceType::Loopback));
        assert_eq!(p.mac_address.value.as_deref(), Some("00:00:00:00:00:00"));
    }

    #[test]
    fn speed_sentinel_and_zero_are_unavailable_not_values() {
        let out = run(NormalizerNos::Cumulus);
        // bond01 reports FRR sentinel 4294967295 -> unknown, never a value.
        let p = payload_of(fact_for(&out, "bond01"));
        assert_eq!(p.speed.state, FieldAvailability::Unknown);
        assert!(p.speed.value.is_none());
        // peerlink.4094 reports 2000 -> preserved raw, unit unknown.
        let p = payload_of(fact_for(&out, "peerlink.4094"));
        match p.speed.state {
            FieldAvailability::Known => {
                let v = p.speed.value.unwrap();
                assert_eq!(v.value, 2000);
                assert_eq!(v.unit, SpeedUnit::Unknown);
            }
            s => panic!("expected known speed, got {s:?}"),
        }
    }

    #[test]
    fn sonic_marks_internal_interfaces() {
        let cum = run(NormalizerNos::Cumulus);
        let son = run(NormalizerNos::Sonic);
        // No internal names in this fixture; assert the mechanism via a
        // synthetic observation instead.
        let vtysh = "Interface CPU is up, line protocol is up\n  vrf: default\n  index 1 metric 0 mtu 1500 speed 0 \n";
        let s = normalize(&InterfaceObservation {
            nos: NormalizerNos::Sonic,
            vtysh_output: vtysh,
            ip_addr_output: "",
            context: ctx(),
        });
        let p = payload_of(&s.facts[0]);
        assert_eq!(p.interface_type.value, Some(InterfaceType::Virtual));
        let _ = (cum, son); // fixture has no internal ifnames; mechanism tested above
    }

    #[test]
    fn empty_observation_is_explicit_state_not_success() {
        let out = normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh_output: "",
            ip_addr_output: "",
            context: ctx(),
        });
        assert_eq!(out.facts.len(), 1);
        let f = &out.facts[0];
        assert_eq!(f.payload.state, Some(PayloadState::Empty));
        assert!(f.validate().is_ok());
        assert_eq!(f.quality.completeness, Completeness::Unknown);
    }

    #[test]
    fn malformed_lines_are_counted_not_fatal() {
        let out = normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh_output: "garbage line one\nInterface Eth0 is up\n  nonsense field\n",
            ip_addr_output: "not an ip addr header\n",
            context: ctx(),
        });
        assert!(out.report.unparsed_lines >= 1);
        assert_eq!(out.facts.len(), 1);
    }

    #[test]
    fn fact_identity_binds_namespace_and_entity() {
        let a = run(NormalizerNos::Cumulus);
        let mut b = normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh_output: Box::leak(fixture("vtysh-show-int-vrf-all.txt").into_boxed_str()),
            ip_addr_output: Box::leak(fixture("ip-d-address-show.txt").into_boxed_str()),
            context: ObservationContext {
                network_namespace: Some("asic-0"),
                ..ctx()
            },
        });
        // Same interface name in a different network namespace must not
        // share fact_id.
        let fa = fact_for(&a, "swp1").fact_id.clone();
        let fb = fact_for(&b, "swp1").fact_id.clone();
        assert_ne!(fa, fb);
        b.facts.clear();
    }
}

#[cfg(test)]
mod oracle_emit {
    use super::tests::*;
    use super::*;

    /// Regenerate the Rust-side oracle input: `interfaces` diagnostics +
    /// serialized fact payloads for the Cumulus fixture pair.
    /// Run: BONSAI_ORACLE_OUT=path cargo test -p bonsai --lib \
    ///   interface_normalizer::oracle_emit -- --ignored
    #[test]
    #[ignore = "artifact emitter for the differential oracle"]
    fn emit_cumulus_oracle_input() {
        let out = run(NormalizerNos::Cumulus);
        let doc = serde_json::json!({
            "nos": "cumulus",
            "interfaces": out.interfaces,
            "facts": out.facts.iter().map(|f| serde_json::json!({
                "ifname": f.subject.entity,
                "payload": f.payload.fields,
            })).collect::<Vec<_>>(),
            "report": out.report,
        });
        let path = std::env::var("BONSAI_ORACLE_OUT")
            .unwrap_or_else(|_| "target/bn024-interface-oracle-rust.json".into());
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap())
            .unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        eprintln!("wrote {path}");
    }
}

#[cfg(test)]
mod perf {
    use super::tests::*;
    use super::*;
    use std::time::Instant;

    /// Throughput measurement for the coverage/diagnostics requirement.
    /// Run: cargo test -p bonsai --lib --profile ci -- --ignored \
    ///   interface_normalizer::perf
    #[test]
    #[ignore = "perf measurement, not a correctness gate"]
    fn fixture_normalization_throughput() {
        let vtysh = fixture("vtysh-show-int-vrf-all.txt");
        let ipaddr = fixture("ip-d-address-show.txt");
        let n = 2000;
        let t = Instant::now();
        let mut total_facts = 0usize;
        for _ in 0..n {
            let out = normalize(&InterfaceObservation {
                nos: NormalizerNos::Cumulus,
                vtysh_output: Box::leak(vtysh.clone().into_boxed_str()),
                ip_addr_output: Box::leak(ipaddr.clone().into_boxed_str()),
                context: ctx(),
            });
            total_facts += out.facts.len();
        }
        let el = t.elapsed();
        eprintln!(
            "normalized {n} observations ({} facts total) in {:?} -> {:.1} obs/s, {:.3} ms/obs",
            total_facts,
            el,
            n as f64 / el.as_secs_f64(),
            el.as_secs_f64() * 1000.0 / n as f64,
        );
    }

    #[test]
    fn oversized_observation_is_refused_not_truncated() {
        let big = "x".repeat(MAX_INPUT_BYTES + 1);
        let out = normalize(&InterfaceObservation {
            nos: NormalizerNos::Cumulus,
            vtysh_output: Box::leak(big.into_boxed_str()),
            ip_addr_output: "",
            context: ctx(),
        });
        assert!(out.facts.is_empty());
        assert!(out
            .report
            .notes
            .iter()
            .any(|n| n.contains("exceeded input byte bound")));
    }
}

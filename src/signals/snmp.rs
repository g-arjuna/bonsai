use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use hex;
use serde::Serialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};

use crate::config::{SnmpConfig, TargetConfig};
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

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
}

pub async fn run_snmp_receiver(
    cfg: SnmpConfig,
    targets: Vec<TargetConfig>,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let archive = SnmpArchive::open(&cfg.archive_path).await?;
    let target_map = SnmpTargetMap::new(&targets);

    if cfg.udp_addr.trim().is_empty() {
        warn!("snmp receiver enabled but udp_addr empty");
        return Ok(());
    }

    let socket = UdpSocket::bind(&cfg.udp_addr)
        .await
        .with_context(|| format!("bind snmp UDP listener at {}", cfg.udp_addr))?;
    info!(addr = %cfg.udp_addr, "snmp UDP listener started");

    let mut buf = vec![0_u8; cfg.max_frame_bytes.max(1)];
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("snmp UDP listener stopping");
                break;
            }
            recv = socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, peer)) => {
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

                        let resolved = target_map.resolve(&peer_ip, &event);
                        let value = serde_json::to_value(&event)
                            .unwrap_or_else(|_| json!({"peer_addr": event.peer_addr.clone()}));
                        bus.publish(TelemetryUpdate {
                            target: resolved.address,
                            vendor: resolved.vendor,
                            hostname: resolved.hostname,
                            role: resolved.role,
                            site: resolved.site,
                            timestamp_ns: event.timestamp_ns,
                            path: format!("signals/snmp/{}", event.event_type),
                            value,
                        });
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

#[derive(Clone)]
struct ResolvedTarget {
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
        file.write_all(&line).await.context("write snmp archive line")?;
        Ok(())
    }
}

impl SnmpTargetMap {
    fn new(targets: &[TargetConfig]) -> Self {
        let entries = targets
            .iter()
            .map(|target| SnmpTargetEntry {
                address: target.address.split(':').next().unwrap_or(&target.address).to_string(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                site: target.site.clone().unwrap_or_default(),
            })
            .collect();
        Self { entries }
    }

    fn resolve(&self, peer_ip: &str, _event: &SnmpTrapEvent) -> ResolvedTarget {
        if let Some(entry) = self.entries.iter().find(|entry| entry.address == peer_ip) {
            return ResolvedTarget {
                address: entry.address.clone(),
                hostname: if entry.hostname.is_empty() {
                    peer_ip.to_string()
                } else {
                    entry.hostname.clone()
                },
                vendor: entry.vendor.clone(),
                role: entry.role.clone(),
                site: entry.site.clone(),
            };
        }
        ResolvedTarget {
            address: peer_ip.to_string(),
            hostname: peer_ip.to_string(),
            vendor: String::new(),
            role: String::new(),
            site: String::new(),
        }
    }
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

    let (community, pdu_tag, pdu_content) = match version {
        0 | 1 => {
            let community = msg_reader.read_octet_string_text()?;
            let (tag, pdu) = msg_reader.read_tlv()?;
            (community, tag, pdu.to_vec())
        }
        3 => {
            let _header = msg_reader.read_expected(0x30)?;
            let _security = msg_reader.read_expected(0x04)?;
            let (scoped_tag, scoped_content) = msg_reader.read_tlv()?;
            if scoped_tag != 0x30 {
                bail!("encrypted SNMPv3 scoped PDU is not supported");
            }
            let mut scoped = BerReader::new(scoped_content);
            let _context_engine_id = scoped.read_octet_string()?;
            let _context_name = scoped.read_octet_string()?;
            let (tag, pdu) = scoped.read_tlv()?;
            (String::new(), tag, pdu.to_vec())
        }
        _ => bail!("unsupported snmp version {version}"),
    };

    let mut event = if version == 0 && pdu_tag == 0xA4 {
        parse_v1_trap(&pdu_content, peer_addr, timestamp_ns, &version_name, community)?
    } else {
        parse_v2_trap(&pdu_content, peer_addr, timestamp_ns, &version_name, community, pdu_tag)?
    };
    event.raw_hex = hex::encode(raw);
    event.raw_len = raw.len();
    Ok(event)
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
    let _agent_addr = reader.read_expected(0x40)?;
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
        &["psu", "power", "temperature", "thermal", "fan", "voltage", "env"],
    ) {
        return (
            "snmp_environmental".to_string(),
            "environmental".to_string(),
            format!("environmental trap received: {message}"),
        );
    }
    if contains_any(
        &text,
        &["fru", "linecard", "line card", "fabric", "module", "chassis"],
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
    (
        "snmp_raw".to_string(),
        "raw".to_string(),
        message,
    )
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
                Ok(format!("{}.{}.{}.{}", value[0], value[1], value[2], value[3]))
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
                    encode_sequence(&[
                        encode_varbind(
                            "1.3.6.1.4.1.9.9.13.3.1.3.1",
                            encode_octet_string(b"PSU failure alarm asserted"),
                        ),
                    ]),
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
            },
        );
        assert_eq!(resolved.address, "192.0.2.44");
        assert_eq!(resolved.hostname, "xrd-pe1");
        assert_eq!(resolved.vendor, "cisco");
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

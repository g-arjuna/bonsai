use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::NetflowConfig;
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

// ── Netflow v9 field type IDs (RFC 3954) ────────────────────────────────────

const FIELD_IN_BYTES: u16 = 1;
const FIELD_IN_PKTS: u16 = 2;
const FIELD_PROTOCOL: u16 = 4;
const FIELD_SRC_PORT: u16 = 7;
const FIELD_SRC_ADDR: u16 = 8;
const FIELD_DST_PORT: u16 = 11;
const FIELD_DST_ADDR: u16 = 12;
const FIELD_FIRST_SWITCHED: u16 = 22;
const FIELD_LAST_SWITCHED: u16 = 21;

// IPFIX (v10) uses the same field IDs for the basic IPv4 5-tuple.

const MAX_UDP_PAYLOAD: usize = 65535;

/// Per-exporter template cache: source_id → (template_id → Vec<(field_type, field_len)>)
type TemplateCache = HashMap<u32, HashMap<u16, Vec<(u16, u16)>>>;

pub async fn run_netflow_receiver(
    cfg: NetflowConfig,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let socket = UdpSocket::bind(&cfg.udp_addr).await?;
    info!(addr = %cfg.udp_addr, "Netflow/IPFIX UDP receiver listening");

    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut templates: HashMap<SocketAddr, TemplateCache> = HashMap::new();

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, peer)) => {
                        let pkt = &buf[..len];
                        if let Err(e) = handle_packet(pkt, peer, &mut templates, &bus) {
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
    templates: &mut HashMap<SocketAddr, TemplateCache>,
    bus: &Arc<InProcessBus>,
) -> Result<()> {
    if pkt.len() < 4 {
        bail!("packet too short");
    }
    let version = u16::from_be_bytes([pkt[0], pkt[1]]);
    let exporter_ip = peer.ip().to_string();
    match version {
        9 => parse_v9(pkt, peer, templates, bus, &exporter_ip),
        10 => parse_ipfix(pkt, peer, templates, bus, &exporter_ip),
        v => bail!("unsupported netflow version {v}"),
    }
}

// ── Netflow v9 ────────────────────────────────────────────────────────────────

fn parse_v9(
    pkt: &[u8],
    peer: SocketAddr,
    templates: &mut HashMap<SocketAddr, TemplateCache>,
    bus: &Arc<InProcessBus>,
    exporter_ip: &str,
) -> Result<()> {
    // v9 header: version(2) count(2) sys_uptime(4) unix_secs(4) seq(4) source_id(4) = 20 bytes
    if pkt.len() < 20 {
        bail!("v9 header too short");
    }
    let sys_uptime_ms = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as u64;
    let unix_secs = u32::from_be_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]) as u64;
    let source_id = u32::from_be_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]);

    let exporter_cache = templates.entry(peer).or_default();
    let source_cache = exporter_cache.entry(source_id).or_default();

    let mut offset = 20usize;
    while offset + 4 <= pkt.len() {
        let flowset_id = u16::from_be_bytes([pkt[offset], pkt[offset + 1]]);
        let length = u16::from_be_bytes([pkt[offset + 2], pkt[offset + 3]]) as usize;
        if length < 4 || offset + length > pkt.len() {
            break;
        }
        let flowset_data = &pkt[offset + 4..offset + length];

        match flowset_id {
            0 => {
                // Template FlowSet
                parse_v9_template(flowset_data, source_cache);
            }
            1 => {
                // Options Template — skip
            }
            template_id => {
                // Data FlowSet
                if let Some(fields) = source_cache.get(&template_id).cloned() {
                    emit_flows_v9(
                        flowset_data, &fields, sys_uptime_ms, unix_secs, source_id, bus, exporter_ip,
                    );
                }
            }
        }
        offset += length;
    }
    Ok(())
}

fn parse_v9_template(data: &[u8], cache: &mut HashMap<u16, Vec<(u16, u16)>>) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let template_id = u16::from_be_bytes([data[i], data[i + 1]]);
        let field_count = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if i + field_count * 4 > data.len() {
            break;
        }
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let ftype = u16::from_be_bytes([data[i], data[i + 1]]);
            let flen = u16::from_be_bytes([data[i + 2], data[i + 3]]);
            fields.push((ftype, flen));
            i += 4;
        }
        cache.insert(template_id, fields);
    }
}

fn emit_flows_v9(
    data: &[u8],
    fields: &[(u16, u16)],
    sys_uptime_ms: u64,
    unix_secs: u64,
    _source_id: u32,
    bus: &Arc<InProcessBus>,
    exporter_ip: &str,
) {
    let record_len: usize = fields.iter().map(|(_, l)| *l as usize).sum();
    if record_len == 0 {
        return;
    }
    let mut offset = 0usize;
    while offset + record_len <= data.len() {
        let record = &data[offset..offset + record_len];
        if let Some(flow) = decode_record(record, fields, sys_uptime_ms, unix_secs) {
            publish_flow(flow, bus, exporter_ip);
        }
        offset += record_len;
    }
}

// ── IPFIX (v10) ───────────────────────────────────────────────────────────────

fn parse_ipfix(
    pkt: &[u8],
    peer: SocketAddr,
    templates: &mut HashMap<SocketAddr, TemplateCache>,
    bus: &Arc<InProcessBus>,
    exporter_ip: &str,
) -> Result<()> {
    // IPFIX header: version(2) length(2) export_time(4) seq(4) domain_id(4) = 16 bytes
    if pkt.len() < 16 {
        bail!("IPFIX header too short");
    }
    let export_time = u32::from_be_bytes([pkt[4], pkt[5], pkt[6], pkt[7]]) as u64;
    let domain_id = u32::from_be_bytes([pkt[12], pkt[13], pkt[14], pkt[15]]);

    let exporter_cache = templates.entry(peer).or_default();
    let source_cache = exporter_cache.entry(domain_id).or_default();

    let mut offset = 16usize;
    while offset + 4 <= pkt.len() {
        let set_id = u16::from_be_bytes([pkt[offset], pkt[offset + 1]]);
        let length = u16::from_be_bytes([pkt[offset + 2], pkt[offset + 3]]) as usize;
        if length < 4 || offset + length > pkt.len() {
            break;
        }
        let set_data = &pkt[offset + 4..offset + length];

        match set_id {
            2 => {
                // Template Set
                parse_ipfix_template(set_data, source_cache);
            }
            3 => {
                // Options Template — skip
            }
            template_id if template_id >= 256 => {
                if let Some(fields) = source_cache.get(&template_id).cloned() {
                    // IPFIX uses export_time as unix seconds (no uptime offset needed)
                    emit_flows_v9(set_data, &fields, 0, export_time, domain_id, bus, exporter_ip);
                }
            }
            _ => {}
        }
        offset += length;
    }
    Ok(())
}

fn parse_ipfix_template(data: &[u8], cache: &mut HashMap<u16, Vec<(u16, u16)>>) {
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let template_id = u16::from_be_bytes([data[i], data[i + 1]]);
        if template_id < 256 {
            break;
        }
        let field_count = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if i + field_count * 4 > data.len() {
            break;
        }
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            // IPFIX field: type(2) length(2), enterprise bit in high bit of type
            let raw_type = u16::from_be_bytes([data[i], data[i + 1]]);
            let ftype = raw_type & 0x7FFF;
            let flen = u16::from_be_bytes([data[i + 2], data[i + 3]]);
            let enterprise = (raw_type & 0x8000) != 0;
            // Skip 4 extra bytes for enterprise number if enterprise bit set
            fields.push((ftype, flen));
            i += if enterprise { 8 } else { 4 };
        }
        cache.insert(template_id, fields);
    }
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
}

fn decode_record(
    data: &[u8],
    fields: &[(u16, u16)],
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

    let mut offset = 0usize;
    for &(ftype, flen) in fields {
        let flen = flen as usize;
        if offset + flen > data.len() {
            return None;
        }
        let field_data = &data[offset..offset + flen];
        match ftype {
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
            FIELD_FIRST_SWITCHED if flen == 4 => {
                first_ms = u32::from_be_bytes([field_data[0], field_data[1], field_data[2], field_data[3]]) as u64;
            }
            FIELD_LAST_SWITCHED if flen == 4 => {
                last_ms = u32::from_be_bytes([field_data[0], field_data[1], field_data[2], field_data[3]]) as u64;
            }
            _ => {}
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
    })
}

fn publish_flow(flow: FlowRecord, bus: &Arc<InProcessBus>, exporter_ip: &str) {
    let duration_secs = {
        let dur = flow.flow_end_ns - flow.flow_start_ns;
        if dur <= 0 { 1.0 } else { dur as f64 / 1_000_000_000.0 }
    };
    let bytes_per_sec = flow.bytes as f64 / duration_secs;
    let packets_per_sec = flow.packets as f64 / duration_secs;
    let proto_str = protocol_name(flow.protocol);

    let value = serde_json::json!({
        "exporter_address": exporter_ip,
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
    });

    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    bus.publish(TelemetryUpdate {
        target: exporter_ip.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: now_ns,
        path: "streaming/netflow/flow".to_string(),
        value,
    });
}

fn read_variable_u64(data: &[u8]) -> u64 {
    match data.len() {
        1 => data[0] as u64,
        2 => u16::from_be_bytes([data[0], data[1]]) as u64,
        4 => u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64,
        8 => u64::from_be_bytes([
            data[0], data[1], data[2], data[3],
            data[4], data[5], data[6], data[7],
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

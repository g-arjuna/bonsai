use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::config::SflowConfig;
use crate::event_bus::InProcessBus;
use crate::streaming::netflow::classify_amplification_vector;
use crate::telemetry::TelemetryUpdate;

const MAX_UDP_PAYLOAD: usize = 65535;

/// sFlow v5 datagram header (RFC 3176 §4.1)
/// version(4) agent_address_type(4) agent_address(4 or 16) sub_agent_id(4) sequence_number(4) uptime(4) num_samples(4)
/// We only handle IPv4 agent addresses (address_type=1).
const SFLOW_VERSION: u32 = 5;
const ADDR_TYPE_IPV4: u32 = 1;

/// Enterprise 0 sample type codes (RFC 3176 §4.2).
const SAMPLE_FLOW: u32 = 1;
const SAMPLE_COUNTER: u32 = 2;

/// Enterprise 0 flow data format codes (RFC 3176 §3.4.4).
const FLOW_DATA_RAW_PACKET: u32 = 1;
const FLOW_DATA_IPV4: u32 = 3;

/// Enterprise 0 counter data format codes.
const COUNTER_GENERIC_INTERFACE: u32 = 1;

pub async fn run_sflow_receiver(
    cfg: SflowConfig,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let socket = UdpSocket::bind(&cfg.udp_addr).await?;
    info!(addr = %cfg.udp_addr, "sFlow v5 UDP receiver listening");

    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, peer)) => {
                        let pkt = &buf[..len];
                        if let Err(e) = handle_datagram(pkt, peer, &bus) {
                            debug!(%peer, error = %e, "sflow parse error");
                        }
                    }
                    Err(e) => warn!(error = %e, "sflow recv error"),
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }
    Ok(())
}

fn handle_datagram(pkt: &[u8], peer: SocketAddr, bus: &Arc<InProcessBus>) -> Result<()> {
    if pkt.len() < 28 {
        bail!("datagram too short");
    }

    let version = read_u32(pkt, 0)?;
    if version != SFLOW_VERSION {
        bail!("unsupported sflow version {version}");
    }

    let addr_type = read_u32(pkt, 4)?;
    if addr_type != ADDR_TYPE_IPV4 {
        bail!("unsupported agent address type {addr_type} (only IPv4 supported)");
    }

    // agent_address is the authoritative source identity (not UDP src IP)
    let agent_addr = Ipv4Addr::from(read_u32_bytes(pkt, 8)?).to_string();

    // sub_agent_id(4) @ 12, sequence_number(4) @ 16, uptime(4) @ 20
    let num_samples = read_u32(pkt, 24)? as usize;
    debug!(%peer, agent = %agent_addr, num_samples, len = pkt.len(), "sflow datagram accepted");

    let mut offset = 28usize;
    for _ in 0..num_samples {
        if offset + 8 > pkt.len() {
            break;
        }
        let enterprise_format = read_u32(pkt, offset)?;
        let sample_len = read_u32(pkt, offset + 4)? as usize;
        offset += 8;

        if sample_len == 0 || offset + sample_len > pkt.len() {
            break;
        }

        let sample_data = &pkt[offset..offset + sample_len];
        let enterprise = enterprise_format >> 12;
        let format = enterprise_format & 0x0FFF;

        if enterprise == 0 {
            match format {
                SAMPLE_FLOW => {
                    if let Err(e) = parse_flow_sample(sample_data, &agent_addr, bus) {
                        debug!(agent = %agent_addr, error = %e, "sflow flow sample parse error");
                    }
                }
                SAMPLE_COUNTER => {
                    if let Err(e) = parse_counter_sample(sample_data, &agent_addr, bus) {
                        debug!(agent = %agent_addr, error = %e, "sflow counter sample parse error");
                    }
                }
                _ => {}
            }
        }

        offset += sample_len;
    }

    let _ = peer; // peer IP intentionally not used — agent_address is the authoritative exporter identity
    Ok(())
}

/// Parse a flow sample (RFC 3176 §3.4.4):
/// sequence_number(4) source_id(4) sampling_rate(4) sample_pool(4)
/// drops(4) input_if(4) output_if(4) num_records(4) records...
fn parse_flow_sample(data: &[u8], agent_addr: &str, bus: &Arc<InProcessBus>) -> Result<()> {
    if data.len() < 32 {
        bail!("flow sample too short");
    }

    let sampling_rate = read_u32(data, 8)?;
    let num_records = read_u32(data, 28)? as usize;

    let mut offset = 32usize;
    for _ in 0..num_records {
        if offset + 8 > data.len() {
            break;
        }
        let enterprise_format = read_u32(data, offset)?;
        let record_len = read_u32(data, offset + 4)? as usize;
        offset += 8;

        if record_len == 0 || offset + record_len > data.len() {
            break;
        }

        let record_data = &data[offset..offset + record_len];
        let enterprise = enterprise_format >> 12;
        let format = enterprise_format & 0x0FFF;

        if enterprise == 0 {
            match format {
                FLOW_DATA_RAW_PACKET => {
                    if let Some(flow) = parse_raw_packet_header(record_data) {
                        publish_flow(flow, agent_addr, sampling_rate, bus);
                    }
                }
                FLOW_DATA_IPV4 => {
                    if let Some(flow) = parse_ipv4_sampled_header(record_data) {
                        publish_flow(flow, agent_addr, sampling_rate, bus);
                    }
                }
                _ => {}
            }
        }

        offset += record_len;
    }
    Ok(())
}

/// Parse a counter sample (RFC 3176 §3.4.6):
/// sequence_number(4) source_id(4) num_records(4) records...
fn parse_counter_sample(data: &[u8], agent_addr: &str, bus: &Arc<InProcessBus>) -> Result<()> {
    if data.len() < 12 {
        bail!("counter sample too short");
    }

    let num_records = read_u32(data, 8)? as usize;
    let mut offset = 12usize;

    for _ in 0..num_records {
        if offset + 8 > data.len() {
            break;
        }
        let enterprise_format = read_u32(data, offset)?;
        let record_len = read_u32(data, offset + 4)? as usize;
        offset += 8;

        if record_len == 0 || offset + record_len > data.len() {
            break;
        }

        let record_data = &data[offset..offset + record_len];
        let enterprise = enterprise_format >> 12;
        let format = enterprise_format & 0x0FFF;

        if enterprise == 0 && format == COUNTER_GENERIC_INTERFACE {
            if let Some(counters) = parse_generic_interface_counters(record_data) {
                publish_counters(counters, agent_addr, bus);
            }
        }

        offset += record_len;
    }
    Ok(())
}

struct SampledFlow {
    src_address: String,
    dst_address: String,
    src_port: u16,
    dst_port: u16,
    protocol: u8,
    frame_length: u32,
    /// DS-1 T6: TCP flags byte extracted from sampled packet header
    tcp_flags: u8,
    /// DS-1 T6: ICMP type and code (protocol=1 only)
    icmp_type: u8,
    icmp_code: u8,
}

struct InterfaceCounters {
    if_index: u32,
    if_in_octets: u64,
    if_out_octets: u64,
    if_in_errors: u32,
    if_out_errors: u32,
    if_in_discards: u32,
    if_out_discards: u32,
    if_speed: u64,
}

/// Parse raw packet header record (format 1, RFC 3176 §3.4.4.1):
/// header_protocol(4) frame_length(4) stripped(4) header_size(4) header_bytes(variable)
/// For Ethernet frames, skip 14 bytes to reach the IP header.
fn parse_raw_packet_header(data: &[u8]) -> Option<SampledFlow> {
    if data.len() < 16 {
        return None;
    }
    let header_protocol = read_u32(data, 0).ok()?;
    let frame_length = read_u32(data, 4).ok()?;
    let _stripped = read_u32(data, 8).ok()?;
    let header_size = read_u32(data, 12).ok()? as usize;

    if data.len() < 16 + header_size {
        return None;
    }
    let header = &data[16..16 + header_size];

    // header_protocol 1 = Ethernet
    if header_protocol == 1 {
        // Skip 14-byte Ethernet header to reach IP
        if header.len() < 34 {
            return None;
        }
        let ethertype = u16::from_be_bytes([header[12], header[13]]);
        // EtherType 0x0800 = IPv4
        if ethertype != 0x0800 {
            return None;
        }
        parse_ipv4_header(&header[14..], frame_length)
    } else {
        None
    }
}

/// Parse sampled IPv4 header record (format 3, RFC 3176 §3.4.4.3):
/// length(4) protocol(4) src_ip(4) dst_ip(4) src_port(4) dst_port(4) tcp_flags(4) tos(4)
fn parse_ipv4_sampled_header(data: &[u8]) -> Option<SampledFlow> {
    if data.len() < 32 {
        return None;
    }
    let frame_length = read_u32(data, 0).ok()?;
    let protocol = read_u32(data, 4).ok()? as u8;
    let src_addr = Ipv4Addr::from(read_u32_bytes(data, 8).ok()?).to_string();
    let dst_addr = Ipv4Addr::from(read_u32_bytes(data, 12).ok()?).to_string();
    let src_port = read_u32(data, 16).ok()? as u16;
    let dst_port = read_u32(data, 20).ok()? as u16;
    let tcp_flags_word = read_u32(data, 24).ok()? as u8;
    let (tcp_flags, icmp_type, icmp_code) = extract_transport_flags(protocol, tcp_flags_word, src_port);

    Some(SampledFlow {
        src_address: src_addr,
        dst_address: dst_addr,
        src_port,
        dst_port,
        protocol,
        frame_length,
        tcp_flags,
        icmp_type,
        icmp_code,
    })
}

/// Parse a minimal IPv4 header from raw bytes (IHL * 4 header + transport layer).
fn parse_ipv4_header(ip: &[u8], frame_length: u32) -> Option<SampledFlow> {
    if ip.len() < 20 {
        return None;
    }
    let ihl = ((ip[0] & 0x0F) * 4) as usize;
    let protocol = ip[9];
    let src_addr = Ipv4Addr::from([ip[12], ip[13], ip[14], ip[15]]).to_string();
    let dst_addr = Ipv4Addr::from([ip[16], ip[17], ip[18], ip[19]]).to_string();

    let (src_port, dst_port, tcp_flags, icmp_type, icmp_code) =
        if ip.len() >= ihl + 4 {
            let t = &ip[ihl..];
            let sp = u16::from_be_bytes([t[0], t[1]]);
            let dp = u16::from_be_bytes([t[2], t[3]]);
            let (tf, it, ic) = if protocol == 6 && t.len() >= 14 {
                (t[13], 0u8, 0u8)
            } else if protocol == 1 && t.len() >= 2 {
                (0u8, t[0], t[1])
            } else {
                (0u8, 0u8, 0u8)
            };
            (sp, dp, tf, it, ic)
        } else {
            (0, 0, 0, 0, 0)
        };

    Some(SampledFlow {
        src_address: src_addr,
        dst_address: dst_addr,
        src_port,
        dst_port,
        protocol,
        frame_length,
        tcp_flags,
        icmp_type,
        icmp_code,
    })
}

/// DS-1 T6: Extract transport-layer flags from the sampled IPv4 header record (format 3).
/// In format-3 records the tcp_flags field occupies bytes 24-27 as a 32-bit word;
/// only the low byte is meaningful for TCP. For ICMP the src_port carries type/code.
fn extract_transport_flags(protocol: u8, flags_word: u8, src_port: u16) -> (u8, u8, u8) {
    match protocol {
        6 => (flags_word, 0, 0),
        1 => (0, (src_port >> 8) as u8, (src_port & 0xFF) as u8),
        _ => (0, 0, 0),
    }
}

/// Parse generic interface counter record (RFC 3176 §3.4.6.1, 88 bytes):
/// if_index(4) if_type(4) if_speed(8) if_direction(4) if_status(4)
/// in_octets(8) in_ucast(4) in_mcast(4) in_bcast(4) in_discards(4) in_errors(4) in_unknown_proto(4)
/// out_octets(8) out_ucast(4) out_mcast(4) out_bcast(4) out_discards(4) out_errors(4) promiscuous(4)
fn parse_generic_interface_counters(data: &[u8]) -> Option<InterfaceCounters> {
    if data.len() < 88 {
        return None;
    }
    let if_index = read_u32(data, 0).ok()?;
    let if_speed = read_u64(data, 8).ok()?;
    let in_octets = read_u64(data, 24).ok()?;
    let in_discards = read_u32(data, 36).ok()?;
    let in_errors = read_u32(data, 40).ok()?;
    let out_octets = read_u64(data, 56).ok()?;
    let out_discards = read_u32(data, 68).ok()?;
    let out_errors = read_u32(data, 72).ok()?;

    Some(InterfaceCounters {
        if_index,
        if_in_octets: in_octets,
        if_out_octets: out_octets,
        if_in_errors: in_errors,
        if_out_errors: out_errors,
        if_in_discards: in_discards,
        if_out_discards: out_discards,
        if_speed,
    })
}

fn publish_flow(flow: SampledFlow, agent_addr: &str, sampling_rate: u32, bus: &Arc<InProcessBus>) {
    let proto_str = protocol_name(flow.protocol);
    let now_ns = now_ns();

    // bytes_per_sec approximation: scaled frame length by sampling rate.
    // This is a rough estimate — proper rate calculation requires windowing (see D4-5 T5).
    let bytes_per_sec = flow.frame_length as f64 * sampling_rate as f64;
    let amplification_vector = classify_amplification_vector(flow.protocol, flow.dst_port);
    let tcp_flags_pattern = classify_tcp_flags_sflow(flow.tcp_flags, flow.protocol);

    let value = serde_json::json!({
        "exporter_address": agent_addr,
        "src_address": flow.src_address,
        "dst_address": flow.dst_address,
        "src_port": flow.src_port,
        "dst_port": flow.dst_port,
        "protocol": proto_str,
        "frame_length": flow.frame_length,
        "sampling_rate": sampling_rate,
        "bytes_per_sec": bytes_per_sec,
        "packets_per_sec": sampling_rate as f64,
        "tcp_flags": flow.tcp_flags,
        "tcp_flags_pattern": tcp_flags_pattern,
        "icmp_type": flow.icmp_type,
        "icmp_code": flow.icmp_code,
        "amplification_vector": amplification_vector,
    });

    debug!(
        agent = %agent_addr,
        src = %flow.src_address,
        dst = %flow.dst_address,
        protocol = %proto_str,
        frame_length = flow.frame_length,
        sampling_rate,
        "publishing sflow flow sample"
    );

    bus.publish(TelemetryUpdate {
        target: agent_addr.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: now_ns,
        path: "streaming/sflow/flow".to_string(),
        value,
    });
}

fn publish_counters(c: InterfaceCounters, agent_addr: &str, bus: &Arc<InProcessBus>) {
    let now_ns = now_ns();

    let value = serde_json::json!({
        "exporter_address": agent_addr,
        "if_index": c.if_index,
        "if_speed": c.if_speed,
        "in_octets": c.if_in_octets,
        "out_octets": c.if_out_octets,
        "in_errors": c.if_in_errors,
        "out_errors": c.if_out_errors,
        "in_discards": c.if_in_discards,
        "out_discards": c.if_out_discards,
    });

    bus.publish(TelemetryUpdate {
        target: agent_addr.to_string(),
        vendor: String::new(),
        hostname: String::new(),
        role: String::new(),
        site: String::new(),
        timestamp_ns: now_ns,
        path: "streaming/sflow/counters".to_string(),
        value,
    });
}

/// DS-1 T6: Classify TCP flags from sFlow sampled header for DDoS pattern detection.
fn classify_tcp_flags_sflow(flags: u8, protocol: u8) -> &'static str {
    if protocol != 6 {
        return "";
    }
    match flags & 0x3F {
        f if f == 0x02 => "SYN_ONLY",
        f if f == 0x12 => "SYN_ACK",
        f if f & 0x04 != 0 && f & 0x02 == 0 => "RST_FLOOD",
        f if f == 0x10 => "ACK_FLOOD",
        f if f == 0x01 => "FIN_FLOOD",
        f if f & 0x02 != 0 => "SYN_MIX",
        _ => "ESTABLISHED",
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > data.len() {
        bail!("read_u32 out of bounds at {offset}");
    }
    Ok(u32::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

fn read_u32_bytes(data: &[u8], offset: usize) -> Result<[u8; 4]> {
    if offset + 4 > data.len() {
        bail!("read_u32_bytes out of bounds at {offset}");
    }
    Ok([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64> {
    if offset + 8 > data.len() {
        bail!("read_u64 out of bounds at {offset}");
    }
    Ok(u64::from_be_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ]))
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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

    #[test]
    fn test_read_u32() {
        let data = [0x00, 0x00, 0x00, 0x05u8];
        assert_eq!(read_u32(&data, 0).unwrap(), 5);
    }

    #[test]
    fn test_parse_ipv4_sampled_header() {
        // Build a minimal format-3 sampled IPv4 header record (32 bytes)
        let mut data = vec![0u8; 32];
        // length = 1500
        data[0..4].copy_from_slice(&1500u32.to_be_bytes());
        // protocol = 6 (TCP)
        data[4..8].copy_from_slice(&6u32.to_be_bytes());
        // src_ip = 10.0.0.1
        data[8..12].copy_from_slice(&[10, 0, 0, 1]);
        // dst_ip = 10.0.0.2
        data[12..16].copy_from_slice(&[10, 0, 0, 2]);
        // src_port = 12345
        data[16..20].copy_from_slice(&12345u32.to_be_bytes());
        // dst_port = 80
        data[20..24].copy_from_slice(&80u32.to_be_bytes());

        let flow = parse_ipv4_sampled_header(&data).unwrap();
        assert_eq!(flow.src_address, "10.0.0.1");
        assert_eq!(flow.dst_address, "10.0.0.2");
        assert_eq!(flow.src_port, 12345);
        assert_eq!(flow.dst_port, 80);
        assert_eq!(flow.protocol, 6);
    }

    #[test]
    fn test_protocol_name() {
        assert_eq!(protocol_name(6), "tcp");
        assert_eq!(protocol_name(17), "udp");
        assert_eq!(protocol_name(1), "icmp");
        assert_eq!(protocol_name(255), "other");
    }
}

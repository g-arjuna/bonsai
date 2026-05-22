use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};

use crate::config::{BmpConfig, TargetConfig};
use crate::event_bus::InProcessBus;
use crate::resource_governor::GovernorHandle;
use crate::telemetry::TelemetryUpdate;

const BMP_VERSION: u8 = 3;
const BMP_HEADER_LEN: usize = 6;
const BMP_COMMON_PEER_HEADER_LEN: usize = 42;
const BGP_HEADER_LEN: usize = 19;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpEvent {
    pub timestamp_ns: i64,
    pub collector_peer: String,
    pub message_type: String,
    pub peer_type: u8,
    pub peer_flags: u8,
    pub router_distinguisher: u64,
    pub router_address: String,
    pub peer_address: String,
    pub peer_as: u32,
    pub peer_bgp_id: String,
    pub session_state: String,
    pub route_entries: Vec<BmpRouteEntry>,
    pub raw_len: usize,
    // PeerDown fields (RFC 7854 §4.9)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_down_reason: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_down_reason_name: Option<String>,
    // StatisticsReport fields (RFC 7854 §4.8)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stats: Vec<BmpStatEntry>,
    // Initiation fields (RFC 7854 §4.3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sys_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sys_descr: Option<String>,
    // Termination fields (RFC 7854 §4.5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason_name: Option<String>,
    // RFC 7854 §4.2 — Pre/Post-policy and Loc-RIB classification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rib_type: Option<String>,
    // RFC 7854 §4.6 — PeerUp parsed BGP OPEN info
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_up_info: Option<BmpPeerUpInfo>,
    // RFC 7854 §4.3 — Initiation TLV type 2 (admin string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_admin_string: Option<String>,
    // RFC 7854 §4.5 — Termination TLV type 0 (free-form message)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_message: Option<String>,
}

/// RFC 7854 §4.6 — Parsed fields from PeerUp Notification Local/Remote OPEN
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpPeerUpInfo {
    pub local_address: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub sent_hold_time: u16,
    pub received_hold_time: u16,
    pub sent_bgp_id: String,
    pub received_bgp_id: String,
    pub sent_capabilities: Vec<String>,
    pub received_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpStatEntry {
    pub stat_type: u16,
    pub stat_name: String,
    pub value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpRouteEntry {
    pub afi_safi: String,
    pub prefix: String,
    pub prefix_len: u8,
    pub action: String,
    pub next_hop: String,
    pub as_path: Vec<u32>,
    pub communities: Vec<String>,
    pub med: Option<u32>,
    pub local_pref: Option<u32>,
}

pub async fn run_bmp_receiver(
    cfg: BmpConfig,
    targets: Vec<TargetConfig>,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
    governor: Option<GovernorHandle>,
) -> Result<()> {
    let archive = JsonLineArchive::open(&cfg.archive_path).await?;
    let target_map = Arc::new(BmpTargetMap::new(&targets));
    let governor = governor.map(Arc::new);
    let listener = TcpListener::bind(&cfg.tcp_addr)
        .await
        .with_context(|| format!("bind BMP listener at {}", cfg.tcp_addr))?;
    info!(addr = %cfg.tcp_addr, "BMP listener started");

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("BMP listener stopping");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((mut stream, peer)) => {
                        let archive = archive.clone();
                        let target_map = Arc::clone(&target_map);
                        let bus = Arc::clone(&bus);
                        let max_frame_bytes = cfg.max_frame_bytes;
                        let governor = governor.clone();
                        tokio::spawn(async move {
                            loop {
                                let mut header = [0_u8; BMP_HEADER_LEN];
                                if let Err(error) = stream.read_exact(&mut header).await {
                                    if error.kind() != std::io::ErrorKind::UnexpectedEof {
                                        warn!(%error, peer = %peer, "BMP read header failed");
                                    }
                                    break;
                                }
                                let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
                                if header[0] != BMP_VERSION {
                                    warn!(peer = %peer, version = header[0], "unsupported BMP version");
                                    break;
                                }
                                if !(BMP_HEADER_LEN..=max_frame_bytes).contains(&length) {
                                    warn!(peer = %peer, length, "BMP frame exceeded configured bounds");
                                    break;
                                }
                                let mut payload = vec![0_u8; length - BMP_HEADER_LEN];
                                if let Err(error) = stream.read_exact(&mut payload).await {
                                    warn!(%error, peer = %peer, "BMP payload read failed");
                                    break;
                                }
                                match parse_bmp_message(header[5], &payload, peer.to_string()) {
                                    Ok(event) => {
                                        if let Err(error) = archive.append(&event).await {
                                            warn!(%error, "failed to archive BMP event");
                                        }
                                        publish_event(&bus, &target_map, event, governor.as_deref());
                                    }
                                    Err(error) => warn!(%error, peer = %peer, "failed to parse BMP message"),
                                }
                            }
                        });
                    }
                    Err(error) => warn!(%error, "BMP accept failed"),
                }
            }
        }
    }

    Ok(())
}

fn publish_event(
    bus: &Arc<InProcessBus>,
    target_map: &BmpTargetMap,
    event: BmpEvent,
    governor: Option<&GovernorHandle>,
) {
    let path = match event.message_type.as_str() {
        "route_monitoring" => "streaming/bmp/route-monitoring",
        "peer_up" => "streaming/bmp/peer-up",
        "peer_down" => "streaming/bmp/peer-down",
        "statistics_report" => "streaming/bmp/statistics",
        "initiation" => "streaming/bmp/initiation",
        "termination" => "streaming/bmp/termination",
        _ => "streaming/bmp/unknown",
    };

    // Under rate shedding or memory pressure, drop low-value StatisticsReport messages
    // while preserving high-value state-change events (RouteMonitoring, PeerUp, PeerDown).
    // D2-10 T5: should_shed() covers both rate_shedding_active and memory_pressure_active.
    if event.message_type == "statistics_report" && governor.is_some_and(|g| g.should_shed()) {
        metrics::counter!("bonsai_bmp_shed_total").increment(1);
        return;
    }

    let resolved = target_map.resolve(&event);
    let value = serde_json::to_value(event).unwrap_or_default();
    bus.publish(TelemetryUpdate {
        target: resolved.address,
        vendor: resolved.vendor,
        hostname: resolved.hostname,
        role: resolved.role,
        site: resolved.site,
        timestamp_ns: now_ns(),
        path: path.to_string(),
        value,
    });
}

fn parse_bmp_message(message_type: u8, payload: &[u8], collector_peer: String) -> Result<BmpEvent> {
    let timestamp_ns = now_ns();

    // Initiation (4) and Termination (5) carry TLVs only — no per-peer header (RFC 7854 §4.3, §4.5)
    if message_type == 4 {
        return parse_initiation(payload, collector_peer, timestamp_ns);
    }
    if message_type == 5 {
        return parse_termination(payload, collector_peer, timestamp_ns);
    }

    if payload.len() < BMP_COMMON_PEER_HEADER_LEN {
        bail!("BMP payload shorter than common peer header");
    }
    let peer = parse_common_peer_header(&payload[..BMP_COMMON_PEER_HEADER_LEN])?;
    let body = &payload[BMP_COMMON_PEER_HEADER_LEN..];

    // RFC 7854 §4.2 / RFC 8671 / RFC 9069 — classify RIB type from peer_type + flags
    let rib_type = classify_rib_type(peer.peer_type, peer.peer_flags);

    let mut event = BmpEvent {
        timestamp_ns,
        collector_peer,
        message_type: String::new(),
        peer_type: peer.peer_type,
        peer_flags: peer.peer_flags,
        router_distinguisher: peer.router_distinguisher,
        router_address: peer.router_address,
        peer_address: peer.peer_address,
        peer_as: peer.peer_as,
        peer_bgp_id: peer.peer_bgp_id,
        session_state: String::new(),
        route_entries: Vec::new(),
        raw_len: payload.len() + BMP_HEADER_LEN,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name: None,
        sys_descr: None,
        termination_reason: None,
        termination_reason_name: None,
        rib_type: Some(rib_type),
        peer_up_info: None,
        init_admin_string: None,
        termination_message: None,
    };

    match message_type {
        0 => {
            event.message_type = "route_monitoring".to_string();
            event.session_state = "established".to_string();
            event.route_entries = parse_route_monitoring(body)?;
        }
        1 => {
            event.message_type = "statistics_report".to_string();
            event.session_state = "established".to_string();
            event.stats = parse_statistics_report(body);
        }
        2 => {
            event.message_type = "peer_down".to_string();
            event.session_state = "down".to_string();
            let (reason, reason_name) = parse_peer_down_reason(body);
            event.peer_down_reason = Some(reason);
            event.peer_down_reason_name = Some(reason_name);
        }
        3 => {
            event.message_type = "peer_up".to_string();
            event.session_state = "established".to_string();
            event.peer_up_info = parse_peer_up(body, peer.peer_flags);
        }
        other => return Err(anyhow!("unsupported BMP message type {other}")),
    }

    Ok(event)
}

fn parse_statistics_report(body: &[u8]) -> Vec<BmpStatEntry> {
    if body.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let mut cursor = 4;
    let mut entries = Vec::with_capacity(count.min(64));

    while entries.len() < count && cursor + 4 <= body.len() {
        let stat_type = u16::from_be_bytes([body[cursor], body[cursor + 1]]);
        let stat_len = u16::from_be_bytes([body[cursor + 2], body[cursor + 3]]) as usize;
        cursor += 4;
        if cursor + stat_len > body.len() {
            break;
        }
        let value_bytes = &body[cursor..cursor + stat_len];
        cursor += stat_len;

        let value = match stat_len {
            4 if value_bytes.len() == 4 => {
                u32::from_be_bytes(value_bytes.try_into().unwrap_or([0; 4])) as u64
            }
            8 if value_bytes.len() == 8 => {
                u64::from_be_bytes(value_bytes.try_into().unwrap_or([0; 8]))
            }
            // Per-AFI/SAFI counters (type 9/10): 2+1+4=7 bytes; extract just the count
            7 if value_bytes.len() == 7 => {
                u32::from_be_bytes(value_bytes[3..7].try_into().unwrap_or([0; 4])) as u64
            }
            _ => 0,
        };

        entries.push(BmpStatEntry {
            stat_type,
            stat_name: stat_type_name(stat_type).to_string(),
            value,
        });
    }
    entries
}

fn stat_type_name(t: u16) -> &'static str {
    match t {
        0 => "prefixes_rejected_by_policy",
        1 => "duplicate_prefix_advertisements",
        2 => "duplicate_withdrawals",
        3 => "updates_invalid_cluster_list_loop",
        4 => "updates_invalid_as_path_loop",
        5 => "updates_invalid_originator_id",
        6 => "updates_invalid_as_confed_loop",
        7 => "adj_rib_in_routes",
        8 => "loc_rib_routes",
        9 => "adj_rib_in_routes_per_afi_safi",
        10 => "loc_rib_routes_per_afi_safi",
        11 => "updates_route_refresh",
        12 => "routes_stale_graceful_restart",
        13 => "routes_reclaimed_graceful_restart",
        14 => "routes_not_installed_vpn",
        15 => "routes_filtered_adj_rib_out",
        _ => "unknown",
    }
}

fn parse_peer_down_reason(body: &[u8]) -> (u8, String) {
    if body.is_empty() {
        return (0, "unknown".to_string());
    }
    let reason = body[0];
    // RFC 7854 §4.9 reason codes + RFC 9069 code 6
    let name = match reason {
        0 => "reserved",
        1 => "local_bgp_notification",
        2 => "local_fsm_event",
        3 => "remote_bgp_notification",
        4 => "remote_close_no_data",
        5 => "peer_de_configured",
        6 => "vrf_peer_deleted",
        _ => "unknown",
    };
    (reason, name.to_string())
}

fn parse_initiation(payload: &[u8], collector_peer: String, timestamp_ns: i64) -> Result<BmpEvent> {
    let mut sys_name = None;
    let mut sys_descr = None;
    let mut init_admin_string = None;
    let mut cursor = 0;

    while cursor + 4 <= payload.len() {
        let tlv_type = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]);
        let tlv_len = u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]) as usize;
        cursor += 4;
        if cursor + tlv_len > payload.len() {
            break;
        }
        let value = &payload[cursor..cursor + tlv_len];
        cursor += tlv_len;
        match tlv_type {
            0 => sys_descr = Some(String::from_utf8_lossy(value).into_owned()),
            1 => sys_name = Some(String::from_utf8_lossy(value).into_owned()),
            2 => init_admin_string = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
    }

    Ok(BmpEvent {
        timestamp_ns,
        collector_peer,
        message_type: "initiation".to_string(),
        peer_type: 0,
        peer_flags: 0,
        router_distinguisher: 0,
        router_address: String::new(),
        peer_address: String::new(),
        peer_as: 0,
        peer_bgp_id: String::new(),
        session_state: "connected".to_string(),
        route_entries: Vec::new(),
        raw_len: payload.len() + BMP_HEADER_LEN,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name,
        sys_descr,
        termination_reason: None,
        termination_reason_name: None,
        rib_type: None,
        peer_up_info: None,
        init_admin_string,
        termination_message: None,
    })
}

fn parse_termination(
    payload: &[u8],
    collector_peer: String,
    timestamp_ns: i64,
) -> Result<BmpEvent> {
    let mut termination_reason = None;
    let mut termination_reason_name = None;
    let mut termination_message = None;
    let mut cursor = 0;

    while cursor + 4 <= payload.len() {
        let tlv_type = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]);
        let tlv_len = u16::from_be_bytes([payload[cursor + 2], payload[cursor + 3]]) as usize;
        cursor += 4;
        if cursor + tlv_len > payload.len() {
            break;
        }
        let value = &payload[cursor..cursor + tlv_len];
        cursor += tlv_len;
        match tlv_type {
            0 => termination_message = Some(String::from_utf8_lossy(value).into_owned()),
            1 if tlv_len == 2 => {
                let code = u16::from_be_bytes([value[0], value[1]]);
                let name = match code {
                    0 => "session_admin_closed",
                    1 => "unspecified",
                    2 => "out_of_resources",
                    3 => "redundant_connection",
                    4 => "perm_admin_closed",
                    _ => "unknown",
                };
                termination_reason = Some(code);
                termination_reason_name = Some(name.to_string());
            }
            _ => {}
        }
    }

    Ok(BmpEvent {
        timestamp_ns,
        collector_peer,
        message_type: "termination".to_string(),
        peer_type: 0,
        peer_flags: 0,
        router_distinguisher: 0,
        router_address: String::new(),
        peer_address: String::new(),
        peer_as: 0,
        peer_bgp_id: String::new(),
        session_state: "disconnected".to_string(),
        route_entries: Vec::new(),
        raw_len: payload.len() + BMP_HEADER_LEN,
        peer_down_reason: None,
        peer_down_reason_name: None,
        stats: Vec::new(),
        sys_name: None,
        sys_descr: None,
        termination_reason,
        termination_reason_name,
        rib_type: None,
        peer_up_info: None,
        init_admin_string: None,
        termination_message,
    })
}

struct ParsedPeer {
    peer_type: u8,
    peer_flags: u8,
    router_distinguisher: u64,
    router_address: String,
    peer_address: String,
    peer_as: u32,
    peer_bgp_id: String,
}

fn parse_common_peer_header(input: &[u8]) -> Result<ParsedPeer> {
    let peer_type = input[0];
    let peer_flags = input[1];
    let router_distinguisher = u64::from_be_bytes(input[2..10].try_into().unwrap_or([0; 8]));
    let raw_addr: [u8; 16] = input[10..26]
        .try_into()
        .map_err(|_| anyhow!("invalid BMP peer address length"))?;
    let is_ipv6 = peer_flags & 0x80 != 0;
    let peer_address = if is_ipv6 {
        IpAddr::V6(Ipv6Addr::from(raw_addr)).to_string()
    } else {
        IpAddr::V4(Ipv4Addr::new(
            raw_addr[12],
            raw_addr[13],
            raw_addr[14],
            raw_addr[15],
        ))
        .to_string()
    };
    let peer_as = u32::from_be_bytes(input[26..30].try_into().unwrap_or([0; 4]));
    let peer_bgp_id = Ipv4Addr::from(u32::from_be_bytes(
        input[30..34].try_into().unwrap_or([0; 4]),
    ))
    .to_string();
    let router_address = peer_bgp_id.clone();
    Ok(ParsedPeer {
        peer_type,
        peer_flags,
        router_distinguisher,
        router_address,
        peer_address,
        peer_as,
        peer_bgp_id,
    })
}

/// RFC 7854 §4.2, RFC 8671, RFC 9069 — classify RIB type from peer_type + peer_flags.
/// peer_type 0 = Global Instance Peer (Adj-RIB-In pre/post-policy based on L flag)
/// peer_type 1 = RD Instance Peer
/// peer_type 2 = Local Instance Peer (RFC 8671: Adj-RIB-Out)
/// peer_type 3 = Loc-RIB (RFC 9069)
/// L flag (bit 6, 0x40) = 0 → pre-policy, 1 → post-policy
fn classify_rib_type(peer_type: u8, peer_flags: u8) -> String {
    match peer_type {
        3 => "loc-rib".to_string(),
        2 => {
            if peer_flags & 0x40 != 0 {
                "adj-rib-out-post-policy".to_string()
            } else {
                "adj-rib-out-pre-policy".to_string()
            }
        }
        _ => {
            if peer_flags & 0x40 != 0 {
                "adj-rib-in-post-policy".to_string()
            } else {
                "adj-rib-in-pre-policy".to_string()
            }
        }
    }
}

/// RFC 7854 §4.6 — PeerUp Notification body:
///   20 bytes: local address (16) + local port (2) + remote port (2)
///   Sent OPEN message (BGP header + OPEN)
///   Received OPEN message (BGP header + OPEN)
///   Optional: Information TLVs
fn parse_peer_up(body: &[u8], peer_flags: u8) -> Option<BmpPeerUpInfo> {
    if body.len() < 20 {
        return None;
    }
    let is_ipv6 = peer_flags & 0x80 != 0;
    let local_address = if is_ipv6 {
        IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&body[0..16]).unwrap_or([0; 16]))).to_string()
    } else {
        IpAddr::V4(Ipv4Addr::new(body[12], body[13], body[14], body[15])).to_string()
    };
    let local_port = u16::from_be_bytes([body[16], body[17]]);
    let remote_port = u16::from_be_bytes([body[18], body[19]]);

    let rest = &body[20..];
    let (sent_open, after_sent) = parse_bgp_open_from_message(rest)?;
    let (received_open, _) = parse_bgp_open_from_message(after_sent)?;

    Some(BmpPeerUpInfo {
        local_address,
        local_port,
        remote_port,
        sent_hold_time: sent_open.hold_time,
        received_hold_time: received_open.hold_time,
        sent_bgp_id: sent_open.bgp_id,
        received_bgp_id: received_open.bgp_id,
        sent_capabilities: sent_open.capabilities,
        received_capabilities: received_open.capabilities,
    })
}

struct ParsedBgpOpen {
    hold_time: u16,
    bgp_id: String,
    capabilities: Vec<String>,
}

/// Parse a BGP OPEN message (prefixed by 19-byte BGP header).
/// Returns the parsed OPEN and remaining bytes.
fn parse_bgp_open_from_message(data: &[u8]) -> Option<(ParsedBgpOpen, &[u8])> {
    if data.len() < BGP_HEADER_LEN {
        return None;
    }
    let msg_len = u16::from_be_bytes([data[16], data[17]]) as usize;
    if msg_len > data.len() || msg_len < BGP_HEADER_LEN + 10 {
        return None;
    }
    // BGP OPEN type = 1
    if data[18] != 1 {
        return None;
    }
    let open = &data[BGP_HEADER_LEN..msg_len];
    // RFC 4271 §4.2: version(1) + my_as(2) + hold_time(2) + bgp_id(4) + opt_params_len(1)
    // Offsets:        [0]          [1..2]       [3..4]        [5..8]       [9]
    if open.len() < 10 {
        return None;
    }
    let hold_time = u16::from_be_bytes([open[3], open[4]]);
    let bgp_id = Ipv4Addr::new(open[5], open[6], open[7], open[8]).to_string();
    let opt_params_len = open[9] as usize;
    let opt_params = if open.len() >= 10 + opt_params_len {
        &open[10..10 + opt_params_len]
    } else {
        &[]
    };
    let capabilities = parse_bgp_capabilities(opt_params);
    let remaining = &data[msg_len..];
    Some((
        ParsedBgpOpen {
            hold_time,
            bgp_id,
            capabilities,
        },
        remaining,
    ))
}

/// Parse BGP Optional Parameters (type 2 = Capability) and extract capability names.
fn parse_bgp_capabilities(params: &[u8]) -> Vec<String> {
    let mut caps = Vec::new();
    let mut cursor = 0;
    while cursor + 2 <= params.len() {
        let param_type = params[cursor];
        let param_len = params[cursor + 1] as usize;
        cursor += 2;
        if cursor + param_len > params.len() {
            break;
        }
        if param_type == 2 {
            // Capability parameter — parse inner capability TLVs
            let mut cap_cursor = cursor;
            while cap_cursor + 2 <= cursor + param_len {
                let cap_code = params[cap_cursor];
                let cap_len = params[cap_cursor + 1] as usize;
                cap_cursor += 2;
                if cap_cursor + cap_len > cursor + param_len {
                    break;
                }
                caps.push(capability_name(cap_code));
                cap_cursor += cap_len;
            }
        }
        cursor += param_len;
    }
    caps
}

fn capability_name(code: u8) -> String {
    match code {
        1 => "multiprotocol".to_string(),
        2 => "route-refresh".to_string(),
        5 => "extended-next-hop".to_string(),
        6 => "extended-message".to_string(),
        64 => "graceful-restart".to_string(),
        65 => "4-byte-as".to_string(),
        69 => "add-path".to_string(),
        70 => "enhanced-route-refresh".to_string(),
        71 => "long-lived-graceful-restart".to_string(),
        73 => "fqdn".to_string(),
        128 => "route-refresh-cisco".to_string(),
        _ => format!("cap-{code}"),
    }
}

fn parse_route_monitoring(payload: &[u8]) -> Result<Vec<BmpRouteEntry>> {
    if payload.len() < BGP_HEADER_LEN {
        bail!("route monitoring payload shorter than BGP header");
    }
    if payload[..16] != [0xff; 16] {
        bail!("invalid BGP marker in route monitoring payload");
    }
    let length = u16::from_be_bytes([payload[16], payload[17]]) as usize;
    if length > payload.len() || payload[18] != 2 {
        return Ok(Vec::new());
    }
    parse_bgp_update(&payload[19..length])
}

fn parse_bgp_update(payload: &[u8]) -> Result<Vec<BmpRouteEntry>> {
    if payload.len() < 4 {
        bail!("BGP UPDATE payload too short");
    }
    let mut cursor = 0;
    let withdrawn_len = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]) as usize;
    cursor += 2;
    if cursor + withdrawn_len > payload.len() {
        bail!("withdrawn routes length exceeds BGP UPDATE payload");
    }
    let withdrawn_routes = parse_nlri(
        "ipv4-unicast",
        &payload[cursor..cursor + withdrawn_len],
        "withdraw",
    )?;
    cursor += withdrawn_len;

    if cursor + 2 > payload.len() {
        bail!("BGP UPDATE missing path attribute length");
    }
    let attrs_len = u16::from_be_bytes([payload[cursor], payload[cursor + 1]]) as usize;
    cursor += 2;
    if cursor + attrs_len > payload.len() {
        bail!("BGP path attribute length exceeds UPDATE payload");
    }
    let attrs = &payload[cursor..cursor + attrs_len];
    cursor += attrs_len;
    let mut path_state = PathState::default();
    parse_path_attributes(attrs, &mut path_state)?;

    let mut entries = withdrawn_routes;
    let announced_v4 = parse_nlri("ipv4-unicast", &payload[cursor..], "announce")?;
    entries.extend(announced_v4.into_iter().map(|mut entry| {
        entry.next_hop = path_state.next_hop.clone();
        entry.as_path = path_state.as_path.clone();
        entry.communities = path_state.communities.clone();
        entry.med = path_state.med;
        entry.local_pref = path_state.local_pref;
        entry
    }));
    for mut entry in path_state.mp_reach {
        entry.action = "announce".to_string();
        entry.as_path = path_state.as_path.clone();
        entry.communities = path_state.communities.clone();
        entry.med = path_state.med;
        entry.local_pref = path_state.local_pref;
        entries.push(entry);
    }
    for entry in &mut entries {
        if entry.action == "withdraw" {
            entry.as_path.clear();
            entry.communities.clear();
            entry.med = None;
            entry.local_pref = None;
        }
    }
    entries.extend(path_state.mp_unreach);
    Ok(entries)
}

#[derive(Default)]
struct PathState {
    next_hop: String,
    as_path: Vec<u32>,
    communities: Vec<String>,
    med: Option<u32>,
    local_pref: Option<u32>,
    mp_reach: Vec<BmpRouteEntry>,
    mp_unreach: Vec<BmpRouteEntry>,
}

fn parse_path_attributes(attrs: &[u8], state: &mut PathState) -> Result<()> {
    let mut cursor = 0;
    while cursor < attrs.len() {
        if cursor + 3 > attrs.len() {
            bail!("truncated BGP path attribute header");
        }
        let flags = attrs[cursor];
        let code = attrs[cursor + 1];
        cursor += 2;
        let extended = flags & 0x10 != 0;
        let len = if extended {
            if cursor + 2 > attrs.len() {
                bail!("truncated extended path attribute length");
            }
            let len = u16::from_be_bytes([attrs[cursor], attrs[cursor + 1]]) as usize;
            cursor += 2;
            len
        } else {
            let len = attrs[cursor] as usize;
            cursor += 1;
            len
        };
        if cursor + len > attrs.len() {
            bail!("path attribute length exceeds payload");
        }
        let value = &attrs[cursor..cursor + len];
        cursor += len;
        match code {
            2 => state.as_path = parse_as_path(value),
            3 if value.len() == 4 => {
                state.next_hop = Ipv4Addr::new(value[0], value[1], value[2], value[3]).to_string()
            }
            4 if value.len() == 4 => {
                state.med = Some(u32::from_be_bytes(value.try_into().unwrap_or([0; 4])))
            }
            5 if value.len() == 4 => {
                state.local_pref = Some(u32::from_be_bytes(value.try_into().unwrap_or([0; 4])))
            }
            8 => state.communities = parse_communities(value),
            14 => parse_mp_reach(value, state)?,
            15 => parse_mp_unreach(value, state)?,
            _ => {}
        }
    }
    Ok(())
}

fn parse_as_path(value: &[u8]) -> Vec<u32> {
    let mut cursor = 0;
    let mut result = Vec::new();
    while cursor + 2 <= value.len() {
        let _segment_type = value[cursor];
        let count = value[cursor + 1] as usize;
        cursor += 2;
        let remaining = value.len().saturating_sub(cursor);
        let width = if remaining >= count * 4 { 4 } else { 2 };
        for _ in 0..count {
            if cursor + width > value.len() {
                return result;
            }
            let asn = if width == 4 {
                u32::from_be_bytes(value[cursor..cursor + 4].try_into().unwrap_or([0; 4]))
            } else {
                u16::from_be_bytes(value[cursor..cursor + 2].try_into().unwrap_or([0; 2])) as u32
            };
            result.push(asn);
            cursor += width;
        }
    }
    result
}

fn parse_communities(value: &[u8]) -> Vec<String> {
    value
        .chunks_exact(4)
        .map(|chunk| {
            let a = u16::from_be_bytes([chunk[0], chunk[1]]);
            let b = u16::from_be_bytes([chunk[2], chunk[3]]);
            format!("{a}:{b}")
        })
        .collect()
}

fn parse_mp_reach(value: &[u8], state: &mut PathState) -> Result<()> {
    if value.len() < 5 {
        bail!("MP_REACH_NLRI too short");
    }
    let afi = u16::from_be_bytes([value[0], value[1]]);
    let safi = value[2];
    let nh_len = value[3] as usize;
    if 4 + nh_len + 1 > value.len() {
        bail!("MP_REACH_NLRI next-hop length exceeds payload");
    }
    state.next_hop = parse_next_hop(afi, &value[4..4 + nh_len]);
    let nlri = &value[5 + nh_len..];
    state.mp_reach = parse_nlri(afi_safi_name(afi, safi), nlri, "announce")?
        .into_iter()
        .map(|mut entry| {
            entry.next_hop = state.next_hop.clone();
            entry
        })
        .collect();
    Ok(())
}

fn parse_mp_unreach(value: &[u8], state: &mut PathState) -> Result<()> {
    if value.len() < 3 {
        bail!("MP_UNREACH_NLRI too short");
    }
    let afi = u16::from_be_bytes([value[0], value[1]]);
    let safi = value[2];
    state.mp_unreach = parse_nlri(afi_safi_name(afi, safi), &value[3..], "withdraw")?;
    Ok(())
}

fn parse_next_hop(afi: u16, bytes: &[u8]) -> String {
    match (afi, bytes.len()) {
        (1, 4) => Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        (2, 16) => Ipv6Addr::from(<[u8; 16]>::try_from(bytes).unwrap_or([0; 16])).to_string(),
        _ => String::new(),
    }
}

fn afi_safi_name(afi: u16, safi: u8) -> &'static str {
    match (afi, safi) {
        (1, 1) => "ipv4-unicast",
        (2, 1) => "ipv6-unicast",
        _ => "unknown",
    }
}

fn parse_nlri(afi_safi: &str, mut bytes: &[u8], action: &str) -> Result<Vec<BmpRouteEntry>> {
    let mut entries = Vec::new();
    while !bytes.is_empty() {
        let prefix_len = bytes[0];
        let octets = (prefix_len as usize).div_ceil(8);
        bytes = &bytes[1..];
        if bytes.len() < octets {
            bail!("NLRI prefix length exceeds payload");
        }
        let prefix = match afi_safi {
            "ipv6-unicast" => {
                let mut addr = [0_u8; 16];
                addr[..octets].copy_from_slice(&bytes[..octets]);
                Ipv6Addr::from(addr).to_string()
            }
            _ => {
                let mut addr = [0_u8; 4];
                let width = octets.min(4);
                addr[..width].copy_from_slice(&bytes[..width]);
                Ipv4Addr::from(addr).to_string()
            }
        };
        entries.push(BmpRouteEntry {
            afi_safi: afi_safi.to_string(),
            prefix,
            prefix_len,
            action: action.to_string(),
            next_hop: String::new(),
            as_path: Vec::new(),
            communities: Vec::new(),
            med: None,
            local_pref: None,
        });
        bytes = &bytes[octets..];
    }
    Ok(entries)
}

#[derive(Clone)]
struct JsonLineArchive {
    file: Option<Arc<Mutex<tokio::fs::File>>>,
}

impl JsonLineArchive {
    async fn open(path: &str) -> Result<Self> {
        if path.trim().is_empty() {
            return Ok(Self { file: None });
        }
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create BMP archive directory {}", parent.display()))?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("open BMP archive {}", path))?;
        Ok(Self {
            file: Some(Arc::new(Mutex::new(file))),
        })
    }

    async fn append(&self, event: &BmpEvent) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let mut payload = serde_json::to_vec(event).context("serialize BMP event")?;
        payload.push(b'\n');
        let mut file = file.lock().await;
        file.write_all(&payload)
            .await
            .context("write BMP archive line")?;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct BmpTargetMap {
    entries: Vec<TargetEntry>,
}

#[derive(Clone)]
struct TargetEntry {
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

impl BmpTargetMap {
    fn new(targets: &[TargetConfig]) -> Self {
        let entries = targets
            .iter()
            .map(|target| TargetEntry {
                address: target
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&target.address)
                    .to_string(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                site: target.site.clone().unwrap_or_default(),
            })
            .collect();
        Self { entries }
    }

    fn resolve(&self, event: &BmpEvent) -> ResolvedTarget {
        let lookup = [
            &event.collector_peer,
            &event.router_address,
            &event.peer_address,
        ];
        for key in lookup {
            let ip = key.split(':').next().unwrap_or(key);
            if let Some(entry) = self.entries.iter().find(|entry| entry.address == ip) {
                return ResolvedTarget {
                    address: entry.address.clone(),
                    hostname: if entry.hostname.is_empty() {
                        ip.to_string()
                    } else {
                        entry.hostname.clone()
                    },
                    vendor: entry.vendor.clone(),
                    role: entry.role.clone(),
                    site: entry.site.clone(),
                };
            }
        }
        ResolvedTarget {
            address: event
                .collector_peer
                .split(':')
                .next()
                .unwrap_or(&event.collector_peer)
                .to_string(),
            hostname: event
                .collector_peer
                .split(':')
                .next()
                .unwrap_or(&event.collector_peer)
                .to_string(),
            vendor: String::new(),
            role: String::new(),
            site: String::new(),
        }
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bgp_update_extracts_announced_prefix_and_attrs() {
        // attr_len = 21 (0x15): AS_PATH(7) + NEXT_HOP(7) + LOCAL_PREF(7)
        let payload = [
            0x00, 0x00, // withdrawn len
            0x00, 0x15, // attr len = 21
            0x40, 0x02, 0x04, 0x02, 0x01, 0xfd, 0xe8, // AS_PATH 65000
            0x40, 0x03, 0x04, 192, 0, 2, 1, // NEXT_HOP
            0x40, 0x05, 0x04, 0x00, 0x00, 0x00, 0x64, // LOCAL_PREF 100
            24, 203, 0, 113, // 203.0.113.0/24
        ];
        let entries = parse_bgp_update(&payload).expect("parse BGP UPDATE");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prefix, "203.0.113.0");
        assert_eq!(entries[0].prefix_len, 24);
        assert_eq!(entries[0].next_hop, "192.0.2.1");
        assert_eq!(entries[0].as_path, vec![65000]);
        assert_eq!(entries[0].local_pref, Some(100));
    }

    #[test]
    fn parse_statistics_report_reads_4byte_and_8byte_counters() {
        let body = [
            0x00, 0x00, 0x00, 0x02, // count = 2
            // stat 0 (prefixes rejected): type=0, len=4, value=42
            0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x2a,
            // stat 7 (adj-rib-in routes): type=7, len=8, value=1000
            0x00, 0x07, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xe8,
        ];
        let stats = parse_statistics_report(&body);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].stat_type, 0);
        assert_eq!(stats[0].value, 42);
        assert_eq!(stats[1].stat_type, 7);
        assert_eq!(stats[1].value, 1000);
    }

    #[test]
    fn parse_peer_down_reason_names_code_3() {
        let body = [3u8, 0xff, 0x00]; // reason = 3 + garbage
        let (code, name) = parse_peer_down_reason(&body);
        assert_eq!(code, 3);
        assert_eq!(name, "remote_bgp_notification");
    }

    #[test]
    fn parse_initiation_extracts_sys_name_and_descr() {
        let sys_descr = b"Nokia SR Linux";
        let sys_name = b"srl-spine1";
        let mut payload = Vec::new();
        // TLV type=0 (sysDescr)
        payload.extend_from_slice(&[0x00, 0x00]);
        payload.extend_from_slice(&(sys_descr.len() as u16).to_be_bytes());
        payload.extend_from_slice(sys_descr);
        // TLV type=1 (sysName)
        payload.extend_from_slice(&[0x00, 0x01]);
        payload.extend_from_slice(&(sys_name.len() as u16).to_be_bytes());
        payload.extend_from_slice(sys_name);

        let event =
            parse_initiation(&payload, "10.0.0.1".to_string(), 0).expect("parse initiation");
        assert_eq!(event.sys_descr.as_deref(), Some("Nokia SR Linux"));
        assert_eq!(event.sys_name.as_deref(), Some("srl-spine1"));
        assert_eq!(event.message_type, "initiation");
    }

    #[test]
    fn parse_termination_extracts_reason_code() {
        let mut payload = Vec::new();
        // TLV type=1 (reason), len=2, code=0 (admin closed)
        payload.extend_from_slice(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x00]);
        let event =
            parse_termination(&payload, "10.0.0.1".to_string(), 0).expect("parse termination");
        assert_eq!(event.termination_reason, Some(0));
        assert_eq!(
            event.termination_reason_name.as_deref(),
            Some("session_admin_closed")
        );
    }

    #[test]
    fn classify_rib_type_pre_post_policy() {
        assert_eq!(classify_rib_type(0, 0x00), "adj-rib-in-pre-policy");
        assert_eq!(classify_rib_type(0, 0x40), "adj-rib-in-post-policy");
        assert_eq!(classify_rib_type(1, 0x00), "adj-rib-in-pre-policy");
        assert_eq!(classify_rib_type(2, 0x00), "adj-rib-out-pre-policy");
        assert_eq!(classify_rib_type(2, 0x40), "adj-rib-out-post-policy");
        assert_eq!(classify_rib_type(3, 0x00), "loc-rib");
        assert_eq!(classify_rib_type(3, 0x40), "loc-rib");
    }

    #[test]
    fn parse_initiation_extracts_admin_string_tlv() {
        let mut payload = Vec::new();
        // TLV type=0 (sysDescr)
        let descr = b"FRRouting/10.2.1";
        payload.extend_from_slice(&[0x00, 0x00]);
        payload.extend_from_slice(&(descr.len() as u16).to_be_bytes());
        payload.extend_from_slice(descr);
        // TLV type=1 (sysName)
        let name = b"frr-rr";
        payload.extend_from_slice(&[0x00, 0x01]);
        payload.extend_from_slice(&(name.len() as u16).to_be_bytes());
        payload.extend_from_slice(name);
        // TLV type=2 (admin string)
        let admin = b"lab bmp source";
        payload.extend_from_slice(&[0x00, 0x02]);
        payload.extend_from_slice(&(admin.len() as u16).to_be_bytes());
        payload.extend_from_slice(admin);

        let event =
            parse_initiation(&payload, "10.0.0.1".to_string(), 0).expect("parse initiation");
        assert_eq!(event.sys_descr.as_deref(), Some("FRRouting/10.2.1"));
        assert_eq!(event.sys_name.as_deref(), Some("frr-rr"));
        assert_eq!(event.init_admin_string.as_deref(), Some("lab bmp source"));
    }

    #[test]
    fn parse_termination_extracts_free_form_message() {
        let mut payload = Vec::new();
        // TLV type=0 (free-form string)
        let msg = b"shutting down BMP";
        payload.extend_from_slice(&[0x00, 0x00]);
        payload.extend_from_slice(&(msg.len() as u16).to_be_bytes());
        payload.extend_from_slice(msg);
        // TLV type=1 (reason code), len=2, code=1 (unspecified)
        payload.extend_from_slice(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x01]);

        let event =
            parse_termination(&payload, "10.0.0.1".to_string(), 0).expect("parse termination");
        assert_eq!(event.termination_message.as_deref(), Some("shutting down BMP"));
        assert_eq!(event.termination_reason, Some(1));
        assert_eq!(event.termination_reason_name.as_deref(), Some("unspecified"));
    }

    #[test]
    fn parse_peer_up_extracts_bgp_open_info() {
        // Build a PeerUp body: 20 bytes header + Sent OPEN + Received OPEN
        let mut body = Vec::new();
        // Local address: IPv4 10.9.0.8 (stored in last 4 bytes of 16)
        body.extend_from_slice(&[0u8; 12]);
        body.extend_from_slice(&[10, 9, 0, 8]);
        // local_port=179, remote_port=45678
        body.extend_from_slice(&179u16.to_be_bytes());
        body.extend_from_slice(&45678u16.to_be_bytes());

        // Build a minimal BGP OPEN message
        fn build_bgp_open(bgp_id: [u8; 4], hold_time: u16, asn: u16) -> Vec<u8> {
            // Optional params: capability param with 4-byte-as cap
            let cap_4byte_as = [0x41, 0x04, 0x00, 0x00, 0xfd, 0xe8]; // cap-code=65, len=4, AS=65000
            let opt_param = [0x02, cap_4byte_as.len() as u8]; // type=2 (capability), len
            let opt_params_len = (opt_param.len() + cap_4byte_as.len()) as u8;
            let open_len = 10 + opt_params_len as usize;
            let total_len = (BGP_HEADER_LEN + open_len) as u16;
            let mut msg = Vec::new();
            // BGP header: marker(16) + length(2) + type(1)
            msg.extend_from_slice(&[0xff; 16]);
            msg.extend_from_slice(&total_len.to_be_bytes());
            msg.push(1); // OPEN type
            // OPEN: version(1) + AS(2) + hold_time(2) + bgp_id(4) + opt_params_len(1)
            msg.push(4); // BGP version 4
            msg.extend_from_slice(&asn.to_be_bytes());
            msg.extend_from_slice(&hold_time.to_be_bytes());
            msg.extend_from_slice(&bgp_id);
            msg.push(opt_params_len);
            msg.extend_from_slice(&opt_param);
            msg.extend_from_slice(&cap_4byte_as);
            msg
        }

        let sent_open = build_bgp_open([10, 9, 0, 8], 90, 65900);
        let recv_open = build_bgp_open([10, 9, 0, 1], 180, 65900);
        body.extend_from_slice(&sent_open);
        body.extend_from_slice(&recv_open);

        let info = parse_peer_up(&body, 0x00).expect("should parse peer_up");
        assert_eq!(info.local_address, "10.9.0.8");
        assert_eq!(info.local_port, 179);
        assert_eq!(info.remote_port, 45678);
        assert_eq!(info.sent_hold_time, 90);
        assert_eq!(info.received_hold_time, 180);
        assert_eq!(info.sent_bgp_id, "10.9.0.8");
        assert_eq!(info.received_bgp_id, "10.9.0.1");
        assert!(info.sent_capabilities.contains(&"4-byte-as".to_string()));
        assert!(info.received_capabilities.contains(&"4-byte-as".to_string()));
    }

    #[test]
    fn capability_name_maps_known_codes() {
        assert_eq!(capability_name(1), "multiprotocol");
        assert_eq!(capability_name(65), "4-byte-as");
        assert_eq!(capability_name(69), "add-path");
        assert_eq!(capability_name(99), "cap-99");
    }
}

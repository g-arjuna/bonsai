use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use chrono::{TimeZone, Utc};
use crate::{Error, Result};
use super::types::*;
use super::path_status_tlv::{find_path_status_tlv, PATH_STATUS_TLV_TYPE};
use crate::bgp::types::{AfiSafi, BgpDecodeContext};
use crate::bgp::update::parse_bgp_update_with_context;
use crate::bgp::open::parse_bgp_open;

const BMP_VERSION: u8 = 3;
/// Minimum BMP header: version(1) + length(4) + type(1)
const BMP_HEADER_LEN: usize = 6;
/// Common peer header: peer_type(1) + peer_flags(1) + RD(8) + peer_addr(16) + peer_as(4) + bgp_id(4) + ts_secs(4) + ts_micros(4)
const PEER_HEADER_LEN: usize = 42;
/// Maximum BMP frame size (default; callers can override)
pub const DEFAULT_MAX_FRAME: u32 = 65535;

/// Validate a 6-byte BMP common header before allocating or reading the frame body.
/// Returns `(declared_frame_len, msg_type)`.
pub fn validate_bmp_header(hdr: &[u8], max_frame: u32) -> Result<(usize, BmpMsgType)> {
    if hdr.len() < BMP_HEADER_LEN {
        return Err(Error::UnexpectedEof {
            needed: BMP_HEADER_LEN,
            have: hdr.len(),
        });
    }
    let version = hdr[0];
    let msg_len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
    let msg_type = hdr[5];

    if version != BMP_VERSION {
        return Err(Error::InvalidBmpVersion(version));
    }
    if (msg_len as usize) < BMP_HEADER_LEN {
        return Err(Error::BmpParse(format!(
            "BMP declared length {msg_len} is smaller than {BMP_HEADER_LEN}-byte header"
        )));
    }
    if msg_len > max_frame {
        return Err(Error::FrameTooLarge(msg_len, max_frame));
    }
    let msg_type = BmpMsgType::try_from(msg_type)?;
    Ok((msg_len as usize, msg_type))
}

/// Read one complete BMP frame from an async stream with early 6-byte header validation,
/// bounded allocation (`<= max_frame`), and explicit header/body completion deadlines.
/// Returns `Ok(None)` only on a clean EOF before the first byte of a frame.
pub async fn read_bmp_frame<R>(
    reader: &mut R,
    max_frame: u32,
    header_timeout: std::time::Duration,
    body_timeout: std::time::Duration,
) -> Result<Option<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut first = [0u8; 1];
    match reader.read_exact(&mut first).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    }

    let mut rest_hdr = [0u8; BMP_HEADER_LEN - 1];
    match tokio::time::timeout(header_timeout, reader.read_exact(&mut rest_hdr)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(Error::UnexpectedEof {
                needed: BMP_HEADER_LEN,
                have: 1,
            });
        }
        Ok(Err(e)) => return Err(Error::Io(e)),
        Err(_) => {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "BMP header read timed out",
            )));
        }
    }

    let hdr = [
        first[0],
        rest_hdr[0],
        rest_hdr[1],
        rest_hdr[2],
        rest_hdr[3],
        rest_hdr[4],
    ];
    let (msg_len_usize, _) = validate_bmp_header(&hdr, max_frame)?;

    let mut frame = Vec::with_capacity(msg_len_usize);
    frame.extend_from_slice(&hdr);
    frame.resize(msg_len_usize, 0);

    if msg_len_usize > BMP_HEADER_LEN {
        match tokio::time::timeout(body_timeout, reader.read_exact(&mut frame[BMP_HEADER_LEN..]))
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(Error::UnexpectedEof {
                    needed: msg_len_usize,
                    have: BMP_HEADER_LEN,
                });
            }
            Ok(Err(e)) => return Err(Error::Io(e)),
            Err(_) => {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "BMP body read timed out",
                )));
            }
        }
    }

    Ok(Some(frame))
}

/// Parse a single BMP message from a byte slice.
/// Returns (BmpPayload, speaker_addr) — caller wraps into BmpMessage with id/received_at.
pub fn parse_bmp_message(
    buf: &[u8],
    speaker_addr: IpAddr,
    max_frame: u32,
) -> Result<BmpPayload> {
    parse_bmp_message_with_peer_context(buf, speaker_addr, max_frame, |_| {
        BgpDecodeContext::default()
    })
}

/// Parse a single BMP message with an explicit `BgpDecodeContext` (e.g. negotiated Add-Path families).
pub fn parse_bmp_message_with_context(
    buf: &[u8],
    speaker_addr: IpAddr,
    max_frame: u32,
    ctx: &BgpDecodeContext,
) -> Result<BmpPayload> {
    parse_bmp_message_with_peer_context(buf, speaker_addr, max_frame, |_| ctx.clone())
}

/// Parse a single BMP message resolving the `BgpDecodeContext` from the decoded `PeerHeader`.
pub fn parse_bmp_message_with_peer_context<F>(
    buf: &[u8],
    _speaker_addr: IpAddr,
    max_frame: u32,
    resolve_ctx: F,
) -> Result<BmpPayload>
where
    F: FnOnce(&PeerHeader) -> BgpDecodeContext,
{
    let (msg_len_usize, msg_type) = validate_bmp_header(buf, max_frame)?;
    if buf.len() < msg_len_usize {
        return Err(Error::UnexpectedEof {
            needed: msg_len_usize,
            have: buf.len(),
        });
    }
    if buf.len() > msg_len_usize {
        return Err(Error::BmpParse(format!(
            "BMP buffer length {} exceeds declared message length {}",
            buf.len(),
            msg_len_usize
        )));
    }

    let body = &buf[BMP_HEADER_LEN..msg_len_usize];

    match msg_type {
        BmpMsgType::Initiation    => parse_initiation(body),
        BmpMsgType::Termination   => parse_termination(body),
        BmpMsgType::PeerUp        => parse_peer_up(body),
        BmpMsgType::PeerDown      => parse_peer_down(body),
        BmpMsgType::RouteMonitoring => parse_route_monitoring(body, resolve_ctx),
        BmpMsgType::StatsReport   => parse_stats_report(body),
        BmpMsgType::RouteMirroring => parse_route_mirroring(body),
    }
}

// ─── Common peer header ───────────────────────────────────────────────────────

pub fn parse_peer_header(buf: &[u8]) -> Result<(PeerHeader, &[u8])> {
    if buf.len() < PEER_HEADER_LEN {
        return Err(Error::UnexpectedEof { needed: PEER_HEADER_LEN, have: buf.len() });
    }
    let peer_type  = PeerType::try_from(buf[0])?;
    let peer_flags = PeerFlags(buf[1]);
    let mut rd = [0u8; 8];
    rd.copy_from_slice(&buf[2..10]);
    let peer_address = if peer_type != PeerType::LocRib && peer_flags.ipv6() {
        let mut b = [0u8; 16];
        b.copy_from_slice(&buf[10..26]);
        IpAddr::V6(Ipv6Addr::from(b))
    } else {
        IpAddr::V4(Ipv4Addr::from([buf[22], buf[23], buf[24], buf[25]]))
    };
    let peer_as   = u32::from_be_bytes([buf[26], buf[27], buf[28], buf[29]]);
    let bgp_id    = Ipv4Addr::from([buf[30], buf[31], buf[32], buf[33]]);
    let ts_secs   = u32::from_be_bytes([buf[34], buf[35], buf[36], buf[37]]);
    let ts_micros = u32::from_be_bytes([buf[38], buf[39], buf[40], buf[41]]);
    let timestamp = Utc.timestamp_opt(ts_secs as i64, ts_micros * 1000)
        .single()
        .unwrap_or_else(Utc::now);

    let rib_type = RibType::from_peer(peer_type, peer_flags);
    let hdr = PeerHeader {
        peer_type,
        peer_flags,
        peer_distinguisher: rd,
        peer_address,
        peer_as,
        peer_bgp_id: bgp_id,
        timestamp,
        rib_type,
    };
    Ok((hdr, &buf[PEER_HEADER_LEN..]))
}

// ─── Message type parsers ─────────────────────────────────────────────────────

/// Decode Initiation Information TLVs (RFC 7854 §4.4), preserving unknown TLV types and raw bytes.
pub fn parse_initiation_tlvs(buf: &[u8]) -> Result<Vec<InitiationInfo>> {
    let mut infos = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        if pos + 4 > buf.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + 4,
                have: buf.len(),
            });
        }
        let tlv_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let tlv_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + tlv_len > buf.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + tlv_len,
                have: buf.len(),
            });
        }
        let raw = &buf[pos..pos + tlv_len];
        pos += tlv_len;
        let info = match tlv_type {
            0 => InitiationInfo::AdminLabel(String::from_utf8_lossy(raw).into_owned()),
            1 => InitiationInfo::SysDescr(String::from_utf8_lossy(raw).into_owned()),
            2 => InitiationInfo::SysName(String::from_utf8_lossy(raw).into_owned()),
            other => InitiationInfo::Unknown {
                tlv_type: other,
                hex: raw.iter().map(|b| format!("{b:02x}")).collect(),
            },
        };
        infos.push(info);
    }
    Ok(infos)
}

fn parse_initiation(buf: &[u8]) -> Result<BmpPayload> {
    let mut sys_name  = None;
    let mut sys_descr = None;
    let mut labels    = Vec::new();
    for info in parse_initiation_tlvs(buf)? {
        match info {
            InitiationInfo::AdminLabel(val) => labels.push(val),
            InitiationInfo::SysDescr(val)   => sys_descr = Some(val),
            InitiationInfo::SysName(val)    => sys_name = Some(val),
            InitiationInfo::Unknown { .. }  => {}
        }
    }
    Ok(BmpPayload::Initiation { sys_name, sys_descr, labels })
}

fn parse_termination(buf: &[u8]) -> Result<BmpPayload> {
    let mut reason_code = 0u16;
    let mut reason_text = None;
    let mut pos = 0;
    while pos < buf.len() {
        if pos + 4 > buf.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + 4,
                have: buf.len(),
            });
        }
        let tlv_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let tlv_len  = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + tlv_len > buf.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + tlv_len,
                have: buf.len(),
            });
        }
        match tlv_type {
            0 => {
                reason_text = Some(String::from_utf8_lossy(&buf[pos..pos + tlv_len]).to_string());
            }
            1 if tlv_len == 2 => {
                reason_code = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
            }
            1 => {
                return Err(Error::BmpParse(format!(
                    "invalid Termination Reason TLV length {tlv_len}, expected 2"
                )));
            }
            _ => {}
        }
        pos += tlv_len;
    }
    Ok(BmpPayload::Termination { reason_code, reason_text })
}

fn parse_peer_up(buf: &[u8]) -> Result<BmpPayload> {
    let (peer_header, rest) = parse_peer_header(buf)?;
    if rest.len() < 20 {
        return Err(Error::UnexpectedEof { needed: 20, have: rest.len() });
    }
    let local_addr = if peer_header.peer_flags.ipv6() {
        let mut b = [0u8; 16];
        b.copy_from_slice(&rest[..16]);
        IpAddr::V6(Ipv6Addr::from(b))
    } else {
        IpAddr::V4(Ipv4Addr::from([rest[12], rest[13], rest[14], rest[15]]))
    };
    let local_port  = u16::from_be_bytes([rest[16], rest[17]]);
    let remote_port = u16::from_be_bytes([rest[18], rest[19]]);
    let bgp_pdus    = &rest[20..];

    // Find boundary of sent OPEN: parse header to get length
    let sent_open = parse_bgp_open_pdu(bgp_pdus)?;
    let sent_len  = bgp_open_pdu_len(bgp_pdus);
    let recv_pdus = &bgp_pdus[sent_len..];
    let recv_open = parse_bgp_open_pdu(recv_pdus)?;
    let recv_len  = bgp_open_pdu_len(recv_pdus);

    // Optional PeerUp Information TLVs (RFC 7854 §4.6)
    let _ = parse_initiation_tlvs(&recv_pdus[recv_len..])?;

    Ok(BmpPayload::PeerUp(PeerUpMessage {
        peer_header,
        local_addr,
        local_port,
        remote_port,
        sent_open,
        recv_open,
    }))
}

fn bgp_open_pdu_len(buf: &[u8]) -> usize {
    if buf.len() < 19 { return buf.len(); }
    // BGP common header = marker(16) + length(2) + type(1)
    u16::from_be_bytes([buf[16], buf[17]]) as usize
}

fn parse_bgp_open_pdu(buf: &[u8]) -> Result<BgpOpenInfo> {
    if buf.len() < 19 {
        return Err(Error::UnexpectedEof { needed: 19, have: buf.len() });
    }
    let msg_len = u16::from_be_bytes([buf[16], buf[17]]) as usize;
    if !(29..=4096).contains(&msg_len) {
        return Err(Error::BgpParse(format!(
            "invalid BGP OPEN declared length {msg_len} (must be 29..=4096)"
        )));
    }
    if buf.len() < msg_len {
        return Err(Error::UnexpectedEof { needed: msg_len, have: buf.len() });
    }
    parse_bgp_open(&buf[..msg_len])
}

fn parse_peer_down(buf: &[u8]) -> Result<BmpPayload> {
    let (peer_header, rest) = parse_peer_header(buf)?;
    if rest.is_empty() {
        return Err(Error::UnexpectedEof { needed: 1, have: 0 });
    }
    let reason_code = rest[0];
    let reason = match reason_code {
        1 => PeerDownReason::LocalSystemClosed  { notification: Some(rest[1..].to_vec()) },
        2 => PeerDownReason::LocalSystemClosed2,
        3 => PeerDownReason::RemoteSystemClosed { notification: Some(rest[1..].to_vec()) },
        4 => PeerDownReason::RemoteSystemClosed2,
        5 => PeerDownReason::PeerDeConfigured,
        6 => PeerDownReason::VrfDown,
        _ => PeerDownReason::Unknown(reason_code),
    };
    Ok(BmpPayload::PeerDown { peer_header, reason })
}

fn parse_route_monitoring<F>(buf: &[u8], resolve_ctx: F) -> Result<BmpPayload>
where
    F: FnOnce(&PeerHeader) -> BgpDecodeContext,
{
    let (peer_header, rest) = parse_peer_header(buf)?;
    if rest.len() < 19 {
        return Err(Error::UnexpectedEof { needed: 19, have: rest.len() });
    }

    // RV7-P2: Scan leading TLVs for Path Status TLV before the BGP PDU.
    // The BGP PDU starts with the 16-byte marker (all 0xFF), so we scan
    // any leading bytes for TLV structures until we hit the BGP marker.
    // This handles implementations that prepend BMP TLVs before the BGP UPDATE.
    let path_status = find_path_status_tlv(rest, PATH_STATUS_TLV_TYPE);
    let mut ctx = resolve_ctx(&peer_header);
    // RFC 7854 §4.2: A-flag (0x20) in Per-Peer Header specifies whether AS_PATH in this
    // RouteMonitoring message uses legacy 2-byte ASNs (as2() == true) or 4-byte ASNs (as2() == false).
    ctx.four_byte_asn = !peer_header.peer_flags.as2();

    let mut update = parse_bgp_update_with_context(rest, &ctx)?;
    if let Some(ps) = path_status {
        update.attributes.path_status = Some(ps);
    }

    Ok(BmpPayload::RouteMonitoring { peer_header, update })
}

fn parse_stats_report(buf: &[u8]) -> Result<BmpPayload> {
    let (peer_header, rest) = parse_peer_header(buf)?;
    if rest.len() < 4 {
        return Err(Error::UnexpectedEof { needed: 4, have: rest.len() });
    }
    let stat_count = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    let max_possible = rest.len().saturating_sub(4) / 4;
    if stat_count > max_possible {
        return Err(Error::UnexpectedEof {
            needed: 4 + stat_count.saturating_mul(4),
            have: rest.len(),
        });
    }
    let mut stats = Vec::with_capacity(stat_count.min(256));
    let mut pos = 4;
    for _ in 0..stat_count {
        if pos + 4 > rest.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + 4,
                have: rest.len(),
            });
        }
        let stat_type = u16::from_be_bytes([rest[pos], rest[pos + 1]]);
        let stat_len  = u16::from_be_bytes([rest[pos + 2], rest[pos + 3]]) as usize;
        pos += 4;
        if pos + stat_len > rest.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + stat_len,
                have: rest.len(),
            });
        }

        let entry = if stat_is_per_afi_safi_11byte(stat_type) && stat_len == 11 {
            // RFC 9972: AFI(2) + SAFI(1) + 64-bit Gauge(8)
            let afi  = u16::from_be_bytes([rest[pos], rest[pos + 1]]);
            let safi = rest[pos + 2];
            let val  = u64::from_be_bytes(rest[pos + 3..pos + 11].try_into().unwrap());
            StatEntry {
                stat_type,
                name:     stat_name(stat_type).to_string(),
                value:    val,
                afi_safi: Some(AfiSafi::new(afi, safi)),
            }
        } else if stat_is_per_afi_safi_7byte(stat_type) && stat_len == 7 {
            // RFC 7854: AFI(2) + SAFI(1) + 32-bit Counter(4)
            let afi  = u16::from_be_bytes([rest[pos], rest[pos + 1]]);
            let safi = rest[pos + 2];
            let val  = u32::from_be_bytes(rest[pos + 3..pos + 7].try_into().unwrap()) as u64;
            StatEntry {
                stat_type,
                name:     stat_name(stat_type).to_string(),
                value:    val,
                afi_safi: Some(AfiSafi::new(afi, safi)),
            }
        } else {
            let value: u64 = match stat_len {
                4 => u32::from_be_bytes(rest[pos..pos + 4].try_into().unwrap()) as u64,
                8 => u64::from_be_bytes(rest[pos..pos + 8].try_into().unwrap()),
                _ => 0,
            };
            StatEntry { stat_type, name: stat_name(stat_type).to_string(), value, afi_safi: None }
        };

        pos += stat_len;
        stats.push(entry);
    }
    if pos != rest.len() {
        return Err(Error::BmpParse(format!(
            "BMP StatsReport has {} trailing unparsed bytes",
            rest.len() - pos
        )));
    }
    Ok(BmpPayload::StatsReport { peer_header, stats })
}

fn parse_route_mirroring(buf: &[u8]) -> Result<BmpPayload> {
    let (peer_header, rest) = parse_peer_header(buf)?;
    // TLV type 0 = BGP Message PDU
    let mut pdu = Vec::new();
    let mut pos = 0;
    while pos < rest.len() {
        if pos + 4 > rest.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + 4,
                have: rest.len(),
            });
        }
        let tlv_type = u16::from_be_bytes([rest[pos], rest[pos + 1]]);
        let tlv_len  = u16::from_be_bytes([rest[pos + 2], rest[pos + 3]]) as usize;
        pos += 4;
        if pos + tlv_len > rest.len() {
            return Err(Error::UnexpectedEof {
                needed: pos + tlv_len,
                have: rest.len(),
            });
        }
        if tlv_type == 0 {
            pdu = rest[pos..pos + tlv_len].to_vec();
        }
        pos += tlv_len;
    }
    Ok(BmpPayload::RouteMirroring { peer_header, pdu })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn speaker() -> IpAddr { "10.0.0.1".parse().unwrap() }

    #[test]
    fn test_parse_initiation() {
        // BMP Initiation: version=3, len=20, type=4, TLV type=2 (sysName), TLV type=1 (sysDescr)
        let mut msg = vec![3u8, 0, 0, 0, 20, 4];
        // TLV: type=2 (sysName), len=4, "test"
        msg.extend_from_slice(&[0, 2, 0, 4]);
        msg.extend_from_slice(b"test");
        // TLV: type=1 (sysDescr), len=2, "hw"
        msg.extend_from_slice(&[0, 1, 0, 2]);
        msg.extend_from_slice(b"hw");
        let payload = parse_bmp_message(&msg, speaker(), DEFAULT_MAX_FRAME).unwrap();
        match payload {
            BmpPayload::Initiation { sys_name, sys_descr, .. } => {
                assert_eq!(sys_name, Some("test".to_string()));
                assert_eq!(sys_descr, Some("hw".to_string()));
            }
            _ => panic!("expected Initiation"),
        }
    }

    #[test]
    fn test_invalid_version() {
        let msg = vec![2u8, 0, 0, 0, 6, 4];
        assert!(matches!(
            parse_bmp_message(&msg, speaker(), DEFAULT_MAX_FRAME),
            Err(Error::InvalidBmpVersion(2))
        ));
    }

    #[test]
    fn test_frame_too_large() {
        let mut msg = vec![3u8, 0, 0, 0xFF, 0xFF, 4]; // len = 65535
        msg.resize(1001, 0);
        assert!(matches!(
            parse_bmp_message(&msg, speaker(), 1000),
            Err(Error::FrameTooLarge(65535, 1000))
        ));
    }
}


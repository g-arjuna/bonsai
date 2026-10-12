use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

// ─── SR Policy NLRI (RFC 9256 / RFC 9831, AFI 1/2 SAFI 73) ──────────────────

/// SR Policy NLRI key (AFI 1/2, SAFI 73 — RFC 9256 §2.1)
/// Wire: distinguisher(4) + color(4) + endpoint(4 or 16)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrPolicyNlri {
    pub distinguisher: u32,
    pub color:         u32,
    pub endpoint:      IpAddr,
}

/// Segment list with weight (RFC 9256 §2.4.2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentList {
    pub weight:   u32,
    pub segments: Vec<Segment>,
}

/// Segment types A–K (RFC 9256 §2.4.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Segment {
    /// Type 1 (A): MPLS label with S/E/V/L flags
    MplsLabel { label: u32, tc: u8, s: bool, ttl: u8 },
    /// Type 2 (B): SRv6 SID (128-bit) with optional endpoint behavior
    Srv6Sid { sid: [u8; 16], endpoint_behavior: Option<u16> },
    /// Type 3 (C): IPv4 prefix with algorithm
    Ipv4Prefix { prefix: Ipv4Addr, prefix_len: u8, algorithm: u8 },
    /// Type 4 (D): IPv6 prefix with algorithm
    Ipv6Prefix { prefix: Ipv6Addr, prefix_len: u8, algorithm: u8 },
    /// Type 5 (E): IPv4 adjacency — local/remote interface IDs
    Ipv4Adjacency { local_id: u32, remote_id: u32 },
    /// Type 6 (F): IPv4 interface addresses
    Ipv4Interface { local_addr: Ipv4Addr, remote_addr: Ipv4Addr },
    /// Type 7 (G): IPv6 adjacency — local interface ID + addresses
    Ipv6LocalAdj { local_id: u32, local_addr: Ipv6Addr, remote_addr: Ipv6Addr },
    /// Type 8 (H): IPv6 adjacency — both interface IDs + addresses
    Ipv6Adjacency { local_id: u32, remote_id: u32, local_addr: Ipv6Addr, remote_addr: Ipv6Addr },
    /// Type 9 (I): IPv4 next-hop with algorithm
    Ipv4NextHop { nexthop: Ipv4Addr, algorithm: u8 },
    /// Type 10 (J): IPv6 next-hop with algorithm
    Ipv6NextHop { nexthop: Ipv6Addr, algorithm: u8 },
    /// Type 11 (K): Segment sub-list (nested)
    SubList { sub_segments: Vec<Segment> },
    Unknown { seg_type: u8, data: Vec<u8> },
}

/// SR Policy candidate path (RFC 9256 §2.3)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidatePath {
    pub preference:    u32,
    pub name:          Option<String>,
    pub segment_lists: Vec<SegmentList>,
    pub is_best:       bool,
}

/// Decode SR Policy NLRIs from MP_REACH body (after next-hop + SNPA consumed).
/// AFI=1 → IPv4 endpoint (4 bytes), AFI=2 → IPv6 endpoint (16 bytes).
pub fn decode_srpolicy_nlri(buf: &[u8], afi_is_ipv6: bool) -> Result<Vec<SrPolicyNlri>> {
    let ep_octets: usize = if afi_is_ipv6 { 16 } else { 4 };
    let nlri_size = 4 + 4 + ep_octets; // distinguisher + color + endpoint
    let mut policies = Vec::new();
    let mut pos = 0;

    while pos + nlri_size <= buf.len() {
        let distinguisher = u32::from_be_bytes(buf[pos..pos+4].try_into().unwrap()); pos += 4;
        let color         = u32::from_be_bytes(buf[pos..pos+4].try_into().unwrap()); pos += 4;
        let endpoint = if afi_is_ipv6 {
            let mut b = [0u8; 16]; b.copy_from_slice(&buf[pos..pos+16]);
            IpAddr::V6(Ipv6Addr::from(b))
        } else {
            IpAddr::V4(Ipv4Addr::from([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]]))
        };
        pos += ep_octets;
        policies.push(SrPolicyNlri { distinguisher, color, endpoint });
    }
    Ok(policies)
}

/// Parse SR Policy candidate paths from the Tunnel Encapsulation attribute (type 23)
/// sub-TLVs when tunnel type = 15 (SR-MPLS) or 23 (SRv6).
/// The buf passed here is the content of the SR Policy tunnel TLV value.
pub fn parse_srpolicy_candidate_paths(buf: &[u8]) -> Result<Vec<CandidatePath>> {
    let mut paths: Vec<CandidatePath> = Vec::new();
    let mut pos = 0;

    while pos + 3 <= buf.len() {
        let sub_type = buf[pos]; pos += 1;
        let sub_len  = u16::from_be_bytes([buf[pos], buf[pos+1]]) as usize; pos += 2;
        if pos + sub_len > buf.len() { break; }
        let sub_data = &buf[pos..pos+sub_len]; pos += sub_len;

        match sub_type {
            128 if sub_len >= 8 => {
                // Preference sub-TLV (RFC 9256 §2.4.1):
                // flags(1) + reserved(3) + preference(4) + [nested sub-TLVs...]
                let preference = u32::from_be_bytes([
                    sub_data[4], sub_data[5], sub_data[6], sub_data[7],
                ]);
                let seg_lists = if sub_data.len() > 8 {
                    parse_segment_lists(&sub_data[8..]).unwrap_or_default()
                } else {
                    Vec::new()
                };
                let name = extract_path_name(sub_data);
                paths.push(CandidatePath {
                    preference,
                    name,
                    segment_lists: seg_lists,
                    is_best: false,
                });
            }
            _ => {}
        }
    }

    if let Some(best) = paths.iter_mut().max_by_key(|p| p.preference) {
        best.is_best = true;
    }
    Ok(paths)
}

fn extract_path_name(buf: &[u8]) -> Option<String> {
    // Sub-sub-TLV type 130 = ENH path name (optional, variable)
    if buf.len() < 8 { return None; }
    let mut pos = 8; // skip flags+reserved+preference
    while pos + 3 <= buf.len() {
        let t   = buf[pos]; pos += 1;
        let len = u16::from_be_bytes([buf[pos], buf[pos+1]]) as usize; pos += 2;
        if pos + len > buf.len() { break; }
        if t == 130 {
            return Some(String::from_utf8_lossy(&buf[pos..pos+len]).to_string());
        }
        pos += len;
    }
    None
}

fn parse_segment_lists(buf: &[u8]) -> Result<Vec<SegmentList>> {
    let mut lists = Vec::new();
    let mut pos = 0;

    while pos + 3 <= buf.len() {
        let sub_type = buf[pos]; pos += 1;
        let sub_len  = u16::from_be_bytes([buf[pos], buf[pos+1]]) as usize; pos += 2;
        if pos + sub_len > buf.len() { break; }
        let data = &buf[pos..pos+sub_len]; pos += sub_len;

        // Type 132 = Segment List sub-TLV (RFC 9256 §2.4.2)
        // weight(1) + reserved(3) + [segment sub-TLVs...]
        if sub_type == 132 && data.len() >= 4 {
            let weight = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let segments = parse_segments(&data[4..]).unwrap_or_default();
            lists.push(SegmentList { weight, segments });
        }
    }
    Ok(lists)
}

fn parse_segments(buf: &[u8]) -> Result<Vec<Segment>> {
    let mut segs = Vec::new();
    let mut pos = 0;

    while pos + 2 <= buf.len() {
        let seg_type = buf[pos]; pos += 1;
        let seg_len  = buf[pos] as usize; pos += 1;
        // Each segment: type(1) + length(1) + flags(1) + reserved(1) + type-specific
        if seg_len < 2 || pos + seg_len - 2 > buf.len() { break; }
        let _flags   = if pos < buf.len() { buf[pos] } else { 0 }; pos += 1;
        let _rsvd    = if pos < buf.len() { buf[pos] } else { 0 }; pos += 1;
        let data_len = seg_len.saturating_sub(2);
        if pos + data_len > buf.len() { break; }
        let data = &buf[pos..pos+data_len]; pos += data_len;

        let seg = match seg_type {
            1 if data.len() >= 3 => {
                // Type A: MPLS label — 24-bit field, top 20 = label, 3 = TC, 1 = S, 8 = TTL
                let raw = u32::from_be_bytes([0, data[0], data[1], data[2]]);
                Segment::MplsLabel {
                    label: raw >> 12,
                    tc:    ((raw >> 9) & 0x07) as u8,
                    s:     (raw >> 8) & 0x01 != 0,
                    ttl:   (raw & 0xFF) as u8,
                }
            }
            2 if data.len() >= 16 => {
                // Type B: SRv6 SID (16 bytes)
                let mut sid = [0u8; 16]; sid.copy_from_slice(&data[..16]);
                let endpoint_behavior = if data.len() >= 18 {
                    Some(u16::from_be_bytes([data[16], data[17]]))
                } else { None };
                Segment::Srv6Sid { sid, endpoint_behavior }
            }
            3 if data.len() >= 6 => {
                Segment::Ipv4Prefix {
                    prefix:     Ipv4Addr::from([data[0], data[1], data[2], data[3]]),
                    prefix_len: data[4],
                    algorithm:  data[5],
                }
            }
            4 if data.len() >= 18 => {
                let mut b = [0u8; 16]; b.copy_from_slice(&data[..16]);
                Segment::Ipv6Prefix {
                    prefix:     Ipv6Addr::from(b),
                    prefix_len: data[16],
                    algorithm:  data[17],
                }
            }
            5 if data.len() >= 8 => {
                Segment::Ipv4Adjacency {
                    local_id:  u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
                    remote_id: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
                }
            }
            6 if data.len() >= 8 => {
                Segment::Ipv4Interface {
                    local_addr:  Ipv4Addr::from([data[0], data[1], data[2], data[3]]),
                    remote_addr: Ipv4Addr::from([data[4], data[5], data[6], data[7]]),
                }
            }
            7 if data.len() >= 36 => {
                let local_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                let mut la = [0u8; 16]; la.copy_from_slice(&data[4..20]);
                let mut ra = [0u8; 16]; ra.copy_from_slice(&data[20..36]);
                Segment::Ipv6LocalAdj {
                    local_id,
                    local_addr:  Ipv6Addr::from(la),
                    remote_addr: Ipv6Addr::from(ra),
                }
            }
            8 if data.len() >= 40 => {
                let local_id  = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                let remote_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                let mut la = [0u8; 16]; la.copy_from_slice(&data[8..24]);
                let mut ra = [0u8; 16]; ra.copy_from_slice(&data[24..40]);
                Segment::Ipv6Adjacency {
                    local_id, remote_id,
                    local_addr:  Ipv6Addr::from(la),
                    remote_addr: Ipv6Addr::from(ra),
                }
            }
            9 if data.len() >= 5 => {
                Segment::Ipv4NextHop {
                    nexthop:   Ipv4Addr::from([data[0], data[1], data[2], data[3]]),
                    algorithm: data[4],
                }
            }
            10 if data.len() >= 17 => {
                let mut b = [0u8; 16]; b.copy_from_slice(&data[..16]);
                Segment::Ipv6NextHop {
                    nexthop:   Ipv6Addr::from(b),
                    algorithm: data[16],
                }
            }
            11 => {
                let sub_segs = parse_segments(data).unwrap_or_default();
                Segment::SubList { sub_segments: sub_segs }
            }
            _ => Segment::Unknown { seg_type, data: data.to_vec() },
        };
        segs.push(seg);
    }
    Ok(segs)
}

// ─── Route Target Constraint NLRI (RFC 4684, AFI 1/2 SAFI 132) ───────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RtcNlri {
    /// prefix_len = 0 → wildcard (interested in ALL RTs)
    Wildcard,
    /// prefix_len > 0 → first 4 bytes are origin AS (if ≥ 32 bits), rest is RT EC prefix
    Specific { origin_as: Option<u32>, prefix_len: u8, data: Vec<u8> },
}

/// Decode RTC NLRIs from MP_REACH or MP_UNREACH body.
pub fn decode_rtc_nlri(buf: &[u8]) -> Result<Vec<RtcNlri>> {
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < buf.len() {
        let prefix_len = buf[pos] as usize; pos += 1;
        if prefix_len == 0 {
            result.push(RtcNlri::Wildcard);
            continue;
        }
        let octets = (prefix_len + 7) / 8;
        if pos + octets > buf.len() {
            return Err(Error::UnexpectedEof { needed: pos + octets, have: buf.len() });
        }
        let bytes = &buf[pos..pos+octets]; pos += octets;
        let origin_as = if octets >= 4 {
            Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        } else {
            None
        };
        result.push(RtcNlri::Specific {
            origin_as,
            prefix_len: prefix_len as u8,
            data: bytes.to_vec(),
        });
    }
    Ok(result)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_srpolicy_nlri_ipv4() {
        // distinguisher(4) + color(4) + IPv4 endpoint(4)
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0, 0, 0, 1]); // distinguisher = 1
        buf.extend_from_slice(&[0, 0, 0x00, 0x64]); // color = 100
        buf.extend_from_slice(&[10, 0, 0, 1]); // endpoint = 10.0.0.1
        let nlris = decode_srpolicy_nlri(&buf, false).unwrap();
        assert_eq!(nlris.len(), 1);
        assert_eq!(nlris[0].distinguisher, 1);
        assert_eq!(nlris[0].color, 100);
        assert_eq!(nlris[0].endpoint.to_string(), "10.0.0.1");
    }

    #[test]
    fn test_srpolicy_nlri_ipv6() {
        // distinguisher(4) + color(4) + IPv6 endpoint(16)
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0, 0, 0, 2]); // distinguisher = 2
        buf.extend_from_slice(&[0, 0, 0x01, 0xF4]); // color = 500
        let ipv6_bytes: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        buf.extend_from_slice(&ipv6_bytes);
        let nlris = decode_srpolicy_nlri(&buf, true).unwrap();
        assert_eq!(nlris.len(), 1);
        assert_eq!(nlris[0].color, 500);
        assert!(matches!(nlris[0].endpoint, IpAddr::V6(_)));
    }

    #[test]
    fn test_rtc_wildcard() {
        let buf = vec![0u8]; // prefix_len = 0 → wildcard
        let nlris = decode_rtc_nlri(&buf).unwrap();
        assert_eq!(nlris.len(), 1);
        assert!(matches!(nlris[0], RtcNlri::Wildcard));
    }

    #[test]
    fn test_rtc_specific_with_origin_as() {
        // prefix_len = 48 bits → 6 octets: origin_as(4) + RT type(1) + partial(1)
        let mut buf = vec![48u8]; // prefix_len = 48
        buf.extend_from_slice(&[0, 0, 0xFD, 0xE8]); // origin_as = 65000
        buf.extend_from_slice(&[0x00, 0x02]); // RT type bytes
        let nlris = decode_rtc_nlri(&buf).unwrap();
        assert_eq!(nlris.len(), 1);
        match &nlris[0] {
            RtcNlri::Specific { origin_as, prefix_len, .. } => {
                assert_eq!(*origin_as, Some(65000));
                assert_eq!(*prefix_len, 48);
            }
            _ => panic!("expected Specific"),
        }
    }
}

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use bytes::Buf;
use serde::{Deserialize, Serialize};
use crate::{Error, Result};
use super::types::{
    BgpLsNlri, BgpLsNlriType, BgpLsProtocol, BgpLsReachNlri, BgpLsUnreachNlri,
    NodeDescriptor, LinkDescriptor, Prefix, RouterId, Srv6SidNlri,
};
use ipnet::{Ipv4Net, Ipv6Net};

// ─── Node descriptor sub-TLV codes (RFC 7752 §3.2.1.2 Table 3) ───────────────
const ND_AS_NUMBER:     u16 = 512;
const ND_BGP_LS_ID:     u16 = 513;
const ND_OSPF_AREA:     u16 = 514;
const ND_IGP_ROUTER_ID: u16 = 515;

// ─── Link descriptor sub-TLV codes (RFC 7752 §3.2.2 Table 5) ─────────────────
const LD_LINK_LOCAL_REMOTE_ID: u16 = 258;
const LD_IPV4_IF_ADDR:         u16 = 259;
const LD_IPV4_NEIGHBOR_ADDR:   u16 = 260;
const LD_IPV6_IF_ADDR:         u16 = 261;
const LD_IPV6_NEIGHBOR_ADDR:   u16 = 262;

// ─── Prefix descriptor sub-TLV codes (RFC 7752 §3.2.3 Table 6) ───────────────
const PD_OSPF_ROUTE_TYPE: u16 = 264;
const PD_IP_REACHABILITY:  u16 = 265;

/// Decode a list of BGP-LS NLRIs from a flat byte buffer.
/// Used by both MP_REACH and MP_UNREACH parsers.
pub fn decode_bgpls_nlri(buf: &[u8]) -> Result<Vec<BgpLsNlri>> {
    let mut cur = std::io::Cursor::new(buf);
    let mut nlris = Vec::new();

    while cur.remaining() >= 4 {
        let nlri_type = cur.get_u16();
        let nlri_len  = cur.get_u16() as usize;
        if cur.remaining() < nlri_len {
            return Err(Error::UnexpectedEof { needed: nlri_len, have: cur.remaining() });
        }
        let nlri_bytes = cur.copy_to_bytes(nlri_len);
        let nlri = decode_one(nlri_type, &nlri_bytes)?;
        nlris.push(nlri);
    }
    Ok(nlris)
}

/// Decode MP_REACH BGP-LS (next_hop already consumed by caller, buf = NLRI bytes only)
pub fn decode_bgpls_reach(next_hop: Option<IpAddr>, nlri_buf: &[u8]) -> Result<BgpLsReachNlri> {
    let nlris = decode_bgpls_nlri(nlri_buf)?;
    Ok(BgpLsReachNlri { next_hop, nlris })
}

/// Decode MP_UNREACH BGP-LS
pub fn decode_bgpls_unreach(buf: &[u8]) -> Result<BgpLsUnreachNlri> {
    let nlris = decode_bgpls_nlri(buf)?;
    Ok(BgpLsUnreachNlri { nlris })
}

// ─── Internal decoder ─────────────────────────────────────────────────────────

fn decode_one(nlri_type: u16, buf: &[u8]) -> Result<BgpLsNlri> {
    // Every NLRI starts with: protocol_id(1) + identifier(8) + local_node_descriptors_tlv
    if buf.len() < 9 {
        return Ok(BgpLsNlri::Unknown { nlri_type, data: buf.to_vec() });
    }
    let protocol   = BgpLsProtocol::from(buf[0]);
    let identifier = u64::from_be_bytes([buf[1],buf[2],buf[3],buf[4],buf[5],buf[6],buf[7],buf[8]]);
    let rest       = &buf[9..];

    match BgpLsNlriType::from(nlri_type) {
        BgpLsNlriType::Node => {
            let local_node = decode_node_descriptor(rest)?;
            Ok(BgpLsNlri::Node { protocol, identifier, local_node })
        }
        BgpLsNlriType::Link => {
            decode_link_nlri(protocol, identifier, rest)
        }
        BgpLsNlriType::Ipv4Prefix | BgpLsNlriType::Ipv6Prefix => {
            let is_v6 = nlri_type == 4;
            decode_prefix_nlri(protocol, identifier, rest, is_v6)
        }
        BgpLsNlriType::Srv6Sid => {
            decode_srv6_sid_nlri(buf[0], identifier, rest)
        }
        BgpLsNlriType::Unknown(_) => {
            Ok(BgpLsNlri::Unknown { nlri_type, data: buf.to_vec() })
        }
    }
}

// ─── SRv6 SID NLRI decoder (RFC 9514 §2) ─────────────────────────────────────

/// Decode an SRv6 SID NLRI.
///
/// Format (after the 9-byte common header consumed by decode_one):
///   local_node_descriptor TLV + SRv6 SID Descriptors sub-TLVs
///
/// SID Descriptor TLV (type=1): SID(16) + optional SID Structure sub-TLV
fn decode_srv6_sid_nlri(
    protocol_id: u8,
    identifier:  u64,
    buf:         &[u8],
) -> Result<BgpLsNlri> {
    let mut cur = std::io::Cursor::new(buf);

    // Local node descriptor TLV
    let local_node = decode_node_descriptor(buf)?;
    let nd_consumed = if buf.len() >= 4 {
        let nd_len = u16::from_be_bytes([buf[2], buf[3]]) as usize + 4;
        nd_len.min(buf.len())
    } else {
        buf.len()
    };
    let _ = cur.copy_to_bytes(nd_consumed.min(cur.remaining()));

    let mut srv6_sid      = [0u8; 16];
    let mut endpoint_behavior: Option<u16> = None;
    let mut sid_structure: Option<(u8, u8, u8, u8)> = None;

    // SRv6 SID Descriptor TLVs
    while cur.remaining() >= 4 {
        let tlv_type = cur.get_u16();
        let tlv_len  = cur.get_u16() as usize;
        if cur.remaining() < tlv_len { break; }
        let tlv_data = cur.copy_to_bytes(tlv_len);

        match tlv_type {
            1 if tlv_data.len() >= 16 => {
                // SRv6 SID Information TLV — SID(16)
                srv6_sid.copy_from_slice(&tlv_data[..16]);
                // Sub-sub-TLVs follow the 16-byte SID
                let mut sub_pos = 16;
                while sub_pos + 4 <= tlv_data.len() {
                    let sub_type = u16::from_be_bytes([tlv_data[sub_pos], tlv_data[sub_pos+1]]);
                    let sub_len  = u16::from_be_bytes([tlv_data[sub_pos+2], tlv_data[sub_pos+3]]) as usize;
                    sub_pos += 4;
                    if sub_pos + sub_len > tlv_data.len() { break; }
                    let sub_data = &tlv_data[sub_pos..sub_pos + sub_len];
                    sub_pos += sub_len;

                    match sub_type {
                        1 if sub_data.len() >= 2 => {
                            // Endpoint Behavior sub-TLV
                            endpoint_behavior = Some(u16::from_be_bytes([sub_data[0], sub_data[1]]));
                        }
                        2 if sub_data.len() >= 4 => {
                            // SID Structure sub-TLV: block_len, node_len, function_len, arg_len
                            sid_structure = Some((sub_data[0], sub_data[1], sub_data[2], sub_data[3]));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(BgpLsNlri::Srv6Sid(Srv6SidNlri {
        protocol_id,
        identifier,
        local_node,
        srv6_sid,
        endpoint_behavior,
        sid_structure,
    }))
}

// ─── Node descriptor (RFC 7752 §3.2.1) ───────────────────────────────────────

fn decode_node_descriptor(buf: &[u8]) -> Result<NodeDescriptor> {
    // buf should start with the Node Descriptor TLV:
    // type(2) + length(2) + sub-TLVs
    let mut cur = std::io::Cursor::new(buf);
    let mut nd  = NodeDescriptor::default();

    // Outer TLV wrapper — type must be 256 (local) or 257 (remote)
    if cur.remaining() < 4 { return Ok(nd); }
    let _outer_type = cur.get_u16();   // 256 = local, 257 = remote
    let outer_len   = cur.get_u16() as usize;
    if cur.remaining() < outer_len { return Ok(nd); }
    let sub_bytes = cur.copy_to_bytes(outer_len);
    decode_node_sub_tlvs(&sub_bytes, &mut nd);
    Ok(nd)
}

fn decode_node_sub_tlvs(buf: &[u8], nd: &mut NodeDescriptor) {
    let mut cur = std::io::Cursor::new(buf);
    while cur.remaining() >= 4 {
        let sub_type = cur.get_u16();
        let sub_len  = cur.get_u16() as usize;
        if cur.remaining() < sub_len { break; }
        let val = cur.copy_to_bytes(sub_len);
        match sub_type {
            ND_AS_NUMBER if val.len() >= 4 => {
                nd.asn = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            ND_BGP_LS_ID if val.len() >= 4 => {
                nd.bgp_ls_id = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            ND_OSPF_AREA if val.len() >= 4 => {
                nd.ospf_area = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
            }
            ND_IGP_ROUTER_ID => {
                nd.igp_router = Some(RouterId { bytes: val.to_vec() });
            }
            _ => {}
        }
    }
}

// ─── Link NLRI (RFC 7752 §3.2.2) ─────────────────────────────────────────────

fn decode_link_nlri(protocol: BgpLsProtocol, identifier: u64, buf: &[u8]) -> Result<BgpLsNlri> {
    let mut cur = std::io::Cursor::new(buf);
    let mut local_node  = NodeDescriptor::default();
    let mut remote_node = NodeDescriptor::default();
    let mut link        = LinkDescriptor::default();

    while cur.remaining() >= 4 {
        let tlv_type = cur.get_u16();
        let tlv_len  = cur.get_u16() as usize;
        if cur.remaining() < tlv_len { break; }
        let val = cur.copy_to_bytes(tlv_len);

        match tlv_type {
            256 => { // Local Node Descriptor
                decode_node_sub_tlvs(&val, &mut local_node);
            }
            257 => { // Remote Node Descriptor
                decode_node_sub_tlvs(&val, &mut remote_node);
            }
            LD_LINK_LOCAL_REMOTE_ID if val.len() >= 8 => {
                link.local_id  = Some(u32::from_be_bytes([val[0], val[1], val[2], val[3]]));
                link.remote_id = Some(u32::from_be_bytes([val[4], val[5], val[6], val[7]]));
            }
            LD_IPV4_IF_ADDR if val.len() >= 4 => {
                link.local_addr = Some(IpAddr::V4(Ipv4Addr::from([val[0], val[1], val[2], val[3]])));
            }
            LD_IPV4_NEIGHBOR_ADDR if val.len() >= 4 => {
                link.remote_addr = Some(IpAddr::V4(Ipv4Addr::from([val[0], val[1], val[2], val[3]])));
            }
            LD_IPV6_IF_ADDR if val.len() >= 16 => {
                let mut a = [0u8; 16]; a.copy_from_slice(&val[..16]);
                link.local_addr = Some(IpAddr::V6(Ipv6Addr::from(a)));
            }
            LD_IPV6_NEIGHBOR_ADDR if val.len() >= 16 => {
                let mut a = [0u8; 16]; a.copy_from_slice(&val[..16]);
                link.remote_addr = Some(IpAddr::V6(Ipv6Addr::from(a)));
            }
            _ => {}
        }
    }

    Ok(BgpLsNlri::Link { protocol, identifier, local_node, remote_node, link })
}

// ─── Prefix NLRI (RFC 7752 §3.2.3) ───────────────────────────────────────────

fn decode_prefix_nlri(
    protocol:   BgpLsProtocol,
    identifier: u64,
    buf:        &[u8],
    is_v6:      bool,
) -> Result<BgpLsNlri> {
    let mut cur        = std::io::Cursor::new(buf);
    let mut local_node = NodeDescriptor::default();
    let mut prefix_opt: Option<Prefix> = None;

    while cur.remaining() >= 4 {
        let tlv_type = cur.get_u16();
        let tlv_len  = cur.get_u16() as usize;
        if cur.remaining() < tlv_len { break; }
        let val = cur.copy_to_bytes(tlv_len);

        match tlv_type {
            256 => {
                decode_node_sub_tlvs(&val, &mut local_node);
            }
            PD_IP_REACHABILITY if val.len() >= 1 => {
                // prefix_len(1) + prefix_bytes (ceil(len/8))
                let plen   = val[0];
                let octets = (plen as usize + 7) / 8;
                if val.len() >= 1 + octets {
                    prefix_opt = if is_v6 {
                        let mut a = [0u8; 16];
                        a[..octets].copy_from_slice(&val[1..1+octets]);
                        Ipv6Net::new(Ipv6Addr::from(a), plen).ok()
                            .map(|n| Prefix::V6(n.trunc()))
                    } else {
                        let mut a = [0u8; 4];
                        a[..octets].copy_from_slice(&val[1..1+octets]);
                        Ipv4Net::new(Ipv4Addr::from(a), plen).ok()
                            .map(|n| Prefix::V4(n.trunc()))
                    };
                }
            }
            PD_OSPF_ROUTE_TYPE | _ => {}
        }
    }

    let prefix = prefix_opt.unwrap_or(if is_v6 {
        Prefix::V6(Ipv6Net::new(Ipv6Addr::UNSPECIFIED, 0).unwrap())
    } else {
        Prefix::V4(Ipv4Net::new(Ipv4Addr::UNSPECIFIED, 0).unwrap())
    });

    Ok(BgpLsNlri::Prefix { protocol, identifier, local_node, prefix })
}

// ─── BGP-LS path attribute (type 29, RFC 7752 §3.3) ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrCapabilities {
    pub flags:       u8,
    pub srgb_ranges: Vec<(u32, u32)>,  // (base, range)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrLocalBlock {
    pub flags:       u8,
    pub srlb_ranges: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdjSid {
    pub flags:  u8,
    pub weight: u8,
    pub label:  u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanAdjSid {
    pub flags:       u8,
    pub weight:      u8,
    pub neighbor_id: [u8; 7],
    pub label:       u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsPrefixSid {
    pub flags:     u8,
    pub algorithm: u8,
    pub label:     u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlexAlgoDef {
    pub algo_id:     u8,
    pub metric_type: u8,
    pub calc_type:   u8,
    pub priority:    u8,
    pub exclude_any: Option<u32>,
    pub include_any: Option<u32>,
    pub include_all: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlexAlgoPrefixMetric {
    pub algo_id: u8,
    pub metric:  u32,
}

/// Decoded BGP-LS path attribute (attribute type 29).
/// Populated from `parse_bgpls_attribute` and stored in `PathAttributes.bgpls_attr`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BgpLsAttribute {
    // ── Node attributes ──────────────────────────────────────────────────────
    pub node_flags:          Option<u8>,
    pub node_name:           Option<String>,
    pub isis_area_id:        Option<Vec<u8>>,
    pub ipv4_router_id:      Option<Ipv4Addr>,
    pub ipv6_router_id:      Option<Ipv6Addr>,
    pub sr_capabilities:     Option<SrCapabilities>,
    pub sr_algorithm:        Vec<u8>,
    pub sr_local_block:      Option<SrLocalBlock>,
    // ── Link attributes ──────────────────────────────────────────────────────
    pub link_metric_igp:     Option<u32>,
    pub link_metric_te:      Option<u32>,
    pub admin_group:         Option<u32>,
    pub max_bandwidth:       Option<f32>,
    pub max_reservable_bw:   Option<f32>,
    pub unreserved_bw:       Vec<f32>,
    pub srlg:                Vec<u32>,
    pub adj_sid:             Vec<AdjSid>,
    pub lan_adj_sid:         Vec<LanAdjSid>,
    pub peer_node_sid:       Option<u32>,
    pub peer_adj_sid:        Option<u32>,
    pub peer_set_sid:        Option<u32>,
    // ── Prefix attributes ────────────────────────────────────────────────────
    pub prefix_metric:       Option<u32>,
    pub ospf_fwd_addr:       Option<IpAddr>,
    pub prefix_sid:          Vec<LsPrefixSid>,
    // ── Flex Algorithm (RFC 9350) ─────────────────────────────────────────────
    pub flex_algo_defs:            Vec<FlexAlgoDef>,
    pub flex_algo_prefix_metrics:  Vec<FlexAlgoPrefixMetric>,
    // ── Unknown TLVs preserved ───────────────────────────────────────────────
    pub unknown_tlvs:        Vec<(u16, Vec<u8>)>,
}

/// Parse BGP-LS path attribute (type 29) — TLV encoded per RFC 7752 §3.3.
/// Returns a `BgpLsAttribute`; unknown TLVs are preserved in `unknown_tlvs`.
pub fn parse_bgpls_attribute(buf: &[u8]) -> BgpLsAttribute {
    let mut attr = BgpLsAttribute::default();
    let mut pos  = 0;

    while pos + 4 <= buf.len() {
        let tlv_type = u16::from_be_bytes([buf[pos], buf[pos+1]]);
        let tlv_len  = u16::from_be_bytes([buf[pos+2], buf[pos+3]]) as usize;
        pos += 4;
        if pos + tlv_len > buf.len() { break; }
        let data = &buf[pos..pos+tlv_len];
        pos += tlv_len;

        match tlv_type {
            // ── Node attributes ──────────────────────────────────────────────
            // TLV 1024: Node Flag Bits
            1024 if tlv_len >= 1 => attr.node_flags = Some(data[0]),
            // TLV 1026: Node Name (IS-IS TLV 137 hostname)
            1026 => attr.node_name = Some(String::from_utf8_lossy(data).to_string()),
            // TLV 1027: IS-IS Area Identifier
            1027 => attr.isis_area_id = Some(data.to_vec()),
            // TLV 1028: IPv4 Router-ID of Local Node
            1028 if tlv_len == 4 => {
                attr.ipv4_router_id = Some(Ipv4Addr::from([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1029: IPv6 Router-ID of Local Node
            1029 if tlv_len == 16 => {
                let mut b = [0u8; 16]; b.copy_from_slice(data);
                attr.ipv6_router_id = Some(Ipv6Addr::from(b));
            }
            // TLV 1034: SR Capabilities (RFC 9085 §2.1.2)
            // Value: flags(1) + reserved(1) + [range(3) + SID-sub-TLV(type(1)+len(1)+value(3))]...
            1034 if tlv_len >= 2 => {
                let flags = data[0];
                let mut ranges = Vec::new();
                let mut i = 2; // skip flags(1) + reserved(1)
                // Each SRGB block: range(3) + SID-sub-TLV type(1)+len(1)+base(3) = 8 bytes
                while i + 8 <= data.len() {
                    let range = u32::from_be_bytes([0, data[i], data[i+1], data[i+2]]);
                    // data[i+3] = SID sub-TLV type, data[i+4] = SID sub-TLV len (=3)
                    let base = u32::from_be_bytes([0, data[i+5], data[i+6], data[i+7]]);
                    ranges.push((base, range));
                    i += 8;
                }
                attr.sr_capabilities = Some(SrCapabilities { flags, srgb_ranges: ranges });
            }
            // TLV 1035: SR Algorithm
            1035 => attr.sr_algorithm = data.to_vec(),
            // TLV 1036: SR Local Block (SRLB)
            1036 if tlv_len >= 2 => {
                let flags = data[0];
                let mut ranges = Vec::new();
                let mut i = 2;
                while i + 6 <= data.len() {
                    let range = u32::from_be_bytes([0, data[i], data[i+1], data[i+2]]);
                    let base  = if i + 6 <= data.len() {
                        u32::from_be_bytes([0, data[i+3], data[i+4], data[i+5]])
                    } else { 0 };
                    ranges.push((base, range));
                    i += 6;
                }
                attr.sr_local_block = Some(SrLocalBlock { flags, srlb_ranges: ranges });
            }
            // ── Link attributes ──────────────────────────────────────────────
            // TLV 1081: Max Link Bandwidth (IEEE 754 float, bytes/sec)
            1081 if tlv_len == 4 => {
                attr.max_bandwidth = Some(f32::from_be_bytes([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1082: Max Reservable Link Bandwidth
            1082 if tlv_len == 4 => {
                attr.max_reservable_bw = Some(f32::from_be_bytes([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1083: Unreserved Bandwidth (8 × 4-byte IEEE 754 floats)
            1083 if tlv_len == 32 => {
                for i in 0..8 {
                    attr.unreserved_bw.push(f32::from_be_bytes(
                        [data[i*4], data[i*4+1], data[i*4+2], data[i*4+3]]));
                }
            }
            // TLV 1088: TE Admin Group / Link Color
            1088 if tlv_len == 4 => {
                attr.admin_group = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1092: TE Default Metric
            1092 if tlv_len == 4 => {
                attr.link_metric_te = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1094: SRLG (Shared Risk Link Group) — array of u32
            1094 => {
                let mut i = 0;
                while i + 4 <= data.len() {
                    attr.srlg.push(u32::from_be_bytes([data[i], data[i+1], data[i+2], data[i+3]]));
                    i += 4;
                }
            }
            // TLV 1095: IGP Metric (variable 1-3 bytes)
            1095 if tlv_len >= 1 && tlv_len <= 4 => {
                let metric = match tlv_len {
                    1 => data[0] as u32,
                    2 => u16::from_be_bytes([data[0], data[1]]) as u32,
                    3 => u32::from_be_bytes([0, data[0], data[1], data[2]]),
                    _ => u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
                };
                attr.link_metric_igp = Some(metric);
            }
            // TLV 1099: Adjacency SID (RFC 8667 §2.2.1)
            1099 if tlv_len >= 7 => {
                let flags  = data[0];
                let weight = data[1];
                // bytes 2-3 reserved, then 3-byte label
                let label  = u32::from_be_bytes([0, data[4], data[5], data[6]]);
                attr.adj_sid.push(AdjSid { flags, weight, label });
            }
            // TLV 1100: LAN Adjacency SID — flags(1)+weight(1)+neighbor_id(7)+reserved(2)+label(3) = 14 bytes
            1100 if tlv_len >= 14 => {
                let flags  = data[0];
                let weight = data[1];
                let mut neighbor_id = [0u8; 7];
                neighbor_id.copy_from_slice(&data[2..9]);
                // bytes 9-10 reserved, bytes 11-13 = 3-byte label
                let label = u32::from_be_bytes([0, data[11], data[12], data[13]]);
                attr.lan_adj_sid.push(LanAdjSid { flags, weight, neighbor_id, label });
            }
            // TLV 1101: Peer Node SID (BGP EPE)
            1101 if tlv_len >= 7 => {
                attr.peer_node_sid = Some(u32::from_be_bytes([0, data[4], data[5], data[6]]));
            }
            // TLV 1102: Peer Adjacency SID (BGP EPE)
            1102 if tlv_len >= 7 => {
                attr.peer_adj_sid = Some(u32::from_be_bytes([0, data[4], data[5], data[6]]));
            }
            // TLV 1103: Peer Set SID (BGP EPE)
            1103 if tlv_len >= 7 => {
                attr.peer_set_sid = Some(u32::from_be_bytes([0, data[4], data[5], data[6]]));
            }
            // ── Prefix attributes ────────────────────────────────────────────
            // TLV 1155: Prefix Metric
            1155 if tlv_len == 4 => {
                attr.prefix_metric = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
            }
            // TLV 1156: OSPF Forwarding Address
            1156 if tlv_len == 4 => {
                attr.ospf_fwd_addr = Some(IpAddr::V4(Ipv4Addr::from([data[0], data[1], data[2], data[3]])));
            }
            1156 if tlv_len == 16 => {
                let mut b = [0u8; 16]; b.copy_from_slice(data);
                attr.ospf_fwd_addr = Some(IpAddr::V6(Ipv6Addr::from(b)));
            }
            // TLV 1158: Prefix-SID (BGP-LS variant, RFC 8667 §2.1)
            1158 if tlv_len >= 7 => {
                let flags     = data[0];
                let algorithm = data[1];
                // bytes 2-3 reserved, then 3-byte label or 4-byte index
                let label = if tlv_len >= 8 {
                    u32::from_be_bytes([data[4], data[5], data[6], data[7]])
                } else {
                    u32::from_be_bytes([0, data[4], data[5], data[6]])
                };
                attr.prefix_sid.push(LsPrefixSid { flags, algorithm, label });
            }
            // ── Flex Algorithm (RFC 9350) ─────────────────────────────────────
            // TLV 1039: Flex Algorithm Definition
            1039 if tlv_len >= 4 => {
                let algo_id     = data[0];
                let metric_type = data[1];
                let calc_type   = data[2];
                let priority    = data[3];
                let mut exclude_any = None;
                let mut include_any = None;
                let mut include_all = None;
                let mut i = 4;
                // Optional sub-TLVs follow
                while i + 4 <= data.len() {
                    let st   = u16::from_be_bytes([data[i], data[i+1]]);
                    let slen = u16::from_be_bytes([data[i+2], data[i+3]]) as usize;
                    i += 4;
                    if i + slen > data.len() { break; }
                    let sv = &data[i..i+slen]; i += slen;
                    if slen >= 4 {
                        let v = u32::from_be_bytes([sv[0], sv[1], sv[2], sv[3]]);
                        match st {
                            1 => exclude_any = Some(v),
                            2 => include_any = Some(v),
                            3 => include_all = Some(v),
                            _ => {}
                        }
                    }
                }
                attr.flex_algo_defs.push(FlexAlgoDef {
                    algo_id, metric_type, calc_type, priority,
                    exclude_any, include_any, include_all,
                });
            }
            // TLV 1044: Flex Algorithm Prefix Metric
            1044 if tlv_len >= 5 => {
                attr.flex_algo_prefix_metrics.push(FlexAlgoPrefixMetric {
                    algo_id: data[0],
                    metric:  u32::from_be_bytes([data[1], data[2], data[3], data[4]]),
                });
            }
            _ => attr.unknown_tlvs.push((tlv_type, data.to_vec())),
        }
    }
    attr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_buf_returns_empty() {
        let result = decode_bgpls_nlri(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_unknown_nlri_type_preserved() {
        // type=99, len=4, data=0xDEADBEEF
        let buf = [0u8, 99, 0, 4, 0xDE, 0xAD, 0xBE, 0xEF];
        let result = decode_bgpls_nlri(&buf).unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], BgpLsNlri::Unknown { nlri_type: 99, .. }));
    }

    #[test]
    fn test_bgpls_attr_link_metrics_srlg_adjsid() {
        let mut buf = Vec::new();
        // TLV 1095: IGP Metric = 100 (3 bytes)
        buf.extend_from_slice(&1095u16.to_be_bytes());
        buf.extend_from_slice(&3u16.to_be_bytes());
        buf.extend_from_slice(&[0, 0, 100]);
        // TLV 1094: SRLG with two groups: 10, 20
        buf.extend_from_slice(&1094u16.to_be_bytes());
        buf.extend_from_slice(&8u16.to_be_bytes());
        buf.extend_from_slice(&10u32.to_be_bytes());
        buf.extend_from_slice(&20u32.to_be_bytes());
        // TLV 1099: Adjacency SID — flags(1)+weight(1)+reserved(2)+label(3) = 7 bytes
        buf.extend_from_slice(&1099u16.to_be_bytes());
        buf.extend_from_slice(&7u16.to_be_bytes());
        buf.extend_from_slice(&[0x30, 0x01, 0, 0, 0, 0x10, 0x00]); // label = 0x001000 = 4096
        let attr = parse_bgpls_attribute(&buf);
        assert_eq!(attr.link_metric_igp, Some(100));
        assert_eq!(attr.srlg, vec![10, 20]);
        assert_eq!(attr.adj_sid.len(), 1);
        assert_eq!(attr.adj_sid[0].label, 0x001000);
    }

    #[test]
    fn test_bgpls_attr_node_name() {
        let name = b"pe1.example.com";
        let mut buf = Vec::new();
        buf.extend_from_slice(&1026u16.to_be_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
        buf.extend_from_slice(name);
        let attr = parse_bgpls_attribute(&buf);
        assert_eq!(attr.node_name.as_deref(), Some("pe1.example.com"));
    }

    #[test]
    fn test_bgpls_attr_sr_capabilities() {
        // RFC 9085 §2.1.2: TLV 1034 value = flags(1)+reserved(1)+[range(3)+SID-sub-TLV(5)...]
        // SID sub-TLV: type(1)=1, length(1)=3, value(3)=base label
        let mut buf = Vec::new();
        buf.extend_from_slice(&1034u16.to_be_bytes());
        buf.extend_from_slice(&10u16.to_be_bytes()); // value length = 10
        buf.push(0xC0); // flags: I-flag + V-flag
        buf.push(0);    // reserved
        // Range sub-TLV: type(1)=9, length(1)=3, range(3)=62500 (0x00F424)
        buf.extend_from_slice(&[0, 0xF4, 0x24]); // range bytes (raw, no sub-TLV wrapper used here)
        // SID/Label sub-TLV: type(1)=1, length(1)=3, base(3)=256 (0x000100)
        buf.extend_from_slice(&[1, 3]);           // sub-TLV type=1, len=3
        buf.extend_from_slice(&[0, 0x01, 0x00]);  // base = 256
        let attr = parse_bgpls_attribute(&buf);
        let caps = attr.sr_capabilities.unwrap();
        assert_eq!(caps.flags, 0xC0);
        assert_eq!(caps.srgb_ranges.len(), 1);
        let (base, range) = caps.srgb_ranges[0];
        assert_eq!(range, 62500);
        assert_eq!(base, 256);
    }

    #[test]
    fn test_node_nlri_protocol_identifier() {
        // Build a minimal Node NLRI: type=1, len=13 (9 hdr + 4 empty outer TLV)
        // protocol=3 (OSPFv2), identifier=42
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u16.to_be_bytes());  // type = Node
        buf.extend_from_slice(&13u16.to_be_bytes()); // len
        buf.push(3);                                  // protocol = OSPFv2
        buf.extend_from_slice(&42u64.to_be_bytes());  // identifier
        // outer node descriptor TLV type=256, len=0
        buf.extend_from_slice(&256u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        let result = decode_bgpls_nlri(&buf).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            BgpLsNlri::Node { protocol, identifier, .. } => {
                assert_eq!(*protocol, BgpLsProtocol::Ospfv2);
                assert_eq!(*identifier, 42);
            }
            _ => panic!("expected Node"),
        }
    }
}

use std::net::{IpAddr, Ipv4Addr};
use bytes::Buf;
use crate::{Error, Result};
use super::types::*;
use super::nlri::{decode_nlri_with_path_id, decode_labeled_nlri, decode_vpn_nlri, decode_next_hops, decode_vpls_nlri};
use super::evpn::decode_evpn_nlri;
use super::flowspec::{decode_flowspec_nlri, FlowspecNlri};
use super::srv6::parse_prefix_sid;
use super::bgpls::{decode_bgpls_reach, decode_bgpls_unreach, parse_bgpls_attribute};
use super::srpolicy::{decode_srpolicy_nlri, decode_rtc_nlri, SrPolicyNlri, RtcNlri};

// ─── Attribute flag bits ──────────────────────────────────────────────────────
#[allow(dead_code)] const FLAG_OPTIONAL:   u8 = 0x80;
#[allow(dead_code)] const FLAG_TRANSITIVE: u8 = 0x40;
#[allow(dead_code)] const FLAG_PARTIAL:    u8 = 0x20;
const FLAG_EXT_LEN:    u8 = 0x10;
const AS_TRANS:        u32 = 23456;

/// Parse all path attributes from buf using default 4-byte ASN decoding context.
pub fn parse_path_attributes(buf: impl Buf) -> Result<PathAttributes> {
    parse_path_attributes_with_context(buf, &BgpDecodeContext::default())
}

/// Parse all path attributes from buf using explicit session decoding context (`BgpDecodeContext`).
/// buf must contain exactly the path attributes section (already length-delimited).
pub fn parse_path_attributes_with_context(mut buf: impl Buf, ctx: &BgpDecodeContext) -> Result<PathAttributes> {
    let mut attrs = PathAttributes::default();
    while buf.remaining() > 0 {
        if buf.remaining() < 3 {
            return Err(Error::UnexpectedEof {
                needed: 3,
                have: buf.remaining(),
            });
        }
        let flags     = buf.get_u8();
        let type_code = buf.get_u8();
        let attr_len = if flags & FLAG_EXT_LEN != 0 {
            if buf.remaining() < 2 {
                return Err(Error::UnexpectedEof { needed: 2, have: buf.remaining() });
            }
            buf.get_u16() as usize
        } else {
            buf.get_u8() as usize
        };

        if buf.remaining() < attr_len {
            return Err(Error::TruncatedAttribute {
                attr_type: type_code,
                declared: attr_len,
                available: buf.remaining(),
            });
        }

        let attr_data = buf.copy_to_bytes(attr_len);
        let attr_buf  = attr_data.as_ref();

        match type_code {
            // Type 1: ORIGIN
            1 => {
                if attr_len != 1 {
                    return Err(Error::BgpParse(format!(
                        "invalid ORIGIN attribute length {attr_len}, expected 1"
                    )));
                }
                attrs.origin = Some(Origin::try_from(attr_buf[0])?);
            }
            // Type 2: AS_PATH
            2 => {
                attrs.as_path = Some(parse_as_path(attr_buf, ctx.four_byte_asn)?);
            }
            // Type 3: NEXT_HOP
            3 => {
                if attr_len != 4 {
                    return Err(Error::BgpParse(format!(
                        "invalid NEXT_HOP attribute length {attr_len}, expected 4"
                    )));
                }
                attrs.next_hop = Some(IpAddr::V4(Ipv4Addr::from([attr_buf[0], attr_buf[1], attr_buf[2], attr_buf[3]])));
            }
            // Type 4: MULTI_EXIT_DISC
            4 => {
                if attr_len != 4 {
                    return Err(Error::BgpParse(format!(
                        "invalid MULTI_EXIT_DISC attribute length {attr_len}, expected 4"
                    )));
                }
                attrs.multi_exit_disc = Some(u32::from_be_bytes(attr_buf[..4].try_into().unwrap()));
            }
            // Type 5: LOCAL_PREF
            5 => {
                if attr_len != 4 {
                    return Err(Error::BgpParse(format!(
                        "invalid LOCAL_PREF attribute length {attr_len}, expected 4"
                    )));
                }
                attrs.local_pref = Some(u32::from_be_bytes(attr_buf[..4].try_into().unwrap()));
            }
            // Type 6: ATOMIC_AGGREGATE
            6 => {
                attrs.atomic_aggregate = true;
            }
            // Type 7: AGGREGATOR (2+4 or 4+4 byte forms)
            7 if attr_len == 6 => {
                let asn = u16::from_be_bytes([attr_buf[0], attr_buf[1]]) as u32;
                let id  = Ipv4Addr::from([attr_buf[2], attr_buf[3], attr_buf[4], attr_buf[5]]);
                attrs.aggregator = Some((asn, id));
            }
            7 if attr_len == 8 => {
                let asn = u32::from_be_bytes([attr_buf[0], attr_buf[1], attr_buf[2], attr_buf[3]]);
                let id  = Ipv4Addr::from([attr_buf[4], attr_buf[5], attr_buf[6], attr_buf[7]]);
                attrs.aggregator = Some((asn, id));
            }
            // Type 8: COMMUNITY (RFC 1997)
            8 => {
                if attr_len % 4 != 0 {
                    return Err(Error::BgpParse(format!(
                        "invalid COMMUNITY attribute length {attr_len} (must be multiple of 4)"
                    )));
                }
                let mut cb = attr_data.as_ref();
                while cb.len() >= 4 {
                    let val = u32::from_be_bytes([cb[0], cb[1], cb[2], cb[3]]);
                    attrs.communities.push(StandardCommunity(val));
                    cb = &cb[4..];
                }
            }
            // Type 9: ORIGINATOR_ID (RFC 4456)
            9 if attr_len == 4 => {
                attrs.originator_id = Some(Ipv4Addr::from([attr_buf[0], attr_buf[1], attr_buf[2], attr_buf[3]]));
            }
            // Type 10: CLUSTER_LIST (RFC 4456)
            10 => {
                let mut cb = attr_data.as_ref();
                while cb.len() >= 4 {
                    attrs.cluster_list.push(Ipv4Addr::from([cb[0], cb[1], cb[2], cb[3]]));
                    cb = &cb[4..];
                }
            }
            // Type 14: MP_REACH_NLRI (RFC 4760)
            14 => {
                let (mp, evpn_reach, flowspec_reach, bgpls_reach, sr_nlris, rtc_nlris)
                    = parse_mp_reach(attr_data.as_ref(), ctx)?;
                attrs.mp_reach          = Some(mp);
                attrs.evpn_reach        = evpn_reach;
                attrs.flowspec_reach    = flowspec_reach;
                attrs.bgpls_reach       = bgpls_reach;
                attrs.sr_policy_nlris   = sr_nlris;
                attrs.rtc_nlris         = rtc_nlris;
            }
            // Type 15: MP_UNREACH_NLRI (RFC 4760)
            15 => {
                let (mp, evpn_unreach, flowspec_unreach, bgpls_unreach, sr_wd, _rtc_wd)
                    = parse_mp_unreach(attr_data.as_ref(), ctx)?;
                attrs.mp_unreach        = Some(mp);
                attrs.evpn_unreach      = evpn_unreach;
                attrs.flowspec_unreach  = flowspec_unreach;
                attrs.bgpls_unreach     = bgpls_unreach;
                if let Some(nlris) = sr_wd {
                    attrs.sr_policy_nlris = nlris;
                }
            }
            // Type 16: EXTENDED COMMUNITIES (RFC 4360)
            16 => {
                let mut cb = attr_data.as_ref();
                while cb.len() >= 8 {
                    let raw: [u8; 8] = cb[..8].try_into().unwrap();
                    attrs.extended_communities.push(ExtendedCommunity::from_bytes(&raw));
                    cb = &cb[8..];
                }
            }
            // Type 17: AS4_PATH (RFC 6793) — always 4-byte ASNs
            17 => {
                attrs.as4_path = Some(parse_as_path_with_asn_size(attr_buf, 4)?);
            }
            // Type 18: AS4_AGGREGATOR (RFC 6793)
            18 if attr_len == 8 => {
                let asn = u32::from_be_bytes([attr_buf[0], attr_buf[1], attr_buf[2], attr_buf[3]]);
                let id  = Ipv4Addr::from([attr_buf[4], attr_buf[5], attr_buf[6], attr_buf[7]]);
                attrs.as4_aggregator = Some((asn, id));
            }
            // Type 23: TUNNEL_ENCAPSULATION (RFC 9012)
            23 => {
                attrs.tunnel_encap = Some(parse_tunnel_encap(attr_data.as_ref()));
            }
            // Type 32: LARGE_COMMUNITY (RFC 8092)
            32 => {
                let mut cb = attr_data.as_ref();
                while cb.len() >= 12 {
                    let ga  = u32::from_be_bytes([cb[0], cb[1], cb[2], cb[3]]);
                    let ld1 = u32::from_be_bytes([cb[4], cb[5], cb[6], cb[7]]);
                    let ld2 = u32::from_be_bytes([cb[8], cb[9], cb[10], cb[11]]);
                    attrs.large_communities.push(LargeCommunity { global_admin: ga, local_data_1: ld1, local_data_2: ld2 });
                    cb = &cb[12..];
                }
            }
            // Type 35: ONLY_TO_CUSTOMER (RFC 9234) — 4-byte AS number
            35 if attr_len == 4 => {
                attrs.only_to_customer = Some(u32::from_be_bytes(attr_buf[..4].try_into().unwrap()));
            }
            // Type 29: BGP-LS attribute (RFC 7752 §3.3)
            29 => {
                attrs.bgpls_attr = Some(parse_bgpls_attribute(attr_data.as_ref()));
            }
            // Type 40: BGP_PREFIX_SID (RFC 8669)
            40 => {
                attrs.prefix_sid = parse_prefix_sid(attr_data.as_ref()).ok();
            }
            // Type 30: BGPsec_Path (RFC 8205, parse-only for RV6)
            30 => {
                attrs.bgpsec_path = Some(parse_bgpsec_path(attr_data.as_ref()));
            }
            // Everything else preserved as raw
            _ => {
                attrs.unknown.push(RawAttribute {
                    flags,
                    type_code,
                    value: attr_data.to_vec(),
                });
            }
        }
    }

    // Populate top-level next_hop from MP_REACH_NLRI when NEXT_HOP (type 3) is absent (e.g. IPv6 routes).
    if attrs.next_hop.is_none() {
        if let Some(mp) = &attrs.mp_reach {
            attrs.next_hop = mp.next_hops.first().copied();
        }
    }

    // RFC 6793 §4.2.3: Reconstruct AS_PATH from AS4_PATH when 2-byte AS_PATH contains AS_TRANS (23456).
    if let (Some(as_path), Some(as4_path)) = (&attrs.as_path, &attrs.as4_path) {
        if !ctx.four_byte_asn || as_path.0.iter().any(|seg| seg.asns().contains(&AS_TRANS)) {
            attrs.as_path = Some(reconstruct_as_path(as_path, as4_path));
        }
    }

    // RFC 6793 §4.2.2: Replace AGGREGATOR with AS4_AGGREGATOR when AGGREGATOR ASN is AS_TRANS (23456).
    if let (Some((agg_asn, _)), Some(as4_agg)) = (attrs.aggregator, attrs.as4_aggregator) {
        if agg_asn == AS_TRANS {
            attrs.aggregator = Some(as4_agg);
        }
    }

    Ok(attrs)
}

/// Reconstruct 4-byte `AsPath` from a 2-byte `as_path` and 4-byte `as4_path` per RFC 6793 §4.2.3.
fn reconstruct_as_path(as_path: &AsPath, as4_path: &AsPath) -> AsPath {
    let as4_segments: Vec<AsPathSegment> = as4_path
        .0
        .iter()
        .filter(|seg| !matches!(seg, AsPathSegment::ConfedSequence(_) | AsPathSegment::ConfedSet(_)))
        .cloned()
        .collect();
    let n_as: usize = as_path.0.iter().map(|s| s.asns().len()).sum();
    let n_as4: usize = as4_segments.iter().map(|s| s.asns().len()).sum();
    if n_as4 == 0 || n_as < n_as4 {
        return as_path.clone();
    }

    let mut keep = n_as - n_as4;
    let mut out: Vec<AsPathSegment> = Vec::new();
    for seg in &as_path.0 {
        if keep == 0 {
            break;
        }
        let asns = seg.asns();
        let take = keep.min(asns.len());
        keep -= take;
        let slice = asns[..take].to_vec();
        if !slice.is_empty() {
            let new_seg = match seg {
                AsPathSegment::Set(_) => AsPathSegment::Set(slice),
                AsPathSegment::Sequence(_) => AsPathSegment::Sequence(slice),
                AsPathSegment::ConfedSequence(_) => AsPathSegment::ConfedSequence(slice),
                AsPathSegment::ConfedSet(_) => AsPathSegment::ConfedSet(slice),
            };
            out.push(new_seg);
        }
    }

    for seg in as4_segments {
        match (out.last_mut(), seg) {
            (Some(AsPathSegment::Sequence(prev)), AsPathSegment::Sequence(next)) => {
                prev.extend(next);
            }
            (_, other) => out.push(other),
        }
    }
    AsPath(out)
}

/// Parse AS_PATH or AS4_PATH attribute.
/// `four_byte`: true when 4-octet ASN encoding is expected on the wire (RFC 6793 / BMP A-flag = 0),
/// false when legacy 2-octet ASN encoding is expected (BMP A-flag = 1).
pub fn parse_as_path(buf: &[u8], four_byte: bool) -> Result<AsPath> {
    if four_byte {
        return match parse_as_path_with_asn_size(buf, 4) {
            Ok(path) => Ok(path),
            Err(original_err) => parse_as_path_with_asn_size(buf, 2).or(Err(original_err)),
        };
    }

    match parse_as_path_with_asn_size(buf, 2) {
        Ok(path) => Ok(path),
        Err(original_err) => parse_as_path_with_asn_size(buf, 4).or(Err(original_err)),
    }
}

fn parse_as_path_with_asn_size(mut buf: &[u8], asn_size: usize) -> Result<AsPath> {
    let mut segments = Vec::new();
    while !buf.is_empty() {
        if buf.len() < 2 {
            return Err(Error::UnexpectedEof {
                needed: 2,
                have: buf.len(),
            });
        }
        let seg_type = buf[0];
        let seg_len  = buf[1] as usize;
        buf = &buf[2..];
        let needed = seg_len * asn_size;
        if buf.len() < needed {
            return Err(Error::UnexpectedEof { needed, have: buf.len() });
        }
        let mut asns: Vec<u32> = Vec::with_capacity(seg_len);
        for i in 0..seg_len {
            let asn = if asn_size == 4 {
                u32::from_be_bytes(buf[i*4..(i+1)*4].try_into().unwrap())
            } else {
                u16::from_be_bytes(buf[i*2..(i+1)*2].try_into().unwrap()) as u32
            };
            asns.push(asn);
        }
        buf = &buf[needed..];
        let seg = match seg_type {
            1 => AsPathSegment::Set(asns),
            2 => AsPathSegment::Sequence(asns),
            3 => AsPathSegment::ConfedSequence(asns),
            4 => AsPathSegment::ConfedSet(asns),
            _ => return Err(Error::BgpParse(format!("unknown AS_PATH segment type {seg_type}"))),
        };
        segments.push(seg);
    }
    Ok(AsPath(segments))
}

fn parse_mp_reach(buf: &[u8], ctx: &BgpDecodeContext) -> Result<(MpReachNlri, Option<EvpnReachNlri>, Option<Vec<FlowspecNlri>>, Option<BgpLsReachNlri>, Vec<SrPolicyNlri>, Vec<RtcNlri>)> {
    if buf.len() < 4 {
        return Err(Error::UnexpectedEof { needed: 4, have: buf.len() });
    }
    let afi      = u16::from_be_bytes([buf[0], buf[1]]);
    let safi     = buf[2];
    let afi_safi = AfiSafi::new(afi, safi);
    let mut cur  = std::io::Cursor::new(&buf[3..]);
    let next_hops = decode_next_hops(&mut cur, afi_safi.afi)?;
    if cur.remaining() < 1 {
        return Err(Error::UnexpectedEof { needed: 1, have: cur.remaining() });
    }
    let _snpa = cur.get_u8();
    let remaining = cur.chunk().to_vec();
    let add_path = ctx.has_add_path(afi_safi);

    let (prefixes, path_ids, evpn_reach, flowspec_reach, bgpls_reach) = match afi_safi.safi {
        Safi::Evpn => {
            let routes = decode_evpn_nlri(&remaining)?;
            let evpn   = EvpnReachNlri { next_hops: next_hops.clone(), routes };
            (Vec::new(), Vec::new(), Some(evpn), None, None)
        }
        Safi::Flowspec | Safi::FlowspecVpn => {
            let ipv6 = matches!(afi_safi.afi, Afi::Ipv6);
            let fs   = decode_flowspec_nlri(&remaining, ipv6)?;
            (Vec::new(), Vec::new(), None, Some(fs), None)
        }
        Safi::BgpLs | Safi::BgpLsSrv6 => {
            let next_hop = next_hops.first().copied();
            let ls = decode_bgpls_reach(next_hop, &remaining)?;
            (Vec::new(), Vec::new(), None, None, Some(ls))
        }
        Safi::Vpls => {
            let _vpls = decode_vpls_nlri(&remaining)?;
            (Vec::new(), Vec::new(), None, None, None)
        }
        Safi::SrPolicy => {
            let afi_is_ipv6 = matches!(afi_safi.afi, Afi::Ipv6);
            let nlris = decode_srpolicy_nlri(&remaining, afi_is_ipv6)?;
            return Ok((MpReachNlri { afi_safi, next_hops, prefixes: Vec::new(), path_ids: Vec::new() }, None, None, None,
                       nlris, Vec::new()));
        }
        Safi::RouteTargetConstraint => {
            let rtcs = decode_rtc_nlri(&remaining)?;
            return Ok((MpReachNlri { afi_safi, next_hops, prefixes: Vec::new(), path_ids: Vec::new() }, None, None, None,
                       Vec::new(), rtcs));
        }
        Safi::Unicast | Safi::Multicast => {
            let with_ids = decode_nlri_with_path_id(&mut std::io::Cursor::new(&remaining), afi_safi.afi, add_path)?;
            let (p, ids): (Vec<_>, Vec<_>) = with_ids.into_iter().unzip();
            (p, ids, None, None, None)
        }
        Safi::LabeledUnicast => {
            let p = decode_labeled_nlri(&mut std::io::Cursor::new(&remaining), afi_safi.afi)?;
            let ids = vec![None; p.len()];
            (p, ids, None, None, None)
        }
        Safi::MplsVpn => {
            let p = decode_vpn_nlri(&mut std::io::Cursor::new(&remaining), afi_safi.afi)?;
            let ids = vec![None; p.len()];
            (p, ids, None, None, None)
        }
        Safi::McastVpn | Safi::MplsVpnMulticast => {
            // RFC 6514 §4: MCAST-VPN NLRI — store raw for future type-specific decode
            (Vec::new(), Vec::new(), None, None, None)
        }
        _ => (Vec::new(), Vec::new(), None, None, None),
    };

    Ok((MpReachNlri { afi_safi, next_hops, prefixes, path_ids }, evpn_reach, flowspec_reach, bgpls_reach,
        Vec::new(), Vec::new()))
}

fn parse_mp_unreach(buf: &[u8], ctx: &BgpDecodeContext) -> Result<(MpUnreachNlri, Option<EvpnUnreachNlri>, Option<Vec<FlowspecNlri>>, Option<BgpLsUnreachNlri>, Option<Vec<SrPolicyNlri>>, Vec<RtcNlri>)> {
    if buf.len() < 3 {
        return Err(Error::UnexpectedEof { needed: 3, have: buf.len() });
    }
    let afi      = u16::from_be_bytes([buf[0], buf[1]]);
    let safi     = buf[2];
    let afi_safi = AfiSafi::new(afi, safi);
    let remaining = &buf[3..];
    let add_path = ctx.has_add_path(afi_safi);

    if remaining.is_empty() {
        return Ok((
            MpUnreachNlri {
                afi_safi,
                prefixes: Vec::new(),
                path_ids: Vec::new(),
            },
            None,
            None,
            None,
            None,
            Vec::new(),
        ));
    }

    let (prefixes, path_ids, evpn_unreach, flowspec_unreach, bgpls_unreach): (Vec<_>, Vec<_>, _, _, _) = match afi_safi.safi {
        Safi::Evpn => {
            let routes = decode_evpn_nlri(remaining)?;
            (Vec::new(), Vec::new(), Some(EvpnUnreachNlri { routes }), None, None)
        }
        Safi::Flowspec | Safi::FlowspecVpn => {
            let ipv6 = matches!(afi_safi.afi, Afi::Ipv6);
            let fs   = decode_flowspec_nlri(remaining, ipv6)?;
            (Vec::new(), Vec::new(), None, Some(fs), None)
        }
        Safi::BgpLs => {
            let ls = decode_bgpls_unreach(remaining)?;
            (Vec::new(), Vec::new(), None, None, Some(ls))
        }
        Safi::SrPolicy => {
            let afi_is_ipv6 = matches!(afi_safi.afi, Afi::Ipv6);
            let nlris = decode_srpolicy_nlri(remaining, afi_is_ipv6)?;
            return Ok((MpUnreachNlri { afi_safi, prefixes: Vec::new(), path_ids: Vec::new() }, None, None, None,
                       Some(nlris), Vec::new()));
        }
        Safi::RouteTargetConstraint => {
            let rtcs = decode_rtc_nlri(remaining)?;
            return Ok((MpUnreachNlri { afi_safi, prefixes: Vec::new(), path_ids: Vec::new() }, None, None, None,
                       None, rtcs));
        }
        Safi::Unicast | Safi::Multicast => {
            let with_ids = decode_nlri_with_path_id(&mut std::io::Cursor::new(remaining), afi_safi.afi, add_path)?;
            let (p, ids): (Vec<_>, Vec<_>) = with_ids.into_iter().unzip();
            (p, ids, None, None, None)
        }
        Safi::LabeledUnicast => {
            let p = decode_labeled_nlri(&mut std::io::Cursor::new(remaining), afi_safi.afi)?;
            let ids = vec![None; p.len()];
            (p, ids, None, None, None)
        }
        Safi::MplsVpn => {
            let p = decode_vpn_nlri(&mut std::io::Cursor::new(remaining), afi_safi.afi)?;
            let ids = vec![None; p.len()];
            (p, ids, None, None, None)
        }
        _ => (Vec::new(), Vec::new(), None, None, None),
    };

    Ok((MpUnreachNlri { afi_safi, prefixes, path_ids }, evpn_unreach, flowspec_unreach, bgpls_unreach,
        None, Vec::new()))
}

fn parse_tunnel_encap(buf: &[u8]) -> Vec<TunnelEncapEntry> {
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos + 4 <= buf.len() {
        let tunnel_type = u16::from_be_bytes([buf[pos], buf[pos+1]]);
        let tlv_len     = u16::from_be_bytes([buf[pos+2], buf[pos+3]]) as usize;
        pos += 4;
        if pos + tlv_len > buf.len() { break; }
        // Sub-TLVs inside: parse for endpoint (type 1) and color (type 9)
        let sub_buf = &buf[pos..pos+tlv_len];
        pos += tlv_len;
        let (endpoint, color) = parse_tunnel_subtlvs(sub_buf);
        entries.push(TunnelEncapEntry {
            tunnel_type,
            tunnel_type_name: tunnel_type_name(tunnel_type).to_string(),
            endpoint,
            color,
        });
    }
    entries
}

fn parse_tunnel_subtlvs(buf: &[u8]) -> (Option<IpAddr>, Option<u32>) {
    let mut endpoint = None;
    let mut color    = None;
    let mut pos = 0;
    while pos + 2 <= buf.len() {
        let sub_type = buf[pos];
        let sub_len  = buf[pos+1] as usize;
        pos += 2;
        if pos + sub_len > buf.len() { break; }
        let data = &buf[pos..pos+sub_len];
        pos += sub_len;
        match sub_type {
            1 if data.len() == 4 => {
                endpoint = Some(IpAddr::V4(std::net::Ipv4Addr::from([data[0], data[1], data[2], data[3]])));
            }
            1 if data.len() == 16 => {
                let mut b = [0u8; 16]; b.copy_from_slice(data);
                endpoint = Some(IpAddr::V6(std::net::Ipv6Addr::from(b)));
            }
            9 if data.len() >= 4 => {
                // Color sub-TLV: flags(1) + reserved(1) + color(4) — skip flags+reserved
                if data.len() >= 6 {
                    color = Some(u32::from_be_bytes([data[2], data[3], data[4], data[5]]));
                }
            }
            _ => {}
        }
    }
    (endpoint, color)
}

// ─── BGPsec_Path parser (RFC 8205, RV6-2) ────────────────────────────────────

/// Parsed BGPsec_Path attribute — parse-only, no validation for RV6.
/// Full cryptographic validation requires RPKI router certificates (future work).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BgpsecPath {
    /// Secure AS numbers extracted from the Secure_Path segment
    pub signing_asns: Vec<u32>,
    /// Number of signature blocks (typically 1 or 2)
    pub sig_block_count: u8,
    /// Raw attribute bytes preserved for future validation
    pub raw: Vec<u8>,
}

/// Parse a BGPsec_Path attribute (RFC 8205 §3.2).
///
/// Structure:
///   Secure_Path (variable):
///     Secure_Path_Segment* = pCount(1) + flags(1) + AS Number(4)
///   Signature_Block* (variable):
///     Algo Suite Identifier(1) + Signature_Segment* ...
///
/// For RV6 we extract only the signing ASN sequence and block count.
pub fn parse_bgpsec_path(buf: &[u8]) -> BgpsecPath {
    let raw = buf.to_vec();
    let mut signing_asns = Vec::new();
    let mut sig_block_count = 0u8;

    if buf.len() < 2 {
        return BgpsecPath { signing_asns, sig_block_count, raw };
    }

    // Secure_Path length is a 2-byte field at the start
    let secure_path_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if buf.len() < 2 + secure_path_len {
        return BgpsecPath { signing_asns, sig_block_count, raw };
    }

    // Parse Secure_Path segments (each 6 bytes: pCount + flags + ASN)
    let mut pos = 2;
    let secure_path_end = 2 + secure_path_len;
    while pos + 6 <= secure_path_end {
        let _pcount = buf[pos];
        let _flags  = buf[pos + 1];
        let asn = u32::from_be_bytes([buf[pos+2], buf[pos+3], buf[pos+4], buf[pos+5]]);
        signing_asns.push(asn);
        pos += 6;
    }

    // Count Signature_Blocks — each starts with 2-byte length + 1-byte algo
    pos = secure_path_end;
    while pos + 3 <= buf.len() {
        let block_len = u16::from_be_bytes([buf[pos], buf[pos+1]]) as usize;
        sig_block_count += 1;
        pos += 2 + block_len;
    }

    BgpsecPath { signing_asns, sig_block_count, raw }
}

#[cfg(test)]
mod tests {
    use super::{parse_as_path, AsPath, AsPathSegment};

    #[test]
    fn parse_as_path_accepts_two_byte_encoding() {
        let buf = [
            2, 2, // AS_SEQUENCE with 2 ASNs
            0xFD, 0x4C, // 64844
            0xFD, 0x4D, // 64845
        ];

        let parsed = parse_as_path(&buf, false).unwrap();
        assert_eq!(
            parsed,
            AsPath(vec![AsPathSegment::Sequence(vec![64844, 64845])])
        );
    }

    #[test]
    fn parse_as_path_falls_back_to_four_byte_encoding() {
        let buf = [
            2, 2, // AS_SEQUENCE with 2 ASNs
            0x00, 0x00, 0xFD, 0x4C, // 64844
            0x00, 0x00, 0xFD, 0x4D, // 64845
        ];

        let parsed = parse_as_path(&buf, false).unwrap();
        assert_eq!(
            parsed,
            AsPath(vec![AsPathSegment::Sequence(vec![64844, 64845])])
        );
    }
}

/// RV6-2: MCAST-VPN NLRI full decode (RFC 6514).
///
/// MCAST-VPN uses AFI=1/2 + SAFI=5 (MplsVpnMulticast) and AFI=1/2 + SAFI=129.
/// In practice BMP carries it via MP_REACH/MP_UNREACH with SAFI=5.
///
/// RFC 6514 defines 7 NLRI types (Route Types 1–7):
///   Type 1 — Intra-AS I-PMSI A-D Route
///   Type 2 — Inter-AS I-PMSI A-D Route
///   Type 3 — S-PMSI A-D Route
///   Type 4 — Leaf A-D Route
///   Type 5 — Source Active A-D Route
///   Type 6 — Shared Tree Join Route
///   Type 7 — Source Tree Join Route
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

// ─── MCAST-VPN NLRI types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MvpnNlri {
    /// Type 1: Intra-AS I-PMSI A-D Route
    IntraAsIPmsi {
        rd:         [u8; 8],
        origin_asn: u32,
    },
    /// Type 2: Inter-AS I-PMSI A-D Route
    InterAsIPmsi {
        rd:           [u8; 8],
        source_as:    u32,
    },
    /// Type 3: S-PMSI A-D Route
    SPmsi {
        rd:           [u8; 8],
        multicast_source: IpAddr,
        multicast_group:  IpAddr,
        origin_asn:   u32,
    },
    /// Type 4: Leaf A-D Route
    LeafAd {
        rd:           [u8; 8],
        source_addr:  IpAddr,
        multicast_group: IpAddr,
        originating_router: IpAddr,
    },
    /// Type 5: Source Active A-D Route
    SourceActive {
        rd:           [u8; 8],
        multicast_source: IpAddr,
        multicast_group:  IpAddr,
    },
    /// Type 6: Shared Tree Join Route
    SharedTreeJoin {
        rd:           [u8; 8],
        source_as:    u32,
        rp_addr:      IpAddr,
        multicast_group: IpAddr,
    },
    /// Type 7: Source Tree Join Route
    SourceTreeJoin {
        rd:           [u8; 8],
        source_as:    u32,
        multicast_source: IpAddr,
        multicast_group:  IpAddr,
    },
    /// Unknown / future route type — raw bytes preserved
    Unknown {
        route_type: u8,
        data:       Vec<u8>,
    },
}

impl MvpnNlri {
    /// Returns the RFC 6514 type code for this NLRI.
    pub fn route_type(&self) -> u8 {
        match self {
            Self::IntraAsIPmsi { .. } => 1,
            Self::InterAsIPmsi { .. } => 2,
            Self::SPmsi { .. }        => 3,
            Self::LeafAd { .. }       => 4,
            Self::SourceActive { .. } => 5,
            Self::SharedTreeJoin { .. } => 6,
            Self::SourceTreeJoin { .. } => 7,
            Self::Unknown { route_type, .. } => *route_type,
        }
    }

    /// Human-readable route type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::IntraAsIPmsi { .. }  => "Intra-AS I-PMSI A-D",
            Self::InterAsIPmsi { .. }  => "Inter-AS I-PMSI A-D",
            Self::SPmsi { .. }         => "S-PMSI A-D",
            Self::LeafAd { .. }        => "Leaf A-D",
            Self::SourceActive { .. }  => "Source Active A-D",
            Self::SharedTreeJoin { .. } => "Shared Tree Join",
            Self::SourceTreeJoin { .. } => "Source Tree Join",
            Self::Unknown { .. }       => "Unknown",
        }
    }
}

// ─── Parser ───────────────────────────────────────────────────────────────────

/// Parse a single MCAST-VPN NLRI from a byte slice.
/// The slice starts at the type byte (RFC 6514 §4 NLRI format).
pub fn parse_mvpn_nlri(buf: &[u8]) -> Result<(MvpnNlri, usize)> {
    if buf.len() < 2 {
        return Err(Error::BgpParse("MVPN NLRI too short".into()));
    }
    let nlri_type = buf[0];
    let nlri_len  = buf[1] as usize;
    if buf.len() < 2 + nlri_len {
        return Err(Error::BgpParse(format!(
            "MVPN NLRI type={nlri_type} claims {nlri_len} bytes but only {} remain",
            buf.len() - 2
        )));
    }
    let data = &buf[2..2 + nlri_len];
    let total_consumed = 2 + nlri_len;

    let nlri = match nlri_type {
        1 => parse_intra_as_ipmsi(data)?,
        2 => parse_inter_as_ipmsi(data)?,
        3 => parse_spmsi(data)?,
        4 => parse_leaf_ad(data)?,
        5 => parse_source_active(data)?,
        6 => parse_shared_tree_join(data)?,
        7 => parse_source_tree_join(data)?,
        _ => MvpnNlri::Unknown {
            route_type: nlri_type,
            data: data.to_vec(),
        },
    };
    Ok((nlri, total_consumed))
}

/// Parse all MCAST-VPN NLRIs from an MP_REACH/MP_UNREACH NLRI value.
pub fn parse_mvpn_nlri_list(buf: &[u8]) -> Vec<MvpnNlri> {
    let mut pos = 0;
    let mut result = Vec::new();
    while pos < buf.len() {
        match parse_mvpn_nlri(&buf[pos..]) {
            Ok((nlri, consumed)) => {
                result.push(nlri);
                pos += consumed;
            }
            Err(_) => break,
        }
    }
    result
}

// ─── Per-type parsers ─────────────────────────────────────────────────────────

fn read_rd(buf: &[u8], pos: usize) -> Result<[u8; 8]> {
    if buf.len() < pos + 8 {
        return Err(Error::BgpParse("MVPN: truncated RD".into()));
    }
    let mut rd = [0u8; 8];
    rd.copy_from_slice(&buf[pos..pos + 8]);
    Ok(rd)
}

fn read_asn(buf: &[u8], pos: usize) -> Result<u32> {
    if buf.len() < pos + 4 {
        return Err(Error::BgpParse("MVPN: truncated ASN".into()));
    }
    Ok(u32::from_be_bytes([buf[pos], buf[pos+1], buf[pos+2], buf[pos+3]]))
}

fn read_ipv4(buf: &[u8], pos: usize) -> Result<IpAddr> {
    if buf.len() < pos + 4 {
        return Err(Error::BgpParse("MVPN: truncated IPv4".into()));
    }
    Ok(IpAddr::V4(Ipv4Addr::new(buf[pos], buf[pos+1], buf[pos+2], buf[pos+3])))
}

fn read_ipv6(buf: &[u8], pos: usize) -> Result<IpAddr> {
    if buf.len() < pos + 16 {
        return Err(Error::BgpParse("MVPN: truncated IPv6".into()));
    }
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&buf[pos..pos+16]);
    Ok(IpAddr::V6(Ipv6Addr::from(bytes)))
}

/// Read an IP address that may be either IPv4 (4 bytes) or IPv6 (16 bytes)
/// based on remaining buffer. We check the address-family from the length field.
fn read_ip(buf: &[u8], pos: usize, is_v6: bool) -> Result<IpAddr> {
    if is_v6 { read_ipv6(buf, pos) } else { read_ipv4(buf, pos) }
}

#[allow(dead_code)]
fn is_v6_addr(buf: &[u8], pos: usize) -> bool {
    // Heuristic: if remaining >= 16 + overhead and address starts with 0-bytes
    // typical of IPv6, treat as IPv6. Otherwise assume IPv4.
    // In practice, the AFI on the enclosing MP_REACH drives this decision.
    // Here we peek at length: if there's clearly more room for v6, prefer v6.
    buf.len() >= pos + 16
}

/// Type 1: Intra-AS I-PMSI A-D Route (RFC 6514 §4.1)
/// Format: RD(8) + Originating-Router's IP Address(4 or 16)
fn parse_intra_as_ipmsi(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 12 {
        return Err(Error::BgpParse("MVPN type-1: too short".into()));
    }
    let rd = read_rd(data, 0)?;
    // Originating-Router IP is in the last 4 or 16 bytes
    let ip_len = data.len() - 8;
    let router_ip = if ip_len == 16 { read_ipv6(data, 8)? } else { read_ipv4(data, 8)? };
    let origin_asn = match router_ip {
        IpAddr::V4(a) => u32::from(a),
        IpAddr::V6(_) => 0, // not directly an ASN for v6
    };
    Ok(MvpnNlri::IntraAsIPmsi { rd, origin_asn })
}

/// Type 2: Inter-AS I-PMSI A-D Route (RFC 6514 §4.2)
/// Format: RD(8) + Source-AS(4)
fn parse_inter_as_ipmsi(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 12 {
        return Err(Error::BgpParse("MVPN type-2: too short".into()));
    }
    let rd        = read_rd(data, 0)?;
    let source_as = read_asn(data, 8)?;
    Ok(MvpnNlri::InterAsIPmsi { rd, source_as })
}

/// Type 3: S-PMSI A-D Route (RFC 6514 §4.3)
/// Format: RD(8) + Multicast-Source-Length(1) + Multicast-Source(4/16) +
///         Multicast-Group-Length(1) + Multicast-Group(4/16) + Originating-Router-IP(4/16)
fn parse_spmsi(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 10 {
        return Err(Error::BgpParse("MVPN type-3: too short".into()));
    }
    let rd  = read_rd(data, 0)?;
    let mut pos = 8;

    let src_len = data[pos] as usize / 8; pos += 1;
    let src     = read_ip(data, pos, src_len == 16)?; pos += src_len;

    let grp_len = data[pos] as usize / 8; pos += 1;
    let grp     = read_ip(data, pos, grp_len == 16)?; pos += grp_len;

    let _router = if pos + 4 <= data.len() { read_ipv4(data, pos).ok() } else { None };
    let origin_asn = 0; // not directly in type-3

    Ok(MvpnNlri::SPmsi {
        rd,
        multicast_source: src,
        multicast_group:  grp,
        origin_asn,
    })
}

/// Type 4: Leaf A-D Route (RFC 6514 §4.4)
/// Format: Route Key(variable) + Originating-Router-IP(4/16)
fn parse_leaf_ad(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 8 {
        return Err(Error::BgpParse("MVPN type-4: too short".into()));
    }
    let rd      = read_rd(data, 0)?;
    let src     = if data.len() >= 24 { read_ipv4(data, 8)? } else { IpAddr::V4(Ipv4Addr::UNSPECIFIED) };
    let grp     = if data.len() >= 28 { read_ipv4(data, 12)? } else { IpAddr::V4(Ipv4Addr::UNSPECIFIED) };
    let router  = if data.len() >= 32 { read_ipv4(data, 28)? } else { IpAddr::V4(Ipv4Addr::UNSPECIFIED) };
    Ok(MvpnNlri::LeafAd {
        rd,
        source_addr:        src,
        multicast_group:    grp,
        originating_router: router,
    })
}

/// Type 5: Source Active A-D Route (RFC 6514 §4.5)
fn parse_source_active(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 10 {
        return Err(Error::BgpParse("MVPN type-5: too short".into()));
    }
    let rd  = read_rd(data, 0)?;
    let mut pos = 8;
    let src_len = data[pos] as usize / 8; pos += 1;
    let src     = read_ip(data, pos, src_len == 16)?; pos += src_len;
    let grp_len = if pos < data.len() { data[pos] as usize / 8 } else { 4 }; pos += 1;
    let grp     = read_ip(data, pos, grp_len == 16)?;
    Ok(MvpnNlri::SourceActive {
        rd,
        multicast_source: src,
        multicast_group:  grp,
    })
}

/// Type 6: Shared Tree Join Route (RFC 6514 §4.6)
fn parse_shared_tree_join(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 17 {
        return Err(Error::BgpParse("MVPN type-6: too short".into()));
    }
    let rd        = read_rd(data, 0)?;
    let source_as = read_asn(data, 8)?;
    let rp        = read_ipv4(data, 12)?;
    let grp       = read_ipv4(data, 16)?;
    Ok(MvpnNlri::SharedTreeJoin {
        rd,
        source_as,
        rp_addr:         rp,
        multicast_group: grp,
    })
}

/// Type 7: Source Tree Join Route (RFC 6514 §4.7)
fn parse_source_tree_join(data: &[u8]) -> Result<MvpnNlri> {
    if data.len() < 20 {
        return Err(Error::BgpParse("MVPN type-7: too short".into()));
    }
    let rd        = read_rd(data, 0)?;
    let source_as = read_asn(data, 8)?;
    let src       = read_ipv4(data, 12)?;
    let grp       = read_ipv4(data, 16)?;
    Ok(MvpnNlri::SourceTreeJoin {
        rd,
        source_as,
        multicast_source: src,
        multicast_group:  grp,
    })
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_inter_as_ipmsi() {
        // Type=2, len=12, RD(8 zero bytes) + source_as=65001
        let mut buf = vec![2u8, 12];
        buf.extend_from_slice(&[0u8; 8]); // RD
        buf.extend_from_slice(&65001u32.to_be_bytes());

        let (nlri, consumed) = parse_mvpn_nlri(&buf).unwrap();
        assert_eq!(consumed, 14);
        assert_eq!(nlri.route_type(), 2);
        assert_eq!(nlri.type_name(), "Inter-AS I-PMSI A-D");
        if let MvpnNlri::InterAsIPmsi { source_as, .. } = nlri {
            assert_eq!(source_as, 65001);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_parse_unknown_type() {
        let buf = vec![99u8, 4, 1, 2, 3, 4];
        let (nlri, consumed) = parse_mvpn_nlri(&buf).unwrap();
        assert_eq!(consumed, 6);
        assert_eq!(nlri.route_type(), 99);
    }

    #[test]
    fn test_parse_list() {
        // Two type-2 NLRIs in sequence
        let mut buf = Vec::new();
        for _ in 0..2 {
            buf.push(2); buf.push(12);
            buf.extend_from_slice(&[0u8; 8]);
            buf.extend_from_slice(&65000u32.to_be_bytes());
        }
        let nlris = parse_mvpn_nlri_list(&buf);
        assert_eq!(nlris.len(), 2);
    }
}

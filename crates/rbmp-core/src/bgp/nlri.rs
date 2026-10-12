use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use ipnet::{Ipv4Net, Ipv6Net};
use bytes::Buf;
use crate::{Error, Result};
use super::types::{Afi, Prefix, RouteDistinguisher};
use smallvec::SmallVec;

/// Decode length-prefixed NLRI with optional Add-Path path_id (RFC 7911).
/// When `add_path` is true, each entry is: path_id(4) + prefix_len(1) + prefix_bytes.
/// When false, degrades to standard decode_nlri behaviour (path_id = None).
/// Returns Vec<(Prefix, Option<u32>)> — path_id is None when add_path is false.
pub fn decode_nlri_with_path_id(buf: &mut impl Buf, afi: Afi, add_path: bool) -> Result<Vec<(Prefix, Option<u32>)>> {
    let mut prefixes = Vec::new();
    while buf.remaining() > 0 {
        let path_id = if add_path {
            if buf.remaining() < 4 {
                return Err(Error::UnexpectedEof { needed: 4, have: buf.remaining() });
            }
            Some(buf.get_u32())
        } else {
            None
        };
        let prefix_len = buf.get_u8();
        let octets = (prefix_len as usize + 7) / 8;
        if buf.remaining() < octets {
            return Err(Error::UnexpectedEof { needed: octets, have: buf.remaining() });
        }
        let max_bits = match afi { Afi::Ipv6 => 128, _ => 32 };
        if prefix_len > max_bits {
            return Err(Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() });
        }
        let prefix = match afi {
            Afi::Ipv6 => {
                let mut addr = [0u8; 16];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv6Net::new(Ipv6Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V6(net.trunc())
            }
            _ => {
                let mut addr = [0u8; 4];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv4Net::new(Ipv4Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V4(net.trunc())
            }
        };
        prefixes.push((prefix, path_id));
    }
    Ok(prefixes)
}

/// Decode length-prefixed NLRI from a byte buffer.
/// Each entry is: 1 byte prefix_len, then ceil(prefix_len/8) prefix bytes.
/// afi selects IPv4 vs IPv6 interpretation.
pub fn decode_nlri(buf: &mut impl Buf, afi: Afi) -> Result<Vec<Prefix>> {
    let mut prefixes = Vec::new();
    while buf.remaining() > 0 {
        let prefix_len = buf.get_u8();
        let octets = (prefix_len as usize + 7) / 8;
        if buf.remaining() < octets {
            return Err(Error::UnexpectedEof { needed: octets, have: buf.remaining() });
        }
        let max_bits = match afi { Afi::Ipv6 => 128, _ => 32 };
        if prefix_len > max_bits {
            return Err(Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() });
        }
        let prefix = match afi {
            Afi::Ipv6 => {
                let mut addr = [0u8; 16];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv6Net::new(Ipv6Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V6(net.trunc())
            }
            _ => {
                let mut addr = [0u8; 4];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv4Net::new(Ipv4Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V4(net.trunc())
            }
        };
        prefixes.push(prefix);
    }
    Ok(prefixes)
}

/// Decode a labeled unicast NLRI (RFC 3107 / RFC 8277).
/// Format: prefix_len(8) | label(24) ... | prefix_bits
/// The prefix_len field includes the label bits.
pub fn decode_labeled_nlri(buf: &mut impl Buf, afi: Afi) -> Result<Vec<Prefix>> {
    let mut prefixes = Vec::new();
    while buf.remaining() > 0 {
        let total_len = buf.get_u8() as usize;
        // Each label = 24 bits; read labels until bottom-of-stack bit set
        let mut labels: SmallVec<[u32; 2]> = SmallVec::new();
        loop {
            if buf.remaining() < 3 {
                return Err(Error::UnexpectedEof { needed: 3, have: buf.remaining() });
            }
            let b = buf.copy_to_bytes(3);
            let raw = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            let label = raw >> 4;
            let bos = raw & 0x01 != 0;
            labels.push(label);
            if bos { break; }
        }
        // remaining bits = total_len - 24*label_count
        let label_bits = labels.len() * 24;
        if total_len < label_bits {
            return Err(Error::BgpParse(format!(
                "labeled NLRI bit length {total_len} smaller than label stack bits {label_bits}"
            )));
        }
        let prefix_len = (total_len - label_bits) as u8;
        let octets = (prefix_len as usize + 7) / 8;
        if buf.remaining() < octets {
            return Err(Error::UnexpectedEof { needed: octets, have: buf.remaining() });
        }
        let inner = match afi {
            Afi::Ipv6 => {
                let mut addr = [0u8; 16];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv6Net::new(Ipv6Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V6(net.trunc())
            }
            _ => {
                let mut addr = [0u8; 4];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv4Net::new(Ipv4Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V4(net.trunc())
            }
        };
        prefixes.push(Prefix::Labeled { prefix: Box::new(inner), labels });
    }
    Ok(prefixes)
}

/// Decode a VPN (L3VPN) NLRI (RFC 4364).
/// Format: prefix_len(8) | label(24) | RD(64) | prefix_bits
pub fn decode_vpn_nlri(buf: &mut impl Buf, afi: Afi) -> Result<Vec<Prefix>> {
    let mut prefixes = Vec::new();
    while buf.remaining() > 0 {
        let total_len = buf.get_u8() as usize;
        // Read 1 label (24 bits)
        if buf.remaining() < 3 {
            return Err(Error::UnexpectedEof { needed: 3, have: buf.remaining() });
        }
        let b = buf.copy_to_bytes(3);
        let raw = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        let label = raw >> 4;
        let labels: SmallVec<[u32; 2]> = SmallVec::from_slice(&[label]);
        // Read RD (8 bytes)
        if buf.remaining() < 8 {
            return Err(Error::UnexpectedEof { needed: 8, have: buf.remaining() });
        }
        let rd_bytes: [u8; 8] = buf.copy_to_bytes(8).as_ref().try_into().unwrap();
        let rd = RouteDistinguisher(rd_bytes);
        // prefix_len covers label(24) + RD(64) + actual prefix bits
        if total_len < 24 + 64 {
            return Err(Error::BgpParse(format!(
                "VPN NLRI bit length {total_len} smaller than 88-bit label+RD header"
            )));
        }
        let prefix_len = (total_len - (24 + 64)) as u8;
        let octets = (prefix_len as usize + 7) / 8;
        if buf.remaining() < octets {
            return Err(Error::UnexpectedEof { needed: octets, have: buf.remaining() });
        }
        let inner = match afi {
            Afi::Ipv6 => {
                let mut addr = [0u8; 16];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv6Net::new(Ipv6Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V6(net.trunc())
            }
            _ => {
                let mut addr = [0u8; 4];
                let chunk = buf.copy_to_bytes(octets);
                addr[..octets].copy_from_slice(&chunk);
                let net = Ipv4Net::new(Ipv4Addr::from(addr), prefix_len)
                    .map_err(|_| Error::InvalidPrefixLen { prefix_len, afi: afi.as_u16() })?;
                Prefix::V4(net.trunc())
            }
        };
        prefixes.push(Prefix::Vpn { rd, prefix: Box::new(inner), labels });
    }
    Ok(prefixes)
}

/// Decode VPLS NLRI (RFC 4761 §3.2.2, AFI=25 SAFI=65).
///
/// Format per NLRI entry:
///   length(2) | RD(8) | VE-ID(2) | VE-Block-Offset(2) | VE-Block-Size(2) | label-base(3)
pub fn decode_vpls_nlri(buf: &[u8]) -> Result<Vec<super::types::VplsNlri>> {
    use super::types::VplsNlri;
    let mut cur = std::io::Cursor::new(buf);
    let mut result = Vec::new();
    while cur.remaining() >= 2 {
        let nlri_len = cur.get_u16() as usize;
        if cur.remaining() < nlri_len { break; }
        let nlri = cur.copy_to_bytes(nlri_len);
        // Minimum: RD(8) + VE-ID(2) + VE-Block-Offset(2) + VE-Block-Size(2) + label(3) = 17
        if nlri.len() < 17 { continue; }
        let mut rd = [0u8; 8];
        rd.copy_from_slice(&nlri[..8]);
        let ve_id           = u16::from_be_bytes([nlri[8],  nlri[9]]);
        let ve_block_offset = u16::from_be_bytes([nlri[10], nlri[11]]);
        let ve_block_size   = u16::from_be_bytes([nlri[12], nlri[13]]);
        // label base: 3 bytes, label in upper 20 bits
        let label_raw = u32::from_be_bytes([0, nlri[14], nlri[15], nlri[16]]);
        let label_base = label_raw >> 4;
        result.push(VplsNlri { rd, ve_id, ve_block_offset, ve_block_size, label_base });
    }
    Ok(result)
}

/// Decode MP_REACH next-hop address(es).
/// Returns a Vec because IPv6 can have link-local second hop.
pub fn decode_next_hops(buf: &mut impl Buf, afi: Afi) -> Result<Vec<IpAddr>> {
    if buf.remaining() < 1 {
        return Err(Error::UnexpectedEof { needed: 1, have: 0 });
    }
    let nh_len = buf.get_u8() as usize;
    if buf.remaining() < nh_len {
        return Err(Error::UnexpectedEof { needed: nh_len, have: buf.remaining() });
    }
    let nh_bytes = buf.copy_to_bytes(nh_len);
    let mut hops = Vec::new();
    let hop_size = match afi { Afi::Ipv6 => 16, _ => 4 };
    if nh_len == 0 || nh_len % hop_size != 0 {
        return Err(Error::BgpParse(format!(
            "invalid MP_REACH next-hop length {nh_len} for AFI {}",
            afi.as_u16()
        )));
    }
    let mut pos = 0;
    while pos + hop_size <= nh_bytes.len() {
        let addr = match afi {
            Afi::Ipv6 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(&nh_bytes[pos..pos + 16]);
                IpAddr::V6(Ipv6Addr::from(b))
            }
            _ => {
                let mut b = [0u8; 4];
                b.copy_from_slice(&nh_bytes[pos..pos + 4]);
                IpAddr::V4(Ipv4Addr::from(b))
            }
        };
        hops.push(addr);
        pos += hop_size;
    }
    Ok(hops)
}

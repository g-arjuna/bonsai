use std::net::Ipv4Addr;
use bytes::Buf;
use crate::{Error, Result};
use super::types::BgpCapability;
use super::capabilities::parse_capabilities;
use crate::bmp::types::BgpOpenInfo;

const BGP_OPEN_TYPE: u8 = 1;

/// Parse a BGP OPEN message. `buf` starts at the BGP common header (19 bytes: 16 marker + 2 len + 1 type).
pub fn parse_bgp_open(mut buf: impl Buf) -> Result<BgpOpenInfo> {
    if buf.remaining() < 19 {
        return Err(Error::UnexpectedEof {
            needed: 19,
            have: buf.remaining(),
        });
    }
    // Validate BGP marker (16 × 0xFF)
    let marker: bytes::Bytes = buf.copy_to_bytes(16);
    if marker.as_ref() != &[0xFF_u8; 16] {
        return Err(Error::InvalidBgpMarker);
    }
    let msg_len = buf.get_u16() as usize;
    let msg_type = buf.get_u8();
    if msg_type != BGP_OPEN_TYPE {
        return Err(Error::InvalidBgpMessageType(msg_type));
    }
    if !(29..=4096).contains(&msg_len) {
        return Err(Error::BgpParse(format!(
            "invalid BGP OPEN declared length {msg_len} (must be 29..=4096)"
        )));
    }
    let body_len = msg_len - 19;
    if buf.remaining() < body_len {
        return Err(Error::UnexpectedEof {
            needed: msg_len,
            have: 19 + buf.remaining(),
        });
    }
    if buf.remaining() > body_len {
        return Err(Error::BgpParse(format!(
            "BGP OPEN buffer length {} exceeds declared length {}",
            19 + buf.remaining(),
            msg_len
        )));
    }
    // BGP OPEN body: version(1) + my_as(2) + hold_time(2) + bgp_id(4) + opt_params_len(1) + params
    let version       = buf.get_u8();
    let my_asn        = buf.get_u16() as u32; // 2-byte ASN; 4-byte overrides via capability
    let hold_time     = buf.get_u16();
    let bgp_id_raw    = buf.copy_to_bytes(4);
    let bgp_id        = Ipv4Addr::from([bgp_id_raw[0], bgp_id_raw[1], bgp_id_raw[2], bgp_id_raw[3]]);
    let opt_params_len = buf.get_u8() as usize;
    if 10 + opt_params_len != body_len {
        return Err(Error::BgpParse(format!(
            "BGP OPEN opt_params_len {opt_params_len} does not match declared body length {body_len}"
        )));
    }
    let params = buf.copy_to_bytes(opt_params_len);
    let capabilities = parse_capabilities(params.as_ref())?;
    // Upgrade 2-byte ASN if 4-byte capability present
    let asn = capabilities.iter().find_map(|c| {
        if let BgpCapability::FourByteAsn(asn) = c { Some(*asn) } else { None }
    }).unwrap_or(my_asn);
    Ok(BgpOpenInfo { version, asn, hold_time, bgp_id, capabilities })
}

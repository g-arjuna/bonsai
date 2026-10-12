use bytes::Buf;
use crate::{Error, Result};
use super::types::{Afi, AfiSafi, BgpDecodeContext, BgpUpdate};
use super::nlri::decode_nlri_with_path_id;
use super::attributes::parse_path_attributes_with_context;

const BGP_UPDATE_TYPE: u8 = 2;

/// Parse a BGP UPDATE message.
/// `buf` must start at the BGP common header (16-byte marker + 2-byte length + 1-byte type).
/// `add_path_ipv4` — set true when the peer negotiated Add-Path for IPv4 unicast (RFC 7911).
pub fn parse_bgp_update(buf: impl Buf, add_path_ipv4: bool) -> Result<BgpUpdate> {
    parse_bgp_update_with_context(buf, &BgpDecodeContext::with_add_path_ipv4(add_path_ipv4))
}

/// Parse a BGP UPDATE message using session-derived decoding context (`BgpDecodeContext`).
pub fn parse_bgp_update_with_context(mut buf: impl Buf, ctx: &BgpDecodeContext) -> Result<BgpUpdate> {
    if buf.remaining() < 19 {
        return Err(Error::UnexpectedEof {
            needed: 19,
            have: buf.remaining(),
        });
    }
    // Validate marker
    let marker = buf.copy_to_bytes(16);
    if marker.as_ref() != &[0xFF_u8; 16] {
        return Err(Error::InvalidBgpMarker);
    }
    let msg_len  = buf.get_u16() as usize;
    let msg_type = buf.get_u8();
    if msg_type != BGP_UPDATE_TYPE {
        return Err(Error::InvalidBgpMessageType(msg_type));
    }
    if !(23..=4096).contains(&msg_len) {
        return Err(Error::BgpParse(format!(
            "invalid BGP UPDATE declared length {msg_len} (must be 23..=4096)"
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
            "BGP UPDATE buffer length {} exceeds declared length {}",
            19 + buf.remaining(),
            msg_len
        )));
    }
    let body_bytes = buf.copy_to_bytes(body_len);
    let mut body = body_bytes.as_ref();
    let add_path_ipv4 = ctx.has_add_path(AfiSafi::ipv4_unicast());

    // Withdrawn routes (IPv4 unicast NLRI)
    if body.remaining() < 2 {
        return Err(Error::UnexpectedEof { needed: 2, have: body.remaining() });
    }
    let withdrawn_len = body.get_u16() as usize;
    if body.remaining() < withdrawn_len {
        return Err(Error::UnexpectedEof { needed: withdrawn_len, have: body.remaining() });
    }
    let withdrawn_bytes = body.copy_to_bytes(withdrawn_len);
    let withdrawn_with_ids = decode_nlri_with_path_id(
        &mut withdrawn_bytes.as_ref(), Afi::Ipv4, add_path_ipv4,
    )?;
    let (withdrawn, withdrawn_path_ids): (Vec<_>, Vec<_>) = withdrawn_with_ids.into_iter().unzip();

    // Path attributes
    if body.remaining() < 2 {
        return Err(Error::UnexpectedEof { needed: 2, have: body.remaining() });
    }
    let attr_len = body.get_u16() as usize;
    if body.remaining() < attr_len {
        return Err(Error::UnexpectedEof { needed: attr_len, have: body.remaining() });
    }
    let attr_bytes = body.copy_to_bytes(attr_len);
    let attributes = parse_path_attributes_with_context(attr_bytes.as_ref(), ctx)?;

    // Announced routes (remaining NLRI = IPv4 unicast)
    let announced_with_ids = decode_nlri_with_path_id(&mut body, Afi::Ipv4, add_path_ipv4)?;
    let (announced, announced_path_ids): (Vec<_>, Vec<_>) = announced_with_ids.into_iter().unzip();

    Ok(BgpUpdate { withdrawn, withdrawn_path_ids, attributes, announced, announced_path_ids })
}

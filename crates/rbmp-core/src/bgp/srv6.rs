use crate::Result;
use super::types::{PrefixSid, Srv6L3Service, Srv6SubSubTlv};

/// Parse a BGP Prefix-SID attribute (RFC 8669, type 40).
/// TLV-encoded: TLV-type(1) + TLV-length(2) + TLV-value
pub fn parse_prefix_sid(buf: &[u8]) -> Result<PrefixSid> {
    let mut label_index   = None;
    let mut originator_srgb = None;
    let mut srv6_l3_service = None;
    let mut raw_tlvs      = Vec::new();

    let mut pos = 0;
    while pos + 3 <= buf.len() {
        let tlv_type = buf[pos];
        let tlv_len  = u16::from_be_bytes([buf[pos+1], buf[pos+2]]) as usize;
        pos += 3;
        if pos + tlv_len > buf.len() { break; }
        let tlv_data = &buf[pos..pos+tlv_len];
        pos += tlv_len;

        match tlv_type {
            1 => {
                // Label Index TLV (RFC 8669 §3.1): flags(1) + reserved(1) + label_index(4)
                if tlv_data.len() >= 6 {
                    label_index = Some(u32::from_be_bytes([
                        tlv_data[2], tlv_data[3], tlv_data[4], tlv_data[5]
                    ]));
                }
            }
            3 => {
                // Originator SRGB TLV: flags(2) + SRGB entries (3-byte base + 3-byte range)
                if tlv_data.len() >= 2 {
                    let flags = u16::from_be_bytes([tlv_data[0], tlv_data[1]]);
                    let mut srgbs = Vec::new();
                    let mut i = 2;
                    while i + 6 <= tlv_data.len() {
                        let base  = u32::from_be_bytes([0, tlv_data[i], tlv_data[i+1], tlv_data[i+2]]);
                        let range = u32::from_be_bytes([0, tlv_data[i+3], tlv_data[i+4], tlv_data[i+5]]);
                        srgbs.push((base, range));
                        i += 6;
                    }
                    originator_srgb = Some((flags, srgbs));
                }
            }
            5 => {
                // SRv6 L3 Service TLV
                srv6_l3_service = Some(parse_srv6_l3_service(tlv_data)?);
            }
            _ => {
                raw_tlvs.push((tlv_type, tlv_data.to_vec()));
            }
        }
    }

    Ok(PrefixSid { label_index, originator_srgb, srv6_l3_service, raw_tlvs })
}

/// Parse the SRv6 L3 Service TLV (RFC 9252 §3.2).
pub fn parse_srv6_l3_service(buf: &[u8]) -> Result<Srv6L3Service> {
    let mut sub_sub_tlvs = Vec::new();
    // Sub-TLV loop: reserved(1) + sub-sub-TLVs
    let mut pos = 1; // skip reserved byte
    while pos + 3 <= buf.len() {
        let sub_type = buf[pos];
        let sub_len  = u16::from_be_bytes([buf[pos+1], buf[pos+2]]) as usize;
        pos += 3;
        if pos + sub_len > buf.len() { break; }
        let sub_data = &buf[pos..pos+sub_len];
        pos += sub_len;

        if sub_type == 1 {
            // SRv6 SID Information Sub-Sub-TLV: SID(16) + flags(1) + endpoint_behavior(2) + sub-sub-TLVs
            if sub_data.len() >= 19 {
                let mut sid = [0u8; 16];
                sid.copy_from_slice(&sub_data[0..16]);
                let sid_flags          = sub_data[16];
                let endpoint_behavior  = u16::from_be_bytes([sub_data[17], sub_data[18]]);
                sub_sub_tlvs.push(Srv6SubSubTlv { sid, sid_flags, endpoint_behavior });
            }
        }
    }
    Ok(Srv6L3Service { sub_sub_tlvs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_sid_label_index() {
        // TLV type=1, len=6, flags(1)+reserved(1)+label_index(4)=42
        let buf = [1u8, 0, 6, 0, 0, 0, 0, 0, 42];
        let sid = parse_prefix_sid(&buf).unwrap();
        assert_eq!(sid.label_index, Some(42));
        assert!(sid.srv6_l3_service.is_none());
    }

    #[test]
    fn test_prefix_sid_unknown_tlv_preserved() {
        // TLV type=99, len=2, data
        let buf = [99u8, 0, 2, 0xDE, 0xAD];
        let sid = parse_prefix_sid(&buf).unwrap();
        assert_eq!(sid.raw_tlvs.len(), 1);
        assert_eq!(sid.raw_tlvs[0].0, 99);
    }
}

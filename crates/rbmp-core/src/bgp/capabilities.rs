use super::types::{Afi, AfiSafi, BgpCapability, Safi};
use crate::{Error, Result};
use bytes::Buf;

/// Parse BGP capabilities from the Optional Parameters section of a BGP OPEN.
/// buf should contain the raw optional parameters bytes (after the opt_params_len field).
pub fn parse_capabilities(mut buf: impl Buf) -> Result<Vec<BgpCapability>> {
    let mut caps = Vec::new();
    while buf.remaining() > 0 {
        if buf.remaining() < 2 {
            return Err(Error::UnexpectedEof {
                needed: 2,
                have: buf.remaining(),
            });
        }
        let param_type = buf.get_u8();
        let param_len = buf.get_u8() as usize;
        if buf.remaining() < param_len {
            return Err(Error::UnexpectedEof {
                needed: param_len,
                have: buf.remaining(),
            });
        }
        let mut param_buf = buf.copy_to_bytes(param_len);
        if param_type != 2 {
            // Not a capability parameter, skip
            continue;
        }
        // param_type == 2: Capabilities Optional Parameter
        while param_buf.remaining() > 0 {
            if param_buf.remaining() < 2 {
                return Err(Error::UnexpectedEof {
                    needed: 2,
                    have: param_buf.remaining(),
                });
            }
            let cap_code = param_buf.get_u8();
            let cap_len = param_buf.get_u8() as usize;
            if param_buf.remaining() < cap_len {
                return Err(Error::UnexpectedEof {
                    needed: cap_len,
                    have: param_buf.remaining(),
                });
            }
            let cap_data = param_buf.copy_to_bytes(cap_len);
            let cap = parse_one_capability(cap_code, &cap_data)?;
            caps.push(cap);
        }
    }
    Ok(caps)
}

fn parse_one_capability(code: u8, data: &[u8]) -> Result<BgpCapability> {
    match code {
        // Multiprotocol (RFC 4760)
        1 => {
            if data.len() != 4 {
                return Err(Error::UnexpectedEof {
                    needed: 4,
                    have: data.len(),
                });
            }
            let afi = u16::from_be_bytes([data[0], data[1]]);
            let safi = data[3];
            Ok(BgpCapability::Multiprotocol(AfiSafi::new(afi, safi)))
        }
        // Route Refresh (RFC 2918)
        2 => Ok(BgpCapability::RouteRefresh),
        // Extended Next-Hop Encoding (RFC 8950)
        5 => {
            if data.len() < 6 {
                return Err(Error::UnexpectedEof {
                    needed: 6,
                    have: data.len(),
                });
            }
            let afi = Afi::from(u16::from_be_bytes([data[0], data[1]]));
            let safi = Safi::from(data[3]);
            let next_hop_afi = Afi::from(u16::from_be_bytes([data[4], data[5]]));
            Ok(BgpCapability::ExtendedNextHop {
                afi,
                safi,
                next_hop_afi,
            })
        }
        // Extended Message (RFC 8654)
        6 => Ok(BgpCapability::ExtendedMessage),
        // Graceful Restart (RFC 4724)
        64 => {
            if data.len() < 2 {
                return Err(Error::UnexpectedEof {
                    needed: 2,
                    have: data.len(),
                });
            }
            let restart_time = u16::from_be_bytes([data[0], data[1]]) & 0x0FFF;
            let mut afi_safis = Vec::new();
            let mut i = 2;
            while i + 3 <= data.len() {
                let afi = u16::from_be_bytes([data[i], data[i + 1]]);
                let safi = data[i + 2];
                let flags = if i + 3 < data.len() { data[i + 3] } else { 0 };
                afi_safis.push((AfiSafi::new(afi, safi), flags));
                i += 4;
            }
            Ok(BgpCapability::GracefulRestart {
                restart_time,
                afi_safis,
            })
        }
        // 4-byte ASN (RFC 6793)
        65 => {
            if data.len() != 4 {
                return Err(Error::UnexpectedEof {
                    needed: 4,
                    have: data.len(),
                });
            }
            let asn = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            Ok(BgpCapability::FourByteAsn(asn))
        }
        // Add-Path (RFC 7911)
        69 => {
            if !data.len().is_multiple_of(4) {
                return Err(Error::BgpParse(format!(
                    "invalid Add-Path capability length {} (must be multiple of 4)",
                    data.len()
                )));
            }
            let mut entries = Vec::new();
            let mut i = 0;
            while i + 4 <= data.len() {
                let afi = u16::from_be_bytes([data[i], data[i + 1]]);
                let safi = data[i + 2];
                let mode = data[i + 3];
                entries.push((AfiSafi::new(afi, safi), mode));
                i += 4;
            }
            Ok(BgpCapability::AddPath(entries))
        }
        // Enhanced Route Refresh (RFC 7313)
        70 => Ok(BgpCapability::EnhancedRouteRefresh),
        // Long-Lived Graceful Restart (RFC 9494)
        71 => Ok(BgpCapability::LongLivedGracefulRestart {
            entries: data.to_vec(),
        }),
        // BGP Role (RFC 9234): role byte 0=Provider, 1=RS, 2=RS-Client, 3=Customer, 4=Peer
        9 => {
            if data.is_empty() {
                return Err(Error::UnexpectedEof { needed: 1, have: 0 });
            }
            Ok(BgpCapability::BgpRole(data[0]))
        }
        // FQDN (draft-walton-bgp-hostname-capability)
        73 => {
            if data.is_empty() {
                return Err(Error::UnexpectedEof { needed: 1, have: 0 });
            }
            let hostname_len = data[0] as usize;
            if data.len() < 1 + hostname_len {
                return Err(Error::UnexpectedEof {
                    needed: 1 + hostname_len,
                    have: data.len(),
                });
            }
            let hostname = String::from_utf8_lossy(&data[1..1 + hostname_len]).to_string();
            let mut domain = String::new();
            if data.len() > 1 + hostname_len {
                let domain_len = data[1 + hostname_len] as usize;
                let start = 2 + hostname_len;
                if data.len() < start + domain_len {
                    return Err(Error::UnexpectedEof {
                        needed: start + domain_len,
                        have: data.len(),
                    });
                }
                domain = String::from_utf8_lossy(&data[start..start + domain_len]).to_string();
            }
            Ok(BgpCapability::Fqdn { hostname, domain })
        }
        _ => Ok(BgpCapability::Unknown {
            code,
            data: data.to_vec(),
        }),
    }
}

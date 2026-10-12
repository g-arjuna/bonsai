use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

/// RFC 7432 §7 — EVPN route types (AFI=25, SAFI=70)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvpnRoute {
    /// Type 1: Ethernet Auto-Discovery (A-D) route
    EthernetAutoDiscovery {
        rd:           [u8; 8],
        esi:          [u8; 10],
        ethernet_tag: u32,
        mpls_label:   u32,
    },
    /// Type 2: MAC/IP Advertisement route
    MacIpAdvertisement {
        rd:           [u8; 8],
        esi:          [u8; 10],
        ethernet_tag: u32,
        mac:          [u8; 6],
        ip:           Option<IpAddr>,
        mpls_label1:  u32,
        mpls_label2:  Option<u32>,
    },
    /// Type 3: Inclusive Multicast Ethernet Tag route
    InclusiveMulticastEthernetTag {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        originating_router_ip: IpAddr,
    },
    /// Type 4: Ethernet Segment route
    EthernetSegment {
        rd:                    [u8; 8],
        esi:                   [u8; 10],
        originating_router_ip: IpAddr,
    },
    /// Type 5: IP Prefix route (RFC 9136)
    IpPrefix {
        rd:           [u8; 8],
        esi:          [u8; 10],
        ethernet_tag: u32,
        prefix:       IpAddr,
        prefix_len:   u8,
        gw_ip:        Option<IpAddr>,
        mpls_label:   u32,
    },
    /// Type 6: Selective Multicast Ethernet Tag A-D route (RFC 8365 §6.3)
    SelectiveMulticastEthernetTag {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        multicast_source:      IpAddr,
        multicast_group:       IpAddr,
        originating_router_ip: IpAddr,
    },
    /// Type 7: IGMP Join Synch A-D route (RFC 8365 §11.2)
    IgmpJoinSynch {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        multicast_source:      IpAddr,
        multicast_group:       IpAddr,
        originating_router_ip: IpAddr,
    },
    /// Type 8: IGMP Leave Synch A-D route (RFC 8365 §11.2)
    IgmpLeaveSynch {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        multicast_source:      IpAddr,
        multicast_group:       IpAddr,
        originating_router_ip: IpAddr,
    },
    /// Type 9: Per-Region I-PMSI A-D route (RFC 9251)
    PerRegionIPmsi {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        originating_router_ip: IpAddr,
    },
    /// Type 10: S-PMSI A-D route (RFC 9251)
    SPmsi {
        rd:                    [u8; 8],
        ethernet_tag:          u32,
        multicast_source:      IpAddr,
        multicast_group:       IpAddr,
        originating_router_ip: IpAddr,
    },
    /// Type 11: Leaf A-D route (RFC 9572)
    LeafAD {
        route_key: Vec<u8>,
        path_id:   u32,
    },
    /// Unknown type — preserved for forward compatibility
    Unknown { route_type: u8, data: Vec<u8> },
}

impl EvpnRoute {
    pub fn route_type_name(&self) -> &'static str {
        match self {
            Self::EthernetAutoDiscovery { .. }        => "ethernet-auto-discovery",
            Self::MacIpAdvertisement { .. }            => "mac-ip-advertisement",
            Self::InclusiveMulticastEthernetTag { .. } => "inclusive-multicast-ethernet-tag",
            Self::EthernetSegment { .. }               => "ethernet-segment",
            Self::IpPrefix { .. }                      => "ip-prefix",
            Self::SelectiveMulticastEthernetTag { .. } => "selective-multicast-ethernet-tag",
            Self::IgmpJoinSynch { .. }                 => "igmp-join-synch",
            Self::IgmpLeaveSynch { .. }                => "igmp-leave-synch",
            Self::PerRegionIPmsi { .. }                => "per-region-i-pmsi",
            Self::SPmsi { .. }                         => "s-pmsi",
            Self::LeafAD { .. }                        => "leaf-ad",
            Self::Unknown { .. }                       => "unknown",
        }
    }

    pub fn route_type_code(&self) -> u8 {
        match self {
            Self::EthernetAutoDiscovery { .. }        => 1,
            Self::MacIpAdvertisement { .. }            => 2,
            Self::InclusiveMulticastEthernetTag { .. } => 3,
            Self::EthernetSegment { .. }               => 4,
            Self::IpPrefix { .. }                      => 5,
            Self::SelectiveMulticastEthernetTag { .. } => 6,
            Self::IgmpJoinSynch { .. }                 => 7,
            Self::IgmpLeaveSynch { .. }                => 8,
            Self::PerRegionIPmsi { .. }                => 9,
            Self::SPmsi { .. }                         => 10,
            Self::LeafAD { .. }                        => 11,
            Self::Unknown { route_type, .. }           => *route_type,
        }
    }
}

/// Decode EVPN NLRI from MP_REACH or MP_UNREACH attribute body.
/// Wire format per RFC 7432: Route-Type(1) + Length(1) + Value(Length)
pub fn decode_evpn_nlri(mut buf: &[u8]) -> Result<Vec<EvpnRoute>> {
    let mut routes = Vec::new();
    while buf.len() >= 2 {
        let route_type = buf[0];
        let length     = buf[1] as usize;
        buf = &buf[2..];
        if buf.len() < length {
            return Err(Error::UnexpectedEof { needed: length, have: buf.len() });
        }
        let value = &buf[..length];
        buf = &buf[length..];
        routes.push(parse_evpn_route(route_type, value)?);
    }
    Ok(routes)
}

fn parse_evpn_route(route_type: u8, v: &[u8]) -> Result<EvpnRoute> {
    match route_type {
        1 => {
            // Type 1: RD(8) + ESI(10) + ETag(4) + Label(3) = 25 bytes
            if v.len() < 25 {
                return Err(Error::UnexpectedEof { needed: 25, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let mut esi = [0u8; 10]; esi.copy_from_slice(&v[8..18]);
            let ethernet_tag = u32::from_be_bytes([v[18], v[19], v[20], v[21]]);
            let mpls_label   = decode_mpls_label(&v[22..25]);
            Ok(EvpnRoute::EthernetAutoDiscovery { rd, esi, ethernet_tag, mpls_label })
        }
        2 => {
            // Type 2: RD(8)+ESI(10)+ETag(4)+MAClen(1)+MAC(6)+IPlen(1)+IP(0/4/16)+Label1(3)+Label2(3)?
            if v.len() < 33 {
                return Err(Error::UnexpectedEof { needed: 33, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let mut esi = [0u8; 10]; esi.copy_from_slice(&v[8..18]);
            let ethernet_tag = u32::from_be_bytes([v[18], v[19], v[20], v[21]]);
            let mac_len = v[22];
            if mac_len != 48 {
                return Err(Error::BgpParse(format!("EVPN type2 mac_len={mac_len}, expected 48")));
            }
            let mut mac = [0u8; 6]; mac.copy_from_slice(&v[23..29]);
            let ip_len  = v[29];
            let mut pos = 30;
            let ip = match ip_len {
                0 => None,
                32 => {
                    if v.len() < pos + 4 {
                        return Err(Error::UnexpectedEof { needed: pos + 4, have: v.len() });
                    }
                    let a = IpAddr::V4(Ipv4Addr::from([v[pos], v[pos+1], v[pos+2], v[pos+3]]));
                    pos += 4;
                    Some(a)
                }
                128 => {
                    if v.len() < pos + 16 {
                        return Err(Error::UnexpectedEof { needed: pos + 16, have: v.len() });
                    }
                    let mut b = [0u8; 16]; b.copy_from_slice(&v[pos..pos+16]);
                    pos += 16;
                    Some(IpAddr::V6(Ipv6Addr::from(b)))
                }
                _ => return Err(Error::BgpParse(format!("EVPN type2 ip_len={ip_len}"))),
            };
            if v.len() < pos + 3 {
                return Err(Error::UnexpectedEof { needed: pos + 3, have: v.len() });
            }
            let mpls_label1 = decode_mpls_label(&v[pos..pos+3]);
            let mpls_label2 = if v.len() >= pos + 6 { Some(decode_mpls_label(&v[pos+3..pos+6])) } else { None };
            Ok(EvpnRoute::MacIpAdvertisement { rd, esi, ethernet_tag, mac, ip, mpls_label1, mpls_label2 })
        }
        3 => {
            // Type 3: RD(8) + ETag(4) + IPlen(1) + IP(4 or 16)
            if v.len() < 13 {
                return Err(Error::UnexpectedEof { needed: 13, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let ethernet_tag = u32::from_be_bytes([v[8], v[9], v[10], v[11]]);
            let ip_len = v[12];
            let originating_router_ip = decode_evpn_router_ip(ip_len, &v[13..])?;
            Ok(EvpnRoute::InclusiveMulticastEthernetTag { rd, ethernet_tag, originating_router_ip })
        }
        4 => {
            // Type 4: RD(8) + ESI(10) + IPlen(1) + IP(4 or 16)
            if v.len() < 19 {
                return Err(Error::UnexpectedEof { needed: 19, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let mut esi = [0u8; 10]; esi.copy_from_slice(&v[8..18]);
            let ip_len = v[18];
            let originating_router_ip = decode_evpn_router_ip(ip_len, &v[19..])?;
            Ok(EvpnRoute::EthernetSegment { rd, esi, originating_router_ip })
        }
        5 => {
            // Type 5: RD(8)+ESI(10)+ETag(4)+IPlen(1)+IP(4/16)+GW_IP(4/16)+Label(3)
            if v.len() < 34 {
                return Err(Error::UnexpectedEof { needed: 34, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let mut esi = [0u8; 10]; esi.copy_from_slice(&v[8..18]);
            let ethernet_tag = u32::from_be_bytes([v[18], v[19], v[20], v[21]]);
            let prefix_len = v[22];
            let ip_octets: usize = if prefix_len <= 32 { 4 } else { 16 };
            if v.len() < 23 + ip_octets * 2 + 3 {
                return Err(Error::UnexpectedEof { needed: 23 + ip_octets * 2 + 3, have: v.len() });
            }
            let prefix = if ip_octets == 4 {
                IpAddr::V4(Ipv4Addr::from([v[23], v[24], v[25], v[26]]))
            } else {
                let mut b = [0u8; 16]; b.copy_from_slice(&v[23..39]);
                IpAddr::V6(Ipv6Addr::from(b))
            };
            let gw_off = 23 + ip_octets;
            let gw_ip = if v[gw_off..gw_off + ip_octets].iter().all(|&b| b == 0) {
                None
            } else if ip_octets == 4 {
                Some(IpAddr::V4(Ipv4Addr::from([v[gw_off], v[gw_off+1], v[gw_off+2], v[gw_off+3]])))
            } else {
                let mut b = [0u8; 16]; b.copy_from_slice(&v[gw_off..gw_off+16]);
                Some(IpAddr::V6(Ipv6Addr::from(b)))
            };
            let label_off = gw_off + ip_octets;
            let mpls_label = decode_mpls_label(&v[label_off..label_off + 3]);
            Ok(EvpnRoute::IpPrefix { rd, esi, ethernet_tag, prefix, prefix_len, gw_ip, mpls_label })
        }
        6 | 7 | 8 => {
            // Types 6,7,8: RD(8) + ETag(4) + src_ip_len(1) + src_ip(4/16)
            //             + grp_ip_len(1) + grp_ip(4/16) + orig_ip_len(1) + orig_ip(4/16)
            if v.len() < 13 {
                return Err(Error::UnexpectedEof { needed: 13, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let ethernet_tag = u32::from_be_bytes([v[8], v[9], v[10], v[11]]);
            let mut pos = 12;
            let src_ip_len = v[pos]; pos += 1;
            let multicast_source = decode_evpn_router_ip(src_ip_len, &v[pos..])?;
            pos += if src_ip_len == 32 { 4 } else { 16 };
            if pos >= v.len() {
                return Err(Error::UnexpectedEof { needed: pos + 1, have: v.len() });
            }
            let grp_ip_len = v[pos]; pos += 1;
            let multicast_group = decode_evpn_router_ip(grp_ip_len, &v[pos..])?;
            pos += if grp_ip_len == 32 { 4 } else { 16 };
            if pos >= v.len() {
                return Err(Error::UnexpectedEof { needed: pos + 1, have: v.len() });
            }
            let orig_ip_len = v[pos]; pos += 1;
            let originating_router_ip = decode_evpn_router_ip(orig_ip_len, &v[pos..])?;
            match route_type {
                6 => Ok(EvpnRoute::SelectiveMulticastEthernetTag {
                    rd, ethernet_tag, multicast_source, multicast_group, originating_router_ip,
                }),
                7 => Ok(EvpnRoute::IgmpJoinSynch {
                    rd, ethernet_tag, multicast_source, multicast_group, originating_router_ip,
                }),
                _ => Ok(EvpnRoute::IgmpLeaveSynch {
                    rd, ethernet_tag, multicast_source, multicast_group, originating_router_ip,
                }),
            }
        }
        9 => {
            // Type 9: RD(8) + ETag(4) + orig_ip_len(1) + orig_ip(4/16)
            if v.len() < 13 {
                return Err(Error::UnexpectedEof { needed: 13, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let ethernet_tag = u32::from_be_bytes([v[8], v[9], v[10], v[11]]);
            let ip_len = v[12];
            let originating_router_ip = decode_evpn_router_ip(ip_len, &v[13..])?;
            Ok(EvpnRoute::PerRegionIPmsi { rd, ethernet_tag, originating_router_ip })
        }
        10 => {
            // Type 10: same structure as type 6/7/8
            if v.len() < 13 {
                return Err(Error::UnexpectedEof { needed: 13, have: v.len() });
            }
            let mut rd = [0u8; 8]; rd.copy_from_slice(&v[0..8]);
            let ethernet_tag = u32::from_be_bytes([v[8], v[9], v[10], v[11]]);
            let mut pos = 12;
            let src_ip_len = v[pos]; pos += 1;
            let multicast_source = decode_evpn_router_ip(src_ip_len, &v[pos..])?;
            pos += if src_ip_len == 32 { 4 } else { 16 };
            if pos >= v.len() {
                return Err(Error::UnexpectedEof { needed: pos + 1, have: v.len() });
            }
            let grp_ip_len = v[pos]; pos += 1;
            let multicast_group = decode_evpn_router_ip(grp_ip_len, &v[pos..])?;
            pos += if grp_ip_len == 32 { 4 } else { 16 };
            if pos >= v.len() {
                return Err(Error::UnexpectedEof { needed: pos + 1, have: v.len() });
            }
            let orig_ip_len = v[pos]; pos += 1;
            let originating_router_ip = decode_evpn_router_ip(orig_ip_len, &v[pos..])?;
            Ok(EvpnRoute::SPmsi { rd, ethernet_tag, multicast_source, multicast_group, originating_router_ip })
        }
        11 => {
            // Type 11: Leaf A-D route — route_key (variable) + path_id(4)
            if v.len() < 4 {
                return Err(Error::UnexpectedEof { needed: 4, have: v.len() });
            }
            let path_id = u32::from_be_bytes([
                v[v.len()-4], v[v.len()-3], v[v.len()-2], v[v.len()-1],
            ]);
            let route_key = v[..v.len()-4].to_vec();
            Ok(EvpnRoute::LeafAD { route_key, path_id })
        }
        _ => Ok(EvpnRoute::Unknown { route_type, data: v.to_vec() }),
    }
}

fn decode_mpls_label(b: &[u8]) -> u32 {
    // 24-bit field; top 20 bits = label value
    let raw = u32::from_be_bytes([0, b[0], b[1], b[2]]);
    raw >> 4
}

fn decode_evpn_router_ip(ip_len: u8, buf: &[u8]) -> Result<IpAddr> {
    match ip_len {
        32 => {
            if buf.len() < 4 {
                return Err(Error::UnexpectedEof { needed: 4, have: buf.len() });
            }
            Ok(IpAddr::V4(Ipv4Addr::from([buf[0], buf[1], buf[2], buf[3]])))
        }
        128 => {
            if buf.len() < 16 {
                return Err(Error::UnexpectedEof { needed: 16, have: buf.len() });
            }
            let mut b = [0u8; 16]; b.copy_from_slice(&buf[..16]);
            Ok(IpAddr::V6(Ipv6Addr::from(b)))
        }
        _ => Err(Error::BgpParse(format!("EVPN originator IP length {ip_len} not 32 or 128"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evpn_type6_selective_multicast() {
        // RD(8) + ETag(4) + src_ip_len(1)+src_ip(4) + grp_ip_len(1)+grp_ip(4) + orig_ip_len(1)+orig_ip(4)
        let mut v = vec![];
        v.extend_from_slice(&[0u8; 8]);       // RD
        v.extend_from_slice(&[0, 0, 0, 1]);   // ETag=1
        v.push(32);                            // src_ip_len
        v.extend_from_slice(&[239, 1, 1, 1]); // 239.1.1.1
        v.push(32);                            // grp_ip_len
        v.extend_from_slice(&[232, 0, 0, 1]); // 232.0.0.1
        v.push(32);                            // orig_ip_len
        v.extend_from_slice(&[10, 0, 0, 1]);  // 10.0.0.1
        let mut buf = vec![6u8, v.len() as u8];
        buf.extend_from_slice(&v);
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route_type_code(), 6);
        assert_eq!(routes[0].route_type_name(), "selective-multicast-ethernet-tag");
    }

    #[test]
    fn test_evpn_type7_igmp_join() {
        let mut v = vec![];
        v.extend_from_slice(&[0u8; 8]);
        v.extend_from_slice(&[0, 0, 0, 2]);
        v.push(32); v.extend_from_slice(&[1, 1, 1, 1]);
        v.push(32); v.extend_from_slice(&[225, 0, 0, 1]);
        v.push(32); v.extend_from_slice(&[10, 0, 0, 2]);
        let mut buf = vec![7u8, v.len() as u8];
        buf.extend_from_slice(&v);
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes[0].route_type_code(), 7);
        assert_eq!(routes[0].route_type_name(), "igmp-join-synch");
    }

    #[test]
    fn test_evpn_type9_per_region_ipmsi() {
        let mut v = vec![];
        v.extend_from_slice(&[0u8; 8]);
        v.extend_from_slice(&[0, 0, 0, 5]);
        v.push(32); v.extend_from_slice(&[10, 0, 0, 5]);
        let mut buf = vec![9u8, v.len() as u8];
        buf.extend_from_slice(&v);
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes[0].route_type_code(), 9);
        assert_eq!(routes[0].route_type_name(), "per-region-i-pmsi");
    }

    #[test]
    fn test_evpn_type11_leaf_ad() {
        // route_key(4) + path_id(4)
        let v = vec![0xAA, 0xBB, 0xCC, 0xDD, 0, 0, 0, 42];
        let mut buf = vec![11u8, v.len() as u8];
        buf.extend_from_slice(&v);
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes[0].route_type_code(), 11);
        match &routes[0] {
            EvpnRoute::LeafAD { path_id, .. } => assert_eq!(*path_id, 42),
            _ => panic!("expected LeafAD"),
        }
    }

    #[test]
    fn test_evpn_type1_auto_discovery() {
        // RD(8) + ESI(10) + ETag(4) + Label(3) = 25 bytes
        let mut buf = vec![1u8, 25]; // type=1, len=25
        buf.extend_from_slice(&[0u8; 8]);   // RD all zeros
        buf.extend_from_slice(&[0xAA; 10]); // ESI
        buf.extend_from_slice(&[0, 0, 0, 100]); // ETag = 100
        buf.extend_from_slice(&[0, 0xAB, 0xCD]); // label = (0x0ABCD << 4) >> 4 = 0xABCD/... actually raw>>4 = 0x0ABCD
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes.len(), 1);
        match &routes[0] {
            EvpnRoute::EthernetAutoDiscovery { ethernet_tag, esi, .. } => {
                assert_eq!(*ethernet_tag, 100);
                assert_eq!(esi, &[0xAA; 10]);
            }
            _ => panic!("expected EthernetAutoDiscovery"),
        }
    }

    #[test]
    fn test_evpn_type3_imet() {
        // RD(8) + ETag(4) + IPlen(1) + IPv4(4) = 17 bytes
        let mut buf = vec![3u8, 17]; // type=3, len=17
        buf.extend_from_slice(&[0u8; 8]);    // RD
        buf.extend_from_slice(&[0, 0, 0, 1]); // ETag = 1
        buf.push(32);                          // IPv4
        buf.extend_from_slice(&[10, 0, 0, 1]); // 10.0.0.1
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes.len(), 1);
        match &routes[0] {
            EvpnRoute::InclusiveMulticastEthernetTag { ethernet_tag, originating_router_ip, .. } => {
                assert_eq!(*ethernet_tag, 1);
                assert_eq!(originating_router_ip.to_string(), "10.0.0.1");
            }
            _ => panic!("expected InclusiveMulticastEthernetTag"),
        }
    }

    #[test]
    fn test_evpn_unknown_type() {
        let buf = vec![99u8, 4, 0xDE, 0xAD, 0xBE, 0xEF];
        let routes = decode_evpn_nlri(&buf).unwrap();
        assert_eq!(routes.len(), 1);
        assert!(matches!(&routes[0], EvpnRoute::Unknown { route_type: 99, .. }));
    }
}

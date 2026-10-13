//! B02 reproductions against the real parser; do not regenerate expectations.
//! RFC 7854 sections 4.1/4.4/4.5, RFC 8671 section 4, RFC 9069 section 4.2.
use rbmp_core::bmp::parser::{DEFAULT_MAX_FRAME, parse_bmp_message};
use rbmp_core::bmp::types::{BmpPayload, RibType};
use std::net::{IpAddr, Ipv4Addr};

const SPEAKER: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
// RFC 7854 section 4.3 requires sysDescr and sysName in Initiation.
const VALID_INITIATION: &[u8] =
    b"\x03\x00\x00\x00\x1a\x04\x00\x01\x00\x06router\x00\x02\x00\x06edge-a";

fn parse(bytes: &[u8]) -> rbmp_core::Result<BmpPayload> {
    parse_bmp_message(bytes, SPEAKER, DEFAULT_MAX_FRAME)
}

#[test]
fn initiation_tlv_assignments_follow_rfc7854() {
    // Independently written six-byte header, TLV 0 String, 1 sysDescr, 2 sysName.
    let bytes =
        b"\x03\x00\x00\x00\x22\x04\x00\x00\x00\x04note\x00\x01\x00\x06router\x00\x02\x00\x06edge-a";
    assert_eq!(bytes.len(), 34);
    let BmpPayload::Initiation {
        sys_name,
        sys_descr,
        labels,
    } = parse(bytes).unwrap()
    else {
        panic!("expected Initiation");
    };
    assert_eq!(sys_name.as_deref(), Some("edge-a"));
    assert_eq!(sys_descr.as_deref(), Some("router"));
    assert_eq!(labels, ["note"]);
}

#[test]
fn termination_tlv_assignments_follow_rfc7854() {
    // String TLV "note" followed by Reason TLV 3 (redundant connection).
    let bytes = b"\x03\x00\x00\x00\x14\x05\x00\x00\x00\x04note\x00\x01\x00\x02\x00\x03";
    assert_eq!(bytes.len(), 20);
    let BmpPayload::Termination {
        reason_code,
        reason_text,
    } = parse(bytes).unwrap()
    else {
        panic!("expected Termination");
    };
    assert_eq!(reason_code, 3);
    assert_eq!(reason_text.as_deref(), Some("note"));
}

fn monitoring(peer_type: u8, flags: u8) -> Vec<u8> {
    // 6-byte BMP + 42-byte per-peer + 23-byte empty BGP UPDATE (IPv4 EOR).
    let mut bytes = vec![3, 0, 0, 0, 71, 0];
    let mut peer = [0u8; 42];
    peer[0] = peer_type;
    peer[1] = flags;
    bytes.extend_from_slice(&peer);
    bytes.extend_from_slice(&[0xff; 16]);
    bytes.extend_from_slice(&[0, 23, 2, 0, 0, 0, 0]);
    assert_eq!(bytes.len(), 71);
    bytes
}

fn view(peer_type: u8, flags: u8) -> RibType {
    let BmpPayload::RouteMonitoring { peer_header, .. } =
        parse(&monitoring(peer_type, flags)).unwrap()
    else {
        panic!("expected Route Monitoring");
    };
    peer_header.rib_type
}

#[test]
fn local_instance_is_inbound_when_o_is_clear() {
    assert_eq!(view(2, 0), RibType::AdjRibInPrePolicy);
    assert_eq!(view(2, 0x40), RibType::AdjRibInPostPolicy);
}

#[test]
fn outbound_o_flag_applies_to_peer_types_zero_through_two() {
    for peer_type in 0..=2 {
        assert_eq!(view(peer_type, 0x10), RibType::AdjRibOutPrePolicy);
        assert_eq!(view(peer_type, 0x50), RibType::AdjRibOutPostPolicy);
    }
}

#[test]
fn locrib_filtered_flag_is_the_high_bit() {
    assert_eq!(view(3, 0x80), RibType::LocRibFiltered);
}

#[test]
fn locrib_reserved_bits_do_not_imply_filtering() {
    assert_eq!(view(3, 0x08), RibType::LocRib);
}

#[test]
fn zero_and_short_declared_lengths_are_rejected() {
    for declared in [0, 5] {
        let mut bytes = VALID_INITIATION.to_vec();
        bytes[4] = declared;
        assert!(parse(&bytes).is_err());
    }
}

#[test]
fn truncated_declared_bmp_message_is_rejected() {
    let mut bytes = VALID_INITIATION.to_vec();
    bytes[4] = 27; // Valid TLVs, but the declared message is one byte longer.
    assert!(parse(&bytes).is_err());
}

#[test]
fn incorrect_embedded_bgp_length_is_rejected() {
    let mut bytes = monitoring(0, 0);
    bytes[65] = 24; // BGP declares 24 bytes; only 23 are present.
    assert!(parse(&bytes).is_err());
}

#[test]
fn six_byte_header_with_required_initiation_tlvs_is_accepted() {
    assert!(matches!(
        parse(VALID_INITIATION),
        Ok(BmpPayload::Initiation { .. })
    ));
}

#[test]
fn original_invalid_generator_fixtures_remain_negative_inputs() {
    let fixtures: [&[u8]; 10] = [
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/01_initiation.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/02_peer_up.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/03_route_monitor_announce.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/04_route_monitor_withdraw.bin"),
        include_bytes!(
            "../fixtures/bmp/negative_legacy_7byte/05_route_monitor_path_status_best.bin"
        ),
        include_bytes!(
            "../fixtures/bmp/negative_legacy_7byte/06_route_monitor_path_status_nonsel.bin"
        ),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/07_peer_down.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/08_stats_report.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/09_route_monitor_ipv6.bin"),
        include_bytes!("../fixtures/bmp/negative_legacy_7byte/10_termination.bin"),
    ];
    for (index, bytes) in fixtures.iter().enumerate() {
        assert!(
            parse(bytes).is_err(),
            "invalid fixture {} accepted",
            index + 1
        );
    }
}

#[test]
fn repaired_positive_fixtures_parse_cleanly() {
    let fixtures: [&[u8]; 10] = [
        include_bytes!("../fixtures/bmp/01_initiation.bin"),
        include_bytes!("../fixtures/bmp/02_peer_up.bin"),
        include_bytes!("../fixtures/bmp/03_route_monitor_announce.bin"),
        include_bytes!("../fixtures/bmp/04_route_monitor_withdraw.bin"),
        include_bytes!("../fixtures/bmp/05_route_monitor_path_status_best.bin"),
        include_bytes!("../fixtures/bmp/06_route_monitor_path_status_nonsel.bin"),
        include_bytes!("../fixtures/bmp/07_peer_down.bin"),
        include_bytes!("../fixtures/bmp/08_stats_report.bin"),
        include_bytes!("../fixtures/bmp/09_route_monitor_ipv6.bin"),
        include_bytes!("../fixtures/bmp/10_termination.bin"),
    ];
    for (index, bytes) in fixtures.iter().enumerate() {
        let declared = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        assert_eq!(
            declared,
            bytes.len(),
            "fixture {} length mismatch",
            index + 1
        );
        assert!(
            parse(bytes).is_ok(),
            "repaired fixture {} rejected: {:?}",
            index + 1,
            parse(bytes)
        );
    }
}

#[test]
fn unknown_initiation_tlv_and_unknown_termination_reason_are_preserved() {
    use rbmp_core::bmp::parser::parse_initiation_tlvs;
    use rbmp_core::bmp::types::InitiationInfo;

    // Initiation TLV type 99 with raw payload 0xdeadbeef alongside required TLVs 1 and 2
    let body = b"\x00\x01\x00\x06router\x00\x02\x00\x06edge-a\x00\x63\x00\x04\xde\xad\xbe\xef";
    let tlvs = parse_initiation_tlvs(body).unwrap();
    assert_eq!(tlvs.len(), 3);
    assert!(matches!(
        &tlvs[2],
        InitiationInfo::Unknown { tlv_type: 99, hex } if hex == "deadbeef"
    ));

    // Termination with unknown reason code 99 (0x0063)
    let term_bytes = b"\x03\x00\x00\x00\x0c\x05\x00\x01\x00\x02\x00\x63";
    let BmpPayload::Termination { reason_code, .. } = parse(term_bytes).unwrap() else {
        panic!("expected Termination");
    };
    assert_eq!(reason_code, 99);
}

#[test]
fn huge_stats_report_count_is_rejected_without_unbounded_allocation() {
    // 6-byte BMP header (len = 52, type = 1 StatsReport) + 42-byte peer header + stat_count = 0xFFFF_FFFF
    let mut bytes = vec![3, 0, 0, 0, 52, 1];
    bytes.extend_from_slice(&[0u8; 42]);
    bytes.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    assert_eq!(bytes.len(), 52);
    assert!(parse(&bytes).is_err());
}

#[test]
fn public_bgp_parsers_reject_short_truncated_and_mismatched_inputs() {
    use rbmp_core::bgp::attributes::parse_path_attributes;
    use rbmp_core::bgp::capabilities::parse_capabilities;
    use rbmp_core::bgp::nlri::{decode_labeled_nlri, decode_next_hops, decode_vpn_nlri};
    use rbmp_core::bgp::open::parse_bgp_open;
    use rbmp_core::bgp::types::Afi;
    use rbmp_core::bgp::update::parse_bgp_update;

    // Short headers (< 19 bytes)
    assert!(parse_bgp_open(&b"\xff\xff\xff"[..]).is_err());
    assert!(parse_bgp_update(&b"\xff\xff\xff"[..], false).is_err());

    // OPEN with declared length 30 but only 29 bytes present
    let mut open = Vec::new();
    open.extend_from_slice(&[0xff; 16]);
    open.extend_from_slice(&30u16.to_be_bytes());
    open.push(1); // OPEN
    open.extend_from_slice(&[4, 0, 100, 0, 90, 10, 0, 0, 1, 0]);
    assert_eq!(open.len(), 29);
    assert!(parse_bgp_open(&open[..]).is_err());

    // OPEN with opt_params_len mismatch
    open[16..18].copy_from_slice(&29u16.to_be_bytes());
    open[28] = 4; // claims 4 bytes of optional params inside 29-byte OPEN
    assert!(parse_bgp_open(&open[..]).is_err());

    // Trailing 1-byte truncated attribute header
    assert!(parse_path_attributes(&[0x40, 0x01][..]).is_err());
    // ORIGIN attribute with invalid length 2
    assert!(parse_path_attributes(&[0x40, 0x01, 0x02, 0x00, 0x00][..]).is_err());
    // NEXT_HOP attribute with invalid length 2
    assert!(parse_path_attributes(&[0x40, 0x03, 0x02, 10, 0][..]).is_err());

    // Trailing 1-byte optional parameter header and truncated 4-byte ASN capability
    assert!(parse_capabilities(&[0x02][..]).is_err());
    assert!(parse_capabilities(&[0x02, 0x04, 65, 0x02, 0x00, 0x01][..]).is_err());

    // Underflowed labeled/VPN NLRI bit lengths and non-multiple next-hop length
    let mut bad_labeled = &b"\x08\x00\x01\x01"[..]; // total_len=8 bits < 24-bit label
    assert!(decode_labeled_nlri(&mut bad_labeled, Afi::Ipv4).is_err());
    let mut bad_vpn = &b"\x10\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x01"[..]; // total_len=16 < 88 bits
    assert!(decode_vpn_nlri(&mut bad_vpn, Afi::Ipv4).is_err());
    let mut bad_nh = &b"\x03\x0a\x00\x00"[..]; // 3-byte IPv4 next-hop
    assert!(decode_next_hops(&mut bad_nh, Afi::Ipv4).is_err());
}

#[tokio::test]
async fn read_bmp_frame_enforces_bounds_deadlines_fragmentation_and_coalescing() {
    use rbmp_core::bmp::parser::read_bmp_frame;
    use rbmp_core::collector_protocol::read_frame_with_timeout;
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, BufReader};

    let t = Duration::from_millis(40);

    // 1. Zero, short, and oversized headers are rejected immediately at the 6-byte boundary
    for hdr in [
        [3u8, 0, 0, 0, 0, 4],
        [3u8, 0, 0, 0, 5, 4],
        [3u8, 0, 0, 4, 1, 4], // 1025 > max_frame=1024
        [2u8, 0, 0, 0, 6, 4], // invalid version 2
    ] {
        let (mut w, mut r) = tokio::io::duplex(64);
        w.write_all(&hdr).await.unwrap();
        assert!(read_bmp_frame(&mut r, 1024, t, t).await.is_err());
    }

    // 2. Stalled partial header and stalled partial body time out
    let (mut w_hdr, mut r_hdr) = tokio::io::duplex(64);
    w_hdr.write_all(&[3, 0, 0]).await.unwrap();
    let err = read_bmp_frame(&mut r_hdr, 1024, t, t).await.unwrap_err();
    assert!(matches!(err, rbmp_core::Error::Io(ref e) if e.kind() == std::io::ErrorKind::TimedOut));

    let (mut w_body, mut r_body) = tokio::io::duplex(64);
    w_body.write_all(&[3, 0, 0, 0, 26, 4, 0, 1]).await.unwrap();
    let err = read_bmp_frame(&mut r_body, 1024, t, t).await.unwrap_err();
    assert!(matches!(err, rbmp_core::Error::Io(ref e) if e.kind() == std::io::ErrorKind::TimedOut));

    // 3. Fragmented and coalesced frames decode cleanly in order
    let (mut w_ok, r_ok) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        // Send first VALID_INITIATION 2 bytes at a time
        for chunk in VALID_INITIATION.chunks(2) {
            w_ok.write_all(chunk).await.unwrap();
        }
        // Send two more frames coalesced in a single write
        let mut two = Vec::new();
        two.extend_from_slice(VALID_INITIATION);
        two.extend_from_slice(VALID_INITIATION);
        w_ok.write_all(&two).await.unwrap();
        drop(w_ok);
    });
    let mut buf_r = BufReader::new(r_ok);
    for _ in 0..3 {
        let frame = read_bmp_frame(
            &mut buf_r,
            1024,
            Duration::from_millis(200),
            Duration::from_millis(200),
        )
        .await
        .unwrap()
        .expect("expected frame");
        assert_eq!(frame, VALID_INITIATION);
        assert!(parse(&frame).is_ok());
    }
    assert!(
        read_bmp_frame(&mut buf_r, 1024, t, t)
            .await
            .unwrap()
            .is_none()
    );

    // 4. Collector protocol rejects zero length, oversized length, and truncated header
    let (mut cw1, mut cr1) = tokio::io::duplex(64);
    cw1.write_all(&0u32.to_be_bytes()).await.unwrap();
    assert!(read_frame_with_timeout(&mut cr1, t).await.is_err());

    let (mut cw2, mut cr2) = tokio::io::duplex(64);
    cw2.write_all(&[0x00, 0x01]).await.unwrap();
    drop(cw2);
    assert!(read_frame_with_timeout(&mut cr2, t).await.is_err());
}

fn build_bmp_route_monitoring(peer_flags: u8, path_attrs: &[u8], nlri: &[u8]) -> Vec<u8> {
    let mut bgp = Vec::new();
    bgp.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + 2 + path_attrs.len() + nlri.len()) as u16;
    bgp.extend_from_slice(&bgp_len.to_be_bytes());
    bgp.push(2); // UPDATE
    bgp.extend_from_slice(&0u16.to_be_bytes()); // withdrawn_len = 0
    bgp.extend_from_slice(&(path_attrs.len() as u16).to_be_bytes());
    bgp.extend_from_slice(path_attrs);
    bgp.extend_from_slice(nlri);

    let total_len = (6 + 42 + bgp.len()) as u32;
    let mut bmp = Vec::with_capacity(total_len as usize);
    bmp.push(3);
    bmp.extend_from_slice(&total_len.to_be_bytes());
    bmp.push(0); // RouteMonitoring

    // 42-byte Per-Peer Header
    bmp.push(0); // peer_type = Global
    bmp.push(peer_flags);
    bmp.extend_from_slice(&[0u8; 8]); // peer_distinguisher
    bmp.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 1]);
    bmp.extend_from_slice(&65001u32.to_be_bytes());
    bmp.extend_from_slice(&[192, 0, 2, 1]);
    bmp.extend_from_slice(&1700000000u32.to_be_bytes());
    bmp.extend_from_slice(&0u32.to_be_bytes());
    bmp.extend_from_slice(&bgp);
    bmp
}

#[test]
fn b05_four_byte_asn_66048_is_not_misparsed_as_two_byte_segments() {
    // 4-byte ASN 66048 = 0x0001_0200. In 2-byte parser this looks like seg_type=2, seg_len=1, asn=1, seg_type=2, seg_len=0!
    // PeerFlags has A-bit (0x20) CLEAR -> 4-byte ASN encoding per RFC 7854 §4.2.
    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1, 0x00, 0x01, 0x02, 0x00]); // AS_PATH = [66048]
    attrs.extend_from_slice(&[0x40, 3, 4, 192, 0, 2, 1]); // NEXT_HOP = 192.0.2.1
    let nlri = [24, 10, 0, 1]; // 10.0.1.0/24

    let frame = build_bmp_route_monitoring(0x00, &attrs, &nlri);
    let msg = parse(&frame).expect("valid RouteMonitoring");
    let BmpPayload::RouteMonitoring { update, .. } = msg else {
        panic!("expected RouteMonitoring");
    };
    assert_eq!(
        update.attributes.as_path.unwrap().0,
        vec![rbmp_core::bgp::types::AsPathSegment::Sequence(vec![66048])]
    );
}

#[test]
fn b05_two_byte_as_trans_reconstructs_with_as4_path_and_as4_aggregator() {
    // PeerFlags has A-bit (0x20) SET -> 2-byte ASN encoding on wire, with AS_TRANS (23456) + AS4_PATH + AS4_AGGREGATOR.
    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    // 2-byte AS_PATH: Sequence([65001, 23456]) -> len = 2 + 4 = 6
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 2]);
    attrs.extend_from_slice(&65001u16.to_be_bytes());
    attrs.extend_from_slice(&23456u16.to_be_bytes());
    attrs.extend_from_slice(&[0x40, 3, 4, 192, 0, 2, 1]); // NEXT_HOP
    // 2-byte AGGREGATOR (type 7, len 6): ASN 23456 + 192.0.2.9
    attrs.extend_from_slice(&[0xc0, 7, 6]);
    attrs.extend_from_slice(&23456u16.to_be_bytes());
    attrs.extend_from_slice(&[192, 0, 2, 9]);
    // AS4_PATH (type 17, len 6): Sequence([4200000001])
    attrs.extend_from_slice(&[0xc0, 17, 6, 2, 1]);
    attrs.extend_from_slice(&4200000001u32.to_be_bytes());
    // AS4_AGGREGATOR (type 18, len 8): ASN 4200000099 + 192.0.2.99
    attrs.extend_from_slice(&[0xc0, 18, 8]);
    attrs.extend_from_slice(&4200000099u32.to_be_bytes());
    attrs.extend_from_slice(&[192, 0, 2, 99]);

    let nlri = [24, 10, 0, 2];
    let frame = build_bmp_route_monitoring(0x20, &attrs, &nlri);
    let msg = parse(&frame).expect("valid RouteMonitoring");
    let BmpPayload::RouteMonitoring { update, .. } = msg else {
        panic!("expected RouteMonitoring");
    };
    assert_eq!(
        update.attributes.as_path.unwrap().0,
        vec![rbmp_core::bgp::types::AsPathSegment::Sequence(vec![
            65001, 4200000001
        ])]
    );
    let agg = update.attributes.aggregator.expect("aggregator present");
    assert_eq!(agg.0, 4200000099);
    assert_eq!(agg.1, std::net::Ipv4Addr::new(192, 0, 2, 99));
}

#[test]
fn b05_mp_reach_ipv6_populates_next_hop_and_evpn_unreach_is_not_eor() {
    // 1. IPv6 MP_REACH_NLRI must populate attributes.next_hop from mp_reach.next_hops
    let mut mp_reach_val = Vec::new();
    mp_reach_val.extend_from_slice(&2u16.to_be_bytes()); // AFI = IPv6
    mp_reach_val.push(1); // SAFI = Unicast
    mp_reach_val.push(16); // next-hop len = 16
    mp_reach_val.extend_from_slice(
        &"2001:db8::1"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets(),
    );
    mp_reach_val.push(0); // reserved
    mp_reach_val.extend_from_slice(&[48, 0x20, 0x01, 0x0d, 0xb8, 0x00, 0x01]); // 2001:db8:1::/48

    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    attrs.extend_from_slice(&[0x80, 14, mp_reach_val.len() as u8]);
    attrs.extend_from_slice(&mp_reach_val);

    let frame = build_bmp_route_monitoring(0x00, &attrs, &[]);
    let BmpPayload::RouteMonitoring { update, .. } = parse(&frame).unwrap() else {
        panic!("expected RouteMonitoring");
    };
    assert_eq!(
        update.attributes.next_hop,
        Some("2001:db8::1".parse().unwrap())
    );

    // 2. EVPN MP_UNREACH_NLRI withdrawal must NOT be classified as End-of-RIB
    let mut evpn_unreach = Vec::new();
    evpn_unreach.extend_from_slice(&25u16.to_be_bytes()); // AFI = L2VPN (25)
    evpn_unreach.push(70); // SAFI = EVPN (70)
    // EVPN Type 3 (Inclusive Multicast Ethernet Tag Route): route_type=3, length=17, RD(8) + EthTag(4) + IPLen(1=32) + IP(4)
    evpn_unreach.push(3);
    evpn_unreach.push(17);
    evpn_unreach.extend_from_slice(&[0, 0, 253, 232, 0, 0, 0, 1]); // RD 65000:1
    evpn_unreach.extend_from_slice(&100u32.to_be_bytes()); // eth_tag_id = 100
    evpn_unreach.push(32); // ip_len = 32 bits
    evpn_unreach.extend_from_slice(&[192, 0, 2, 1]);

    let mut unreach_attrs = Vec::new();
    unreach_attrs.extend_from_slice(&[0x80, 15, evpn_unreach.len() as u8]);
    unreach_attrs.extend_from_slice(&evpn_unreach);

    let unreach_frame = build_bmp_route_monitoring(0x00, &unreach_attrs, &[]);
    let BmpPayload::RouteMonitoring {
        update: unreach_update,
        ..
    } = parse(&unreach_frame).unwrap()
    else {
        panic!("expected RouteMonitoring");
    };
    assert!(!unreach_update.is_eor());
    assert_eq!(unreach_update.eor_afi_safi(), None);
}

#[test]
fn b05_session_aware_add_path_eor_and_family_support_status() {
    use rbmp_core::bgp::types::{AfiSafi, BgpCapability, BgpDecodeContext, FamilySupportStatus};
    use rbmp_core::bmp::parser::parse_bmp_message_with_context;

    // 1. Capabilities negotiate Add-Path for IPv4 Unicast (1/1) and IPv6 Unicast (2/1)
    let caps = vec![
        BgpCapability::FourByteAsn(65001),
        BgpCapability::AddPath(vec![
            (AfiSafi::ipv4_unicast(), 3),
            (AfiSafi::ipv6_unicast(), 3),
        ]),
    ];
    let ctx = BgpDecodeContext::from_capabilities(true, &caps);
    assert!(ctx.has_add_path(AfiSafi::ipv4_unicast()));
    assert!(ctx.has_add_path(AfiSafi::ipv6_unicast()));

    // 2. RouteMonitoring carrying both IPv4 Add-Path NLRI (path_id=7) and IPv6 MP_REACH Add-Path NLRI (path_id=42)
    let mut mp_reach_val = Vec::new();
    mp_reach_val.extend_from_slice(&2u16.to_be_bytes()); // AFI = IPv6
    mp_reach_val.push(1); // SAFI = Unicast
    mp_reach_val.push(16); // next-hop len = 16
    mp_reach_val.extend_from_slice(
        &"2001:db8::99"
            .parse::<std::net::Ipv6Addr>()
            .unwrap()
            .octets(),
    );
    mp_reach_val.push(0); // reserved
    mp_reach_val.extend_from_slice(&42u32.to_be_bytes()); // Add-Path path_id = 42
    mp_reach_val.extend_from_slice(&[48, 0x20, 0x01, 0x0d, 0xb8, 0x00, 0x99]); // 2001:db8:99::/48

    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1, 0x00, 0x00, 0xfd, 0xe9]); // AS_PATH = [65001]
    attrs.extend_from_slice(&[0x40, 3, 4, 192, 0, 2, 1]); // NEXT_HOP
    attrs.extend_from_slice(&[0x80, 14, mp_reach_val.len() as u8]);
    attrs.extend_from_slice(&mp_reach_val);

    let mut v4_nlri = Vec::new();
    v4_nlri.extend_from_slice(&7u32.to_be_bytes()); // Add-Path path_id = 7
    v4_nlri.extend_from_slice(&[24, 10, 99, 0]); // 10.99.0.0/24

    let frame = build_bmp_route_monitoring(0x00, &attrs, &v4_nlri);
    let BmpPayload::RouteMonitoring { update, .. } =
        parse_bmp_message_with_context(&frame, SPEAKER, DEFAULT_MAX_FRAME, &ctx).unwrap()
    else {
        panic!("expected RouteMonitoring");
    };
    let announced: Vec<(String, Option<u32>)> = update
        .all_announced_with_path_id()
        .into_iter()
        .map(|(p, id)| (p.to_string(), id))
        .collect();
    assert_eq!(
        announced,
        vec![
            ("10.99.0.0/24".to_string(), Some(7)),
            ("2001:db8:99::/48".to_string(), Some(42)),
        ]
    );

    // 3. True IPv4 EOR and IPv6 MP_UNREACH EOR report exact AFI/SAFI
    let v4_eor_frame = build_bmp_route_monitoring(0x00, &[], &[]);
    let BmpPayload::RouteMonitoring { update: v4_eor, .. } = parse(&v4_eor_frame).unwrap() else {
        panic!("expected RouteMonitoring");
    };
    assert!(v4_eor.is_eor());
    assert_eq!(v4_eor.eor_afi_safi(), Some(AfiSafi::ipv4_unicast()));

    let v6_eor_attrs = [0x80, 15, 3, 0x00, 0x02, 0x01]; // MP_UNREACH AFI=2 SAFI=1 empty NLRI
    let v6_eor_frame = build_bmp_route_monitoring(0x00, &v6_eor_attrs, &[]);
    let BmpPayload::RouteMonitoring { update: v6_eor, .. } = parse(&v6_eor_frame).unwrap() else {
        panic!("expected RouteMonitoring");
    };
    assert!(v6_eor.is_eor());
    assert_eq!(v6_eor.eor_afi_safi(), Some(AfiSafi::ipv6_unicast()));

    // 4. FamilySupportStatus explicitly classifies Supported vs ExperimentalParseOnly vs Unsupported
    assert_eq!(
        AfiSafi::ipv4_unicast().support_status(),
        FamilySupportStatus::Supported
    );
    assert_eq!(
        AfiSafi::ipv6_unicast().support_status(),
        FamilySupportStatus::Supported
    );
    assert_eq!(
        AfiSafi::ipv4_vpn().support_status(),
        FamilySupportStatus::Supported
    );
    assert_eq!(
        AfiSafi::evpn().support_status(),
        FamilySupportStatus::ExperimentalParseOnly
    );
    assert_eq!(
        AfiSafi::new(16388, 71).support_status(),
        FamilySupportStatus::ExperimentalParseOnly
    );
    assert_eq!(
        AfiSafi::new(9999, 250).support_status(),
        FamilySupportStatus::Unsupported
    );
}

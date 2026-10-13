/// BMP PDU parsing integration tests (RV4-9 T1).
///
/// Tests the full BMP parse path using real captured PDU bytes.
/// Does NOT require a running server — exercises the parse stack directly.
#[cfg(test)]
mod tests {
    use rbmp_core::bmp::parser::{DEFAULT_MAX_FRAME, parse_bmp_message};
    use rbmp_core::bmp::types::BmpPayload;
    use std::net::IpAddr;

    const SPEAKER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));

    /// Minimal BMP Initiation Message (RFC 7854 §4.3)
    /// Header: version=3, length=6, type=4 (Initiation)
    const INITIATION_PDU: &[u8] = &[
        0x03, // version = 3
        0x00, 0x00, 0x00, 0x06, // length = 6
        0x04, // type = 4 (Initiation)
    ];

    /// Minimal BMP Route Monitoring message with synthetic BGP UPDATE
    /// Contains: BMP common header (6) + peer header (42) + BGP UPDATE (27) = 75 bytes
    /// with one withdrawn prefix (192.0.2.0/24) and no path attributes.
    const ROUTE_MONITOR_PDU: &[u8] = &[
        // BMP Common Header (6 bytes)
        0x03, // version
        0x00, 0x00, 0x00, 0x4B, // length = 75 (6 hdr + 42 peer hdr + 27 BGP UPDATE)
        0x00, // type = 0 (Route Monitoring)
        // Peer Header (42 bytes)
        0x00, // peer type = Global
        0x00, // peer flags
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // peer distinguisher (8 bytes)
        // peer address (16 bytes): IPv4-mapped — last 4 bytes = 192.0.2.1
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x02,
        0x01, 0x00, 0x00, 0xFD, 0xE8, // peer AS = 65000
        0x0A, 0x00, 0x00, 0x01, // peer BGP ID = 10.0.0.1
        0x67, 0xAC, 0x00, 0x00, // timestamp seconds (~2025)
        0x00, 0x00, 0x00, 0x00, // timestamp microseconds = 0
        // BGP UPDATE (27 bytes: 19 hdr + 2 withdrawn_len + 4 withdrawn + 2 path_attr_len)
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, // marker
        0x00, 0x1B, // length = 27
        0x02, // type = 2 (UPDATE)
        0x00, 0x04, // withdrawn length = 4
        0x18, 0xC0, 0x00, 0x02, // withdrawn: 192.0.2.0/24
        0x00, 0x00, // total path attr length = 0
    ];

    /// Retained negative fixture: original 75-byte PDU that erroneously declared
    /// BMP length 76 (0x4C) and BGP length 28 (0x1C).
    const LEGACY_MALFORMED_ROUTE_MONITOR_PDU: &[u8] = &[
        0x03, 0x00, 0x00, 0x00, 0x4C, // declared BMP length = 76 (actual = 75)
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x02, 0x01, 0x00, 0x00, 0xFD,
        0xE8, 0x0A, 0x00, 0x00, 0x01, 0x67, 0xAC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
        0x1C, // declared BGP length = 28 (actual = 27)
        0x02, 0x00, 0x04, 0x18, 0xC0, 0x00, 0x02, 0x00, 0x00,
    ];

    #[test]
    fn parse_initiation_message() {
        let result = parse_bmp_message(INITIATION_PDU, SPEAKER, DEFAULT_MAX_FRAME);
        assert!(result.is_ok(), "Initiation PDU must parse: {:?}", result);
        assert!(matches!(result.unwrap(), BmpPayload::Initiation { .. }));
    }

    #[test]
    fn parse_route_monitor_with_withdraw() {
        assert_eq!(ROUTE_MONITOR_PDU.len(), 75);
        assert_eq!(
            u32::from_be_bytes([
                ROUTE_MONITOR_PDU[1],
                ROUTE_MONITOR_PDU[2],
                ROUTE_MONITOR_PDU[3],
                ROUTE_MONITOR_PDU[4]
            ]),
            75
        );
        assert_eq!(ROUTE_MONITOR_PDU.len() - 48, 27);
        assert_eq!(
            u16::from_be_bytes([ROUTE_MONITOR_PDU[64], ROUTE_MONITOR_PDU[65]]),
            27
        );

        let result = parse_bmp_message(ROUTE_MONITOR_PDU, SPEAKER, DEFAULT_MAX_FRAME);
        assert!(result.is_ok(), "RouteMonitor PDU must parse: {:?}", result);
        if let Ok(BmpPayload::RouteMonitoring { update, .. }) = result {
            assert!(
                !update.withdrawn.is_empty(),
                "should have a withdrawn prefix"
            );
        } else {
            panic!("expected RouteMonitoring payload");
        }
    }

    #[test]
    fn legacy_malformed_route_monitor_pdu_is_rejected() {
        assert_eq!(LEGACY_MALFORMED_ROUTE_MONITOR_PDU.len(), 75);
        let declared_bmp = u32::from_be_bytes([
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[1],
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[2],
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[3],
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[4],
        ]);
        let actual_bgp = LEGACY_MALFORMED_ROUTE_MONITOR_PDU.len() - 48;
        let declared_bgp = u16::from_be_bytes([
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[64],
            LEGACY_MALFORMED_ROUTE_MONITOR_PDU[65],
        ]);
        assert_eq!(declared_bmp, 76);
        assert_eq!(actual_bgp, 27);
        assert_eq!(declared_bgp, 28);

        assert!(
            parse_bmp_message(
                LEGACY_MALFORMED_ROUTE_MONITOR_PDU,
                SPEAKER,
                DEFAULT_MAX_FRAME
            )
            .is_err(),
            "75-byte PDU declaring 76 BMP bytes must be rejected"
        );

        // Even if BMP length is patched to 75, declared BGP length 28 != actual 27 must fail
        let mut bgp_only_mismatch = LEGACY_MALFORMED_ROUTE_MONITOR_PDU.to_vec();
        bgp_only_mismatch[4] = 75;
        assert!(
            parse_bmp_message(&bgp_only_mismatch, SPEAKER, DEFAULT_MAX_FRAME).is_err(),
            "27-byte BGP UPDATE declaring 28 bytes must be rejected"
        );
    }

    #[test]
    fn truncated_pdu_returns_error() {
        let truncated = &INITIATION_PDU[..3];
        let result = parse_bmp_message(truncated, SPEAKER, DEFAULT_MAX_FRAME);
        assert!(result.is_err(), "truncated PDU must return error");
    }

    #[test]
    fn oversized_pdu_returns_error() {
        let result = parse_bmp_message(INITIATION_PDU, SPEAKER, 4);
        assert!(result.is_err(), "PDU exceeding max_frame must error");
    }
}

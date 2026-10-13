//! B02 RFC contract regressions against Bonsai's BMP parser (`src/streaming/bmp.rs`).
//! RFC 7854 sections 4.2/4.4/4.5, RFC 8671 section 4, RFC 9069 section 4.2.

use bonsai::streaming::bmp::{
    classify_rib_type, parse_bmp_message, parse_initiation, parse_termination, read_bmp_frame,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[test]
fn initiation_uses_rfc7854_tlv_numbers() {
    let payload = b"\x00\x00\x00\x04note\x00\x01\x00\x06router\x00\x02\x00\x06edge-a";
    let event = parse_initiation(payload, "192.0.2.1".into(), 0).unwrap();
    assert_eq!(event.sys_name.as_deref(), Some("edge-a"));
    assert_eq!(event.sys_descr.as_deref(), Some("router"));
    assert_eq!(event.init_admin_string.as_deref(), Some("note"));
}

#[test]
fn termination_uses_rfc7854_tlv_numbers() {
    let payload = b"\x00\x00\x00\x04note\x00\x01\x00\x02\x00\x03";
    let event = parse_termination(payload, "192.0.2.1".into(), 0).unwrap();
    assert_eq!(event.termination_message.as_deref(), Some("note"));
    assert_eq!(event.termination_reason, Some(3));
    assert_eq!(
        event.termination_reason_name.as_deref(),
        Some("redundant_connection")
    );
}

#[test]
fn local_instance_is_inbound_without_o_flag() {
    assert_eq!(classify_rib_type(2, 0), "adj-rib-in-pre-policy");
    assert_eq!(classify_rib_type(2, 0x40), "adj-rib-in-post-policy");
}

#[test]
fn outbound_view_follows_o_flag_for_all_adj_rib_peer_types() {
    for peer_type in 0..=2 {
        assert_eq!(classify_rib_type(peer_type, 0x10), "adj-rib-out-pre-policy");
        assert_eq!(
            classify_rib_type(peer_type, 0x50),
            "adj-rib-out-post-policy"
        );
    }
}

#[test]
fn locrib_filtered_flag_is_high_bit_without_ipv6_misclassification() {
    assert_eq!(classify_rib_type(3, 0x00), "loc-rib");
    assert_eq!(classify_rib_type(3, 0x08), "loc-rib");
    assert_eq!(classify_rib_type(3, 0x80), "loc-rib-filtered");
}

#[test]
fn route_monitoring_rejects_declared_bgp_length_mismatch() {
    // 42-byte per-peer header + 23-byte empty BGP UPDATE declaring 24 bytes
    let mut payload = vec![0u8; 42];
    payload.extend_from_slice(&[0xff; 16]);
    payload.extend_from_slice(&[0, 24, 2, 0, 0, 0, 0]);
    assert!(parse_bmp_message(0, &payload, "192.0.2.1".into()).is_err());

    // Declared BGP length < 19 must error, not panic
    let mut short_declared = vec![0u8; 42];
    short_declared.extend_from_slice(&[0xff; 16]);
    short_declared.extend_from_slice(&[0, 10, 2, 0, 0, 0, 0]);
    assert!(parse_bmp_message(0, &short_declared, "192.0.2.1".into()).is_err());
}

#[tokio::test]
async fn b03_bmp_framing_bounds_and_timeouts_are_enforced() {
    // 1. Fragmented + coalesced frames decode cleanly
    let frame_a = b"\x03\x00\x00\x00\x0e\x04\x00\x02\x00\x04edge";
    let frame_b = b"\x03\x00\x00\x00\x0c\x05\x00\x01\x00\x02\x00\x03";
    let (mut client, mut server) = tokio::io::duplex(256);
    tokio::spawn(async move {
        client.write_all(&frame_a[..2]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        client.write_all(&frame_a[2..]).await.unwrap();
        client.write_all(frame_b).await.unwrap();
    });

    let (t1, p1) = read_bmp_frame(
        &mut server,
        1024,
        Duration::from_millis(200),
        Duration::from_millis(200),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(t1, 4);
    assert_eq!(
        parse_bmp_message(t1, &p1, "192.0.2.1".into())
            .unwrap()
            .sys_name
            .as_deref(),
        Some("edge")
    );

    let (t2, p2) = read_bmp_frame(
        &mut server,
        1024,
        Duration::from_millis(200),
        Duration::from_millis(200),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(t2, 5);
    assert_eq!(
        parse_bmp_message(t2, &p2, "192.0.2.1".into())
            .unwrap()
            .termination_reason,
        Some(3)
    );
    assert!(
        read_bmp_frame(
            &mut server,
            1024,
            Duration::from_millis(200),
            Duration::from_millis(200)
        )
        .await
        .unwrap()
        .is_none()
    );

    // 2. Zero/short/oversized declared lengths rejected before payload read/allocation
    for bad_header in [
        [3u8, 0, 0, 0, 0, 4],
        [3u8, 0, 0, 0, 5, 4],
        [3u8, 0xff, 0xff, 0xff, 0xff, 4],
    ] {
        let (mut c, mut s) = tokio::io::duplex(64);
        c.write_all(&bad_header).await.unwrap();
        assert!(
            read_bmp_frame(
                &mut s,
                1024,
                Duration::from_millis(100),
                Duration::from_millis(100)
            )
            .await
            .is_err()
        );
    }

    // 3. Stalled header and stalled body time out cleanly
    let (_idle_c, mut idle_s) = tokio::io::duplex(64);
    assert!(
        read_bmp_frame(
            &mut idle_s,
            1024,
            Duration::from_millis(30),
            Duration::from_millis(30)
        )
        .await
        .is_err()
    );

    let (mut partial_c, mut partial_s) = tokio::io::duplex(64);
    partial_c
        .write_all(&[3, 0, 0, 0, 20, 4, 0, 2])
        .await
        .unwrap();
    assert!(
        read_bmp_frame(
            &mut partial_s,
            1024,
            Duration::from_millis(100),
            Duration::from_millis(30)
        )
        .await
        .is_err()
    );
}

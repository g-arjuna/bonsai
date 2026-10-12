/// cargo bench for BMP PDU parsing throughput (RV4-9 T4).
///
/// Run with:
///   cargo bench --bench bmp_parse
///
/// Baseline targets:
///   - Initiation PDU:      > 5 M msgs/sec
///   - Route Monitor PDU:   > 1 M msgs/sec
use std::hint::black_box;

use std::net::{IpAddr, Ipv4Addr};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rbmp_core::bmp::parser::{parse_bmp_message, DEFAULT_MAX_FRAME};

const SPEAKER: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

// ── PDU fixtures ─────────────────────────────────────────────────────────────

const INITIATION_PDU: &[u8] = &[
    0x03, 0x00, 0x00, 0x00, 0x06, 0x04,
];

/// Route Monitor with a single withdrawn prefix (192.0.2.0/24)
const ROUTE_MONITOR_WITHDRAW: &[u8] = &[
    // BMP common header
    0x03, 0x00, 0x00, 0x00, 0x36, 0x00,
    // Peer header (42 bytes)
    0x00, 0x00,
    0x00,0x00,0x00,0x00, 0x00,0x00,0x00,0x00,
    0x00,0x00,0x00,0x00, 0xC0,0x00,0x02,0x01,
    0x00,0x00,0xFD,0xE8,
    0x0A,0x00,0x00,0x01,
    0x67,0xAC,0x00,0x00, 0x00,0x00,0x00,0x00,
    // BGP UPDATE
    0xFF,0xFF,0xFF,0xFF, 0xFF,0xFF,0xFF,0xFF,
    0xFF,0xFF,0xFF,0xFF, 0xFF,0xFF,0xFF,0xFF,
    0x00, 0x1C, 0x02,
    0x00, 0x04, 0x18, 0xC0, 0x00, 0x02,
    0x00, 0x00,
];

/// Route Monitor with an IPv4 unicast announcement (203.0.113.0/24)
/// and minimal path attributes (ORIGIN + AS_PATH + NEXT_HOP)
const ROUTE_MONITOR_ANNOUNCE: &[u8] = &[
    // BMP common header
    0x03, 0x00, 0x00, 0x00, 0x51, 0x00,
    // Peer header (42 bytes)
    0x00, 0x00,
    0x00,0x00,0x00,0x00, 0x00,0x00,0x00,0x00,
    0x00,0x00,0x00,0x00, 0xC0,0x00,0x02,0x01,
    0x00,0x00,0xFD,0xE8,
    0x0A,0x00,0x00,0x01,
    0x67,0xAC,0x00,0x00, 0x00,0x00,0x00,0x00,
    // BGP UPDATE (37 bytes)
    0xFF,0xFF,0xFF,0xFF, 0xFF,0xFF,0xFF,0xFF,
    0xFF,0xFF,0xFF,0xFF, 0xFF,0xFF,0xFF,0xFF,
    0x00, 0x25, 0x02,
    0x00, 0x00,  // withdrawn len = 0
    0x00, 0x12,  // path attrs len = 18
    // ORIGIN = IGP
    0x40, 0x01, 0x01, 0x00,
    // AS_PATH: seq len=1, AS=65000
    0x40, 0x02, 0x06, 0x02, 0x01, 0x00, 0x00, 0xFD, 0xE8,
    // NEXT_HOP = 192.0.2.1
    0x40, 0x03, 0x04, 0xC0, 0x00, 0x02, 0x01,
    // NLRI: 203.0.113.0/24
    0x18, 0xCB, 0x00, 0x71,
];

// ── Benchmarks ───────────────────────────────────────────────────────────────

fn bench_initiation(c: &mut Criterion) {
    let mut g = c.benchmark_group("bmp_parse");
    g.throughput(Throughput::Elements(1));
    g.bench_function("initiation_pdu", |b| {
        b.iter(|| parse_bmp_message(black_box(INITIATION_PDU), SPEAKER, DEFAULT_MAX_FRAME))
    });
    g.finish();
}

fn bench_route_monitor(c: &mut Criterion) {
    let pdus: &[(&str, &[u8])] = &[
        ("withdraw",  ROUTE_MONITOR_WITHDRAW),
        ("announce",  ROUTE_MONITOR_ANNOUNCE),
    ];
    let mut g = c.benchmark_group("bmp_parse");
    g.throughput(Throughput::Elements(1));
    for (name, pdu) in pdus {
        g.bench_with_input(
            BenchmarkId::new("route_monitor", name),
            pdu,
            |b, &pdu| b.iter(|| parse_bmp_message(black_box(pdu), SPEAKER, DEFAULT_MAX_FRAME)),
        );
    }
    g.finish();
}

fn bench_batch_throughput(c: &mut Criterion) {
    const N: u64 = 1000;
    let mut g = c.benchmark_group("bmp_parse");
    g.throughput(Throughput::Elements(N));
    g.bench_function("route_monitor_batch_1k", |b| {
        b.iter(|| {
            for _ in 0..N {
                let _ = parse_bmp_message(black_box(ROUTE_MONITOR_ANNOUNCE), SPEAKER, DEFAULT_MAX_FRAME);
            }
        })
    });
    g.finish();
}

criterion_group!(benches, bench_initiation, bench_route_monitor, bench_batch_throughput);
criterion_main!(benches);

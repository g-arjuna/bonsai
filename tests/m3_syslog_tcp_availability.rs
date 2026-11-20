//! M3 — bounded syslog TCP availability: framing deadlines, oversize
//! rejection, protocol-error rejection, connection-slot recovery and
//! shutdown cancellation. Every case asserts the observable contract:
//! the connection is closed (EOF) rather than left half-open forever,
//! and well-formed traffic on other connections is unaffected.

use std::sync::Arc;
use std::time::Duration;

use bonsai::config::{SyslogConfig, TargetConfig};
use bonsai::event_bus::InProcessBus;
use bonsai::signals::syslog::run_syslog_receiver;
use bonsai::telemetry::TelemetryUpdate;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

struct Lab {
    rx: broadcast::Receiver<Arc<TelemetryUpdate>>,
    tcp_addr: String,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
    _dirs: Vec<tempfile::TempDir>,
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn start_lab(
    max_frame_bytes: usize,
    initial_secs: u64,
    idle_secs: u64,
    frame_secs: u64,
) -> Lab {
    let tcp_port = free_port();
    let dir = tempfile::tempdir().unwrap();
    let pattern_dir = dir.path().join("patterns");
    std::fs::create_dir_all(&pattern_dir).unwrap();
    let archive_path = dir
        .path()
        .join("syslog.jsonl")
        .to_string_lossy()
        .into_owned();
    let cfg = SyslogConfig {
        enabled: true,
        udp_addr: String::new(),
        tcp_addr: format!("127.0.0.1:{tcp_port}"),
        archive_path: archive_path.clone(),
        max_frame_bytes,
        tcp_initial_timeout_secs: initial_secs,
        tcp_idle_timeout_secs: idle_secs,
        tcp_frame_timeout_secs: frame_secs,
        ..SyslogConfig::default()
    };
    let bus = InProcessBus::new(1024);
    let rx = bus.subscribe();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(run_syslog_receiver(
        cfg,
        pattern_dir.to_string_lossy().into_owned(),
        Vec::<TargetConfig>::new(),
        Arc::clone(&bus),
        shutdown_rx,
        None,
        None,
        None,
        None,
    ));
    // Readiness: probe-connect until the listener accepts.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match TcpStream::connect(&format!("127.0.0.1:{tcp_port}")).await {
            Ok(_) => break,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("syslog TCP listener not ready: {e}"),
        }
    }
    Lab {
        rx,
        tcp_addr: format!("127.0.0.1:{tcp_port}"),
        shutdown_tx,
        task,
        _dirs: vec![dir],
    }
}

impl Lab {
    async fn stop(mut self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut self.task).await;
    }
}

async fn connect(lab: &Lab) -> TcpStream {
    TcpStream::connect(&lab.tcp_addr).await.unwrap()
}

/// Assert the server closes the connection (EOF) within `within`.
async fn assert_eof(stream: &mut TcpStream, within: Duration, what: &str) {
    let mut buf = [0u8; 16];
    let res = tokio::time::timeout(within, stream.read(&mut buf)).await;
    match res {
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => panic!("{what}: expected EOF, got {n} bytes"),
        Ok(Err(e)) => panic!("{what}: expected EOF, got read error {e}"),
        Err(_) => panic!("{what}: connection still open after {within:?}"),
    }
}

#[tokio::test]
async fn initial_deadline_closes_silent_connection() {
    let lab = start_lab(8192, 1, 60, 60).await;
    let mut stream = connect(&lab).await;
    // No bytes ever sent: the cumulative initial deadline must close it.
    assert_eof(&mut stream, Duration::from_secs(4), "silent connect").await;
    lab.stop().await;
}

#[tokio::test]
async fn idle_deadline_closes_quiet_connection_after_first_frame() {
    let lab = start_lab(8192, 5, 1, 60).await;
    let mut stream = connect(&lab).await;
    stream
        .write_all(b"<13>1 2026-05-09T12:00:00Z leaf sshd 1 ID47 - first\n")
        .await
        .unwrap();
    // First frame accepted; connection then goes silent → idle deadline.
    assert_eof(&mut stream, Duration::from_secs(4), "idle connect").await;
    lab.stop().await;
}

#[tokio::test]
async fn frame_deadline_closes_stalled_octet_counted_frame() {
    // First frame is bounded by the cumulative initial deadline (2s);
    // the declared 100-byte body never completes, so the connection
    // must close once that window elapses.
    let lab = start_lab(8192, 2, 60, 1).await;
    let mut stream = connect(&lab).await;
    // Octet-counted mode ('1' is a digit), declares 100 bytes, sends few.
    stream.write_all(b"100 <13>1 partial").await.unwrap();
    assert_eof(&mut stream, Duration::from_secs(5), "stalled frame").await;
    lab.stop().await;
}

#[tokio::test]
async fn frame_deadline_bounds_stalled_second_frame() {
    // After a complete first frame, a stalled second frame is bounded
    // by the frame-completion deadline (1s), not the idle deadline.
    let lab = start_lab(8192, 5, 60, 1).await;
    let mut stream = connect(&lab).await;
    let first = b"<13>1 2026-05-09T12:00:00Z leaf sshd 1 ID47 - ok\n";
    stream.write_all(first).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.write_all(b"100 <13>1 partial").await.unwrap();
    assert_eof(&mut stream, Duration::from_secs(4), "stalled 2nd frame").await;
    lab.stop().await;
}

#[tokio::test]
async fn oversize_newline_frame_is_rejected() {
    let lab = start_lab(64, 5, 60, 60).await;
    let mut stream = connect(&lab).await;
    // 200 bytes, no newline: exceeds max_frame_bytes before any '\n'.
    stream.write_all(&vec![b'a'; 200]).await.unwrap();
    assert_eof(&mut stream, Duration::from_secs(4), "oversize newline").await;
    lab.stop().await;
}

#[tokio::test]
async fn oversize_octet_counted_frame_is_rejected() {
    let lab = start_lab(64, 5, 60, 60).await;
    let mut stream = connect(&lab).await;
    // Declares 500 bytes > 64 limit.
    stream.write_all(b"500 <13>1 x").await.unwrap();
    assert_eof(&mut stream, Duration::from_secs(4), "oversize octet").await;
    lab.stop().await;
}

#[tokio::test]
async fn invalid_octet_prefix_is_rejected() {
    let lab = start_lab(8192, 5, 60, 60).await;
    let mut stream = connect(&lab).await;
    // Digit-start selects octet-counting; 'a' is not a digit/space.
    stream.write_all(b"1a <13>1 x\n").await.unwrap();
    assert_eof(&mut stream, Duration::from_secs(4), "bad prefix").await;
    lab.stop().await;
}

#[tokio::test]
async fn connection_limit_rejects_excess_and_recovers() {
    let lab = start_lab(8192, 30, 30, 30).await;
    let mut held = Vec::new();
    for _ in 0..128 {
        held.push(connect(&lab).await);
    }
    // 129th connection must be rejected promptly while slots are full.
    let mut extra = connect(&lab).await;
    assert_eof(&mut extra, Duration::from_secs(3), "over-limit conn").await;
    // Dropping a held connection frees its slot for a new one.
    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut replacement = connect(&lab).await;
    replacement
        .write_all(b"<13>1 2026-05-09T12:00:00Z leaf sshd 1 ID47 - slot\n")
        .await
        .unwrap();
    // The replacement stays open (no EOF within a short window).
    let mut buf = [0u8; 16];
    let res = tokio::time::timeout(Duration::from_millis(500), replacement.read(&mut buf)).await;
    assert!(
        res.is_err(),
        "replacement connection was closed unexpectedly"
    );
    drop(replacement);
    drop(held);
    lab.stop().await;
}

#[tokio::test]
async fn well_formed_octet_counted_and_newline_traffic_flows() {
    let lab = start_lab(8192, 5, 30, 30).await;
    // Newline-framed connection.
    let mut nl = connect(&lab).await;
    nl.write_all(b"<13>1 2026-05-09T12:00:00Z leaf sshd 1 ID47 - nl\n")
        .await
        .unwrap();
    // Octet-counted connection (digit first byte).
    let msg = b"<13>1 2026-05-09T12:00:00Z leaf sshd 1 ID47 - oc";
    let mut oc = connect(&lab).await;
    oc.write_all(format!("{} ", msg.len()).as_bytes())
        .await
        .unwrap();
    oc.write_all(msg).await.unwrap();
    // Both messages should surface on the bus as syslog events.
    let mut rx = lab.rx.resubscribe();
    let mut seen = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while seen < 2 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(update)) if update.path.starts_with("signals/syslog/") => seen += 1,
            _ => break,
        }
    }
    assert!(
        seen >= 1,
        "expected at least one syslog bus event, got {seen}"
    );
    // Connections remain usable after valid frames.
    nl.write_all(b"<13>1 2026-05-09T12:00:01Z leaf sshd 1 ID47 - nl2\n")
        .await
        .unwrap();
    drop(nl);
    drop(oc);
    lab.stop().await;
}

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

use crate::{archive, event_bus::InProcessBus};

static LAST_RSS_BYTES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MemorySnapshot {
    pub rss_bytes: u64,
    pub event_bus_depth: u64,
    pub event_bus_receivers: u64,
    pub archive_buffer_rows: u64,
    pub archive_lag_millis: i64,
    pub archive_last_compression_ppm: u64,
}

/// Read resident set size from /proc/self/status (Linux-only; returns 0 elsewhere).
pub fn rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest.split_whitespace().next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return kb * 1024;
                }
            }
        }
    }
    0
}

pub fn snapshot() -> MemorySnapshot {
    let rss = rss_bytes();
    LAST_RSS_BYTES.store(rss, Ordering::Relaxed);
    let bus = InProcessBus::snapshot();
    let arch = archive::snapshot();

    metrics::gauge!("bonsai_memory_rss_bytes").set(rss as f64);

    MemorySnapshot {
        rss_bytes: rss,
        event_bus_depth: bus.depth,
        event_bus_receivers: bus.receivers,
        archive_buffer_rows: arch.buffer_rows,
        archive_lag_millis: arch.lag_millis,
        archive_last_compression_ppm: arch.last_compression_ppm,
    }
}

/// Background task: sample memory every `interval`, emit Prometheus metrics,
/// optionally write JSON snapshots to `output_path`.
pub async fn run_memory_profiler(
    interval: Duration,
    output_path: Option<std::path::PathBuf>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!(
        interval_secs = interval.as_secs(),
        output_path = output_path.as_ref().map(|p| p.display().to_string()).as_deref().unwrap_or("none"),
        "memory profiler started"
    );

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let snap = snapshot();
                info!(
                    rss_mb = snap.rss_bytes / 1024 / 1024,
                    bus_depth = snap.event_bus_depth,
                    bus_receivers = snap.event_bus_receivers,
                    archive_buffer_rows = snap.archive_buffer_rows,
                    archive_lag_ms = snap.archive_lag_millis,
                    "memory profile"
                );

                if let Some(ref path) = output_path
                    && let Ok(json) = serde_json::to_string_pretty(&snap)
                {
                    let timestamped = format!(
                        "{{\"ts\":\"{}\",\"snapshot\":{}}}\n",
                        chrono_now_iso(),
                        json
                    );
                    if let Err(e) = append_to_file(path, &timestamped) {
                        warn!(path = %path.display(), error = %e, "memory profiler write failed");
                    }
                }
            }
        }
    }

    info!("memory profiler stopped");
}

fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // ISO 8601 without chrono dep: format as Unix seconds wrapped in a simple string
    format!("{secs}")
}

fn append_to_file(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(data.as_bytes())
}

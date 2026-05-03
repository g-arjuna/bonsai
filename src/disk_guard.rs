use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::StorageConfig;

#[derive(Debug, Clone, Default)]
pub struct DiskUsageSnapshot {
    pub archive_bytes: u64,
    pub archive_max_bytes: u64,
    pub archive_pct: u8,
    pub graph_bytes: u64,
    pub graph_max_bytes: u64,
    pub graph_pct: u8,
}

/// Walk `path` and sum all file sizes. Returns 0 if path does not exist.
pub fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(meta) = fs::metadata(&p) {
                total += meta.len();
            }
        }
    }
    total
}

fn usage_pct(used: u64, max: u64) -> u8 {
    if max == 0 {
        return 0;
    }
    ((used * 100) / max).min(100) as u8
}

pub fn snapshot(archive_path: &Path, graph_path: &Path, cfg: &StorageConfig) -> DiskUsageSnapshot {
    let archive_bytes = dir_size_bytes(archive_path);
    let graph_bytes = dir_size_bytes(graph_path);

    let archive_pct = usage_pct(archive_bytes, cfg.max_archive_bytes);
    let graph_pct = usage_pct(graph_bytes, cfg.max_graph_bytes);

    metrics::gauge!("bonsai_archive_disk_bytes").set(archive_bytes as f64);
    metrics::gauge!("bonsai_graph_disk_bytes").set(graph_bytes as f64);
    if cfg.max_archive_bytes > 0 {
        metrics::gauge!("bonsai_archive_disk_use_pct").set(archive_pct as f64);
    }
    if cfg.max_graph_bytes > 0 {
        metrics::gauge!("bonsai_graph_disk_use_pct").set(graph_pct as f64);
    }

    DiskUsageSnapshot {
        archive_bytes,
        archive_max_bytes: cfg.max_archive_bytes,
        archive_pct,
        graph_bytes,
        graph_max_bytes: cfg.max_graph_bytes,
        graph_pct,
    }
}

/// Background task: check disk usage every `cfg.check_interval_secs`.
///
/// Logs a warning when usage exceeds `cfg.warn_threshold_pct` of the cap.
/// When usage reaches 100%, logs an error — the caller is responsible for
/// triggering retention or dropping data.
pub async fn run_disk_guard(
    archive_path: PathBuf,
    graph_path: PathBuf,
    cfg: StorageConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let interval = Duration::from_secs(cfg.check_interval_secs.max(30));
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!(
        check_interval_secs = cfg.check_interval_secs,
        max_archive_gb = cfg.max_archive_bytes / 1024 / 1024 / 1024,
        max_graph_gb = cfg.max_graph_bytes / 1024 / 1024 / 1024,
        warn_threshold_pct = cfg.warn_threshold_pct,
        "disk guard started"
    );

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let snap = snapshot(&archive_path, &graph_path, &cfg);

                let warn_pct = cfg.warn_threshold_pct;

                if cfg.max_archive_bytes > 0 {
                    let archive_gb = snap.archive_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                    let max_gb = snap.archive_max_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                    if snap.archive_pct >= 100 {
                        warn!(
                            archive_gb = format!("{archive_gb:.2}"),
                            max_gb = format!("{max_gb:.2}"),
                            "archive has reached its size cap — retention should be running"
                        );
                        metrics::counter!("bonsai_disk_cap_reached_total",
                            "component" => "archive"
                        ).increment(1);
                    } else if snap.archive_pct >= warn_pct {
                        warn!(
                            pct = snap.archive_pct,
                            archive_gb = format!("{archive_gb:.2}"),
                            max_gb = format!("{max_gb:.2}"),
                            "archive disk usage is above warn threshold"
                        );
                    } else {
                        info!(
                            pct = snap.archive_pct,
                            archive_gb = format!("{archive_gb:.2}"),
                            "archive disk check"
                        );
                    }
                }

                if cfg.max_graph_bytes > 0 {
                    let graph_gb = snap.graph_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                    let max_gb = snap.graph_max_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
                    if snap.graph_pct >= 100 {
                        warn!(
                            graph_gb = format!("{graph_gb:.2}"),
                            max_gb = format!("{max_gb:.2}"),
                            "graph database has reached its size cap"
                        );
                        metrics::counter!("bonsai_disk_cap_reached_total",
                            "component" => "graph"
                        ).increment(1);
                    } else if snap.graph_pct >= warn_pct {
                        warn!(
                            pct = snap.graph_pct,
                            graph_gb = format!("{graph_gb:.2}"),
                            max_gb = format!("{max_gb:.2}"),
                            "graph disk usage is above warn threshold"
                        );
                    }
                }
            }
        }
    }

    info!("disk guard stopped");
}

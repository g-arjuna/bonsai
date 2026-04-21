use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use time::OffsetDateTime;
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};

use crate::event_bus::InProcessBus;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate};

#[derive(Debug)]
struct FlushStats {
    path: PathBuf,
    rows: usize,
    file_bytes: u64,
    raw_bytes: usize,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct ArchivePartition {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    target: String,
}

pub async fn run_archiver(
    bus: Arc<InProcessBus>,
    archive_path: PathBuf,
    flush_interval: Duration,
    max_batch_rows: usize,
) -> Result<()> {
    let mut rx = bus.subscribe();
    let mut buffer: Vec<TelemetryUpdate> = Vec::with_capacity(max_batch_rows);
    let mut flush_timer = tokio::time::interval(flush_interval);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    info!(
        path = %archive_path.display(),
        flush_interval_seconds = flush_interval.as_secs(),
        max_batch_rows,
        "archive consumer started"
    );

    loop {
        tokio::select! {
            recv = rx.recv() => match recv {
                Ok(update) => {
                    buffer.push(update);
                    if buffer.len() >= max_batch_rows {
                        flush_buffer(std::mem::take(&mut buffer), archive_path.clone()).await?;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(dropped = n, "archive consumer lagged on event bus");
                }
                Err(RecvError::Closed) => {
                    if !buffer.is_empty() {
                        flush_buffer(std::mem::take(&mut buffer), archive_path.clone()).await?;
                    }
                    info!("archive consumer stopping: event bus closed");
                    break;
                }
            },
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush_buffer(std::mem::take(&mut buffer), archive_path.clone()).await?;
                }
            }
        }
    }

    Ok(())
}

async fn flush_buffer(buffer: Vec<TelemetryUpdate>, archive_root: PathBuf) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }

    let stats = tokio::task::spawn_blocking(move || flush_buffer_blocking(buffer, &archive_root))
        .await
        .context("archive flush panicked")??;

    for stat in stats {
        let compression_ratio = if stat.file_bytes > 0 {
            stat.raw_bytes as f64 / stat.file_bytes as f64
        } else {
            0.0
        };
        info!(
            path = %stat.path.display(),
            rows = stat.rows,
            file_bytes = stat.file_bytes,
            raw_bytes = stat.raw_bytes,
            compression_ratio,
            "archive flush wrote parquet batch"
        );
    }

    Ok(())
}

fn flush_buffer_blocking(
    buffer: Vec<TelemetryUpdate>,
    archive_root: &Path,
) -> Result<Vec<FlushStats>> {
    let mut grouped: HashMap<ArchivePartition, Vec<TelemetryUpdate>> = HashMap::new();
    for update in buffer {
        let partition = ArchivePartition::from_update(&update)?;
        grouped.entry(partition).or_default().push(update);
    }

    let mut stats = Vec::with_capacity(grouped.len());
    for (partition, updates) in grouped {
        let dir = archive_root
            .join(format!("{:04}", partition.year))
            .join(format!("{:02}", partition.month))
            .join(format!("{:02}", partition.day));
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create archive directory '{}'", dir.display()))?;

        let file_name = batch_file_name(&partition, &updates);
        let path = dir.join(file_name);
        stats.push(write_partition_file(&path, updates)?);
    }

    Ok(stats)
}

fn write_partition_file(path: &Path, updates: Vec<TelemetryUpdate>) -> Result<FlushStats> {
    let rows = updates.len();
    let mut timestamp_ns = Vec::with_capacity(rows);
    let mut target = Vec::with_capacity(rows);
    let mut vendor = Vec::with_capacity(rows);
    let mut hostname = Vec::with_capacity(rows);
    let mut telemetry_path = Vec::with_capacity(rows);
    let mut value = Vec::with_capacity(rows);
    let mut event_type = Vec::with_capacity(rows);
    let mut raw_bytes = 0usize;

    for update in updates {
        let value_json = update.value.to_string();
        let classified = classified_event_type(&update);

        raw_bytes += update.target.len()
            + update.vendor.len()
            + update.hostname.len()
            + update.path.len()
            + value_json.len()
            + classified.len()
            + std::mem::size_of::<i64>();

        timestamp_ns.push(update.timestamp_ns);
        target.push(update.target);
        vendor.push(update.vendor);
        hostname.push(update.hostname);
        telemetry_path.push(update.path);
        value.push(value_json);
        event_type.push(classified.to_string());
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp_ns", DataType::Int64, false),
        Field::new("target", DataType::Utf8, false),
        Field::new("vendor", DataType::Utf8, false),
        Field::new("hostname", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
        Field::new("event_type", DataType::Utf8, false),
    ]));

    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(timestamp_ns)) as ArrayRef,
            Arc::new(StringArray::from(target)) as ArrayRef,
            Arc::new(StringArray::from(vendor)) as ArrayRef,
            Arc::new(StringArray::from(hostname)) as ArrayRef,
            Arc::new(StringArray::from(telemetry_path)) as ArrayRef,
            Arc::new(StringArray::from(value)) as ArrayRef,
            Arc::new(StringArray::from(event_type)) as ArrayRef,
        ],
    )
    .context("failed to build archive record batch")?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).context("invalid zstd level")?,
        ))
        .build();

    let file = File::create(path)
        .with_context(|| format!("failed to create archive file '{}'", path.display()))?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))
        .with_context(|| format!("failed to open parquet writer '{}'", path.display()))?;
    writer
        .write(&batch)
        .with_context(|| format!("failed to write parquet batch '{}'", path.display()))?;
    writer
        .close()
        .with_context(|| format!("failed to close parquet writer '{}'", path.display()))?;

    let file_bytes = fs::metadata(path)
        .with_context(|| format!("failed to stat archive file '{}'", path.display()))?
        .len();

    Ok(FlushStats {
        path: path.to_path_buf(),
        rows,
        file_bytes,
        raw_bytes,
    })
}

fn classified_event_type(update: &TelemetryUpdate) -> &'static str {
    match update.classify() {
        TelemetryEvent::InterfaceStats { .. } => "interface_stats",
        TelemetryEvent::BfdSessionState { .. } => "bfd_session_state",
        TelemetryEvent::BgpNeighborState { .. } => "bgp_neighbor_state",
        TelemetryEvent::LldpNeighbor { .. } => "lldp_neighbor",
        TelemetryEvent::InterfaceOperStatus { .. } => "interface_oper_status",
        TelemetryEvent::Ignored => "ignored",
    }
}

fn batch_file_name(partition: &ArchivePartition, updates: &[TelemetryUpdate]) -> String {
    let min_ts = updates
        .iter()
        .map(|update| update.timestamp_ns)
        .min()
        .unwrap_or_default();
    let max_ts = updates
        .iter()
        .map(|update| update.timestamp_ns)
        .max()
        .unwrap_or_default();
    format!(
        "{}__hour-{:02}__{}-{}__rows-{}.parquet",
        sanitize_for_filename(&partition.target),
        partition.hour,
        min_ts,
        max_ts,
        updates.len()
    )
}

fn sanitize_for_filename(input: &str) -> String {
    input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

impl ArchivePartition {
    fn from_update(update: &TelemetryUpdate) -> Result<Self> {
        let ts = OffsetDateTime::from_unix_timestamp_nanos(update.timestamp_ns as i128)
            .with_context(|| format!("invalid telemetry timestamp_ns {}", update.timestamp_ns))?;
        Ok(Self {
            year: ts.year(),
            month: ts.month() as u8,
            day: ts.day(),
            hour: ts.hour(),
            target: update.target.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn batch_file_name_sanitizes_target() {
        let partition = ArchivePartition {
            year: 2026,
            month: 4,
            day: 20,
            hour: 14,
            target: "10.1.1.1:57400".to_string(),
        };
        let updates = vec![TelemetryUpdate {
            target: partition.target.clone(),
            vendor: "nokia_srl".to_string(),
            hostname: "leaf1".to_string(),
            timestamp_ns: 123,
            path: "interfaces/interface[name=ethernet-1/1]/state/counters".to_string(),
            value: json!({"in-octets": 1}),
        }];

        let name = batch_file_name(&partition, &updates);
        assert!(name.starts_with("10.1.1.1_57400__hour-14__"));
        assert!(name.ends_with(".parquet"));
    }
}

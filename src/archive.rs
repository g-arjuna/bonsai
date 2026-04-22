use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use time::OffsetDateTime;
use tokio::sync::{broadcast::error::RecvError, watch};
use tracing::{info, warn};

use crate::event_bus::InProcessBus;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate};

#[derive(Debug)]
struct FlushStats {
    path: PathBuf,
    rows: usize,
    total_rows: usize,
    file_bytes: u64,
    raw_bytes: usize,
    total_raw_bytes: usize,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct ArchivePartition {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    hour_start_ns: i64,
    target: String,
}

#[derive(Debug)]
struct CloseStats {
    path: PathBuf,
    total_rows: usize,
    file_bytes: u64,
    total_raw_bytes: usize,
}

#[derive(Debug, Default)]
struct ArchiveWriteStats {
    flushes: Vec<FlushStats>,
    closes: Vec<CloseStats>,
}

pub async fn run_archiver(
    bus: Arc<InProcessBus>,
    archive_path: PathBuf,
    flush_interval: Duration,
    max_batch_rows: usize,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut rx = bus.subscribe();
    let mut buffer: Vec<TelemetryUpdate> = Vec::with_capacity(max_batch_rows);
    let mut flush_timer = tokio::time::interval(flush_interval);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let writer = Arc::new(Mutex::new(HourlyArchiveWriter::new(archive_path.clone())));

    info!(
        path = %archive_path.display(),
        flush_interval_seconds = flush_interval.as_secs(),
        max_batch_rows,
        "archive consumer started"
    );
    record_archive_lag(&buffer);

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if !buffer.is_empty() {
                    flush_buffer(std::mem::take(&mut buffer), Arc::clone(&writer)).await?;
                    record_archive_lag(&buffer);
                }
                close_archive_writers(Arc::clone(&writer)).await?;
                info!("archive consumer stopping: shutdown signal received");
                break;
            }
            recv = rx.recv() => match recv {
                Ok(update) => {
                    buffer.push(update);
                    record_archive_lag(&buffer);
                    if buffer.len() >= max_batch_rows {
                        flush_buffer(std::mem::take(&mut buffer), Arc::clone(&writer)).await?;
                        record_archive_lag(&buffer);
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(dropped = n, "archive consumer lagged on event bus");
                }
                Err(RecvError::Closed) => {
                    if !buffer.is_empty() {
                        flush_buffer(std::mem::take(&mut buffer), Arc::clone(&writer)).await?;
                        record_archive_lag(&buffer);
                    }
                    close_archive_writers(Arc::clone(&writer)).await?;
                    info!("archive consumer stopping: event bus closed");
                    break;
                }
            },
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush_buffer(std::mem::take(&mut buffer), Arc::clone(&writer)).await?;
                    record_archive_lag(&buffer);
                }
            }
        }
    }

    Ok(())
}

fn record_archive_lag(buffer: &[TelemetryUpdate]) {
    let oldest_timestamp_ns = buffer
        .iter()
        .filter_map(|update| (update.timestamp_ns > 0).then_some(update.timestamp_ns))
        .min();
    let lag_seconds = oldest_timestamp_ns
        .map(|timestamp_ns| (now_ns().saturating_sub(timestamp_ns) as f64) / 1_000_000_000.0)
        .unwrap_or(0.0);
    metrics::gauge!("bonsai_archive_lag_seconds").set(lag_seconds);
    metrics::gauge!("bonsai_archive_buffer_rows").set(buffer.len() as f64);
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

async fn flush_buffer(
    buffer: Vec<TelemetryUpdate>,
    writer: Arc<Mutex<HourlyArchiveWriter>>,
) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }

    let stats = tokio::task::spawn_blocking(move || {
        let mut writer = writer
            .lock()
            .map_err(|_| anyhow::anyhow!("archive writer lock poisoned"))?;
        writer.append(buffer)
    })
    .await
    .context("archive flush panicked")??;

    for stat in stats.flushes {
        let compression_ratio = if stat.file_bytes > 0 {
            stat.raw_bytes as f64 / stat.file_bytes as f64
        } else {
            0.0
        };
        info!(
            path = %stat.path.display(),
            rows = stat.rows,
            total_rows = stat.total_rows,
            file_bytes = stat.file_bytes,
            raw_bytes = stat.raw_bytes,
            total_raw_bytes = stat.total_raw_bytes,
            compression_ratio,
            "archive flush appended parquet row group"
        );
    }
    log_close_stats(stats.closes);

    Ok(())
}

async fn close_archive_writers(writer: Arc<Mutex<HourlyArchiveWriter>>) -> Result<()> {
    let stats = tokio::task::spawn_blocking(move || {
        let mut writer = writer
            .lock()
            .map_err(|_| anyhow::anyhow!("archive writer lock poisoned"))?;
        writer.close_all()
    })
    .await
    .context("archive close panicked")??;

    log_close_stats(stats);

    Ok(())
}

fn log_close_stats(stats: Vec<CloseStats>) {
    for stat in stats {
        let compression_ratio = if stat.file_bytes > 0 {
            stat.total_raw_bytes as f64 / stat.file_bytes as f64
        } else {
            0.0
        };
        info!(
            path = %stat.path.display(),
            total_rows = stat.total_rows,
            file_bytes = stat.file_bytes,
            total_raw_bytes = stat.total_raw_bytes,
            compression_ratio,
            "archive closed hourly parquet file"
        );
    }
}

struct HourlyArchiveWriter {
    archive_root: PathBuf,
    open: HashMap<ArchivePartition, OpenPartitionWriter>,
}

impl HourlyArchiveWriter {
    fn new(archive_root: PathBuf) -> Self {
        Self {
            archive_root,
            open: HashMap::new(),
        }
    }

    fn append(&mut self, buffer: Vec<TelemetryUpdate>) -> Result<ArchiveWriteStats> {
        let mut grouped: HashMap<ArchivePartition, Vec<TelemetryUpdate>> = HashMap::new();
        let mut max_hour_start_ns = i64::MIN;
        for update in buffer {
            let partition = ArchivePartition::from_update(&update)?;
            max_hour_start_ns = max_hour_start_ns.max(partition.hour_start_ns);
            grouped.entry(partition).or_default().push(update);
        }

        let mut stats = ArchiveWriteStats {
            flushes: Vec::with_capacity(grouped.len()),
            closes: Vec::new(),
        };
        for (partition, updates) in grouped {
            let writer = self.open_partition_writer(partition)?;
            stats.flushes.push(writer.append(updates)?);
        }

        stats.closes = self.close_hours_before(max_hour_start_ns)?;
        Ok(stats)
    }

    fn close_all(&mut self) -> Result<Vec<CloseStats>> {
        let writers = std::mem::take(&mut self.open);
        writers
            .into_values()
            .map(OpenPartitionWriter::close)
            .collect()
    }

    fn close_hours_before(&mut self, hour_start_ns: i64) -> Result<Vec<CloseStats>> {
        let stale: Vec<_> = self
            .open
            .keys()
            .filter(|partition| partition.hour_start_ns < hour_start_ns)
            .cloned()
            .collect();
        let mut stats = Vec::with_capacity(stale.len());
        for partition in stale {
            if let Some(writer) = self.open.remove(&partition) {
                stats.push(writer.close()?);
            }
        }
        Ok(stats)
    }

    fn open_partition_writer(
        &mut self,
        partition: ArchivePartition,
    ) -> Result<&mut OpenPartitionWriter> {
        if !self.open.contains_key(&partition) {
            let writer = OpenPartitionWriter::open(&self.archive_root, &partition)?;
            self.open.insert(partition.clone(), writer);
        }

        Ok(self
            .open
            .get_mut(&partition)
            .expect("partition writer was just inserted"))
    }
}

struct OpenPartitionWriter {
    path: PathBuf,
    writer: ArrowWriter<File>,
    total_rows: usize,
    total_raw_bytes: usize,
}

impl OpenPartitionWriter {
    fn open(archive_root: &Path, partition: &ArchivePartition) -> Result<Self> {
        let dir = archive_root
            .join(format!("{:04}", partition.year))
            .join(format!("{:02}", partition.month))
            .join(format!("{:02}", partition.day));
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create archive directory '{}'", dir.display()))?;

        let (path, file) = create_hourly_archive_file(&dir, partition)?;
        let writer = ArrowWriter::try_new(file, archive_schema(), Some(writer_properties()?))
            .with_context(|| format!("failed to open parquet writer '{}'", path.display()))?;

        Ok(Self {
            path,
            writer,
            total_rows: 0,
            total_raw_bytes: 0,
        })
    }

    fn append(&mut self, updates: Vec<TelemetryUpdate>) -> Result<FlushStats> {
        let rows = updates.len();
        let (batch, raw_bytes) = build_record_batch(updates)?;
        self.writer
            .write(&batch)
            .with_context(|| format!("failed to write parquet batch '{}'", self.path.display()))?;
        self.total_rows += rows;
        self.total_raw_bytes += raw_bytes;

        let file_bytes = fs::metadata(&self.path)
            .with_context(|| format!("failed to stat archive file '{}'", self.path.display()))?
            .len();

        Ok(FlushStats {
            path: self.path.clone(),
            rows,
            total_rows: self.total_rows,
            file_bytes,
            raw_bytes,
            total_raw_bytes: self.total_raw_bytes,
        })
    }

    fn close(self) -> Result<CloseStats> {
        self.writer
            .close()
            .with_context(|| format!("failed to close parquet writer '{}'", self.path.display()))?;
        let file_bytes = fs::metadata(&self.path)
            .with_context(|| format!("failed to stat archive file '{}'", self.path.display()))?
            .len();

        Ok(CloseStats {
            path: self.path,
            total_rows: self.total_rows,
            file_bytes,
            total_raw_bytes: self.total_raw_bytes,
        })
    }
}

fn create_hourly_archive_file(dir: &Path, partition: &ArchivePartition) -> Result<(PathBuf, File)> {
    for part in 0.. {
        let path = dir.join(hourly_file_name(partition, part));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create archive file '{}'", path.display())
                });
            }
        }
    }

    unreachable!("unbounded part search must return or error")
}

fn archive_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ns", DataType::Int64, false),
        Field::new("target", DataType::Utf8, false),
        Field::new("vendor", DataType::Utf8, false),
        Field::new("hostname", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
        Field::new("event_type", DataType::Utf8, false),
    ]))
}

fn writer_properties() -> Result<WriterProperties> {
    Ok(WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).context("invalid zstd level")?,
        ))
        .build())
}

fn build_record_batch(updates: Vec<TelemetryUpdate>) -> Result<(RecordBatch, usize)> {
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

    let schema = archive_schema();

    let batch = RecordBatch::try_new(
        schema,
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

    Ok((batch, raw_bytes))
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

fn hourly_file_name(partition: &ArchivePartition, part: usize) -> String {
    let suffix = if part == 0 {
        String::new()
    } else {
        format!("__part-{part:02}")
    };
    format!(
        "{}__hour-{:02}{suffix}.parquet",
        sanitize_for_filename(&partition.target),
        partition.hour,
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
        let hour_start_ns = update
            .timestamp_ns
            .div_euclid(3_600_000_000_000)
            .saturating_mul(3_600_000_000_000);
        Ok(Self {
            year: ts.year(),
            month: ts.month() as u8,
            day: ts.day(),
            hour: ts.hour(),
            hour_start_ns,
            target: update.target.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use parquet::file::reader::{FileReader, SerializedFileReader};
    use serde_json::json;

    use super::*;

    #[test]
    fn hourly_file_name_sanitizes_target() {
        let partition = ArchivePartition {
            year: 2026,
            month: 4,
            day: 20,
            hour: 14,
            hour_start_ns: 1_776_693_600_000_000_000,
            target: "10.1.1.1:57400".to_string(),
        };

        let name = hourly_file_name(&partition, 0);
        assert_eq!(name, "10.1.1.1_57400__hour-14.parquet");
        assert_eq!(
            hourly_file_name(&partition, 2),
            "10.1.1.1_57400__hour-14__part-02.parquet"
        );
    }

    #[test]
    fn hourly_writer_reuses_file_across_flushes_for_same_target_hour() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut writer = HourlyArchiveWriter::new(tempdir.path().to_path_buf());
        let target = "10.1.1.1:57400";

        writer
            .append(vec![sample_update(target, 1_776_695_400_000_000_000, 1)])
            .unwrap();
        writer
            .append(vec![sample_update(target, 1_776_695_401_000_000_000, 2)])
            .unwrap();
        let close_stats = writer.close_all().unwrap();

        assert_eq!(close_stats.len(), 1);
        let files = parquet_files(tempdir.path());
        assert_eq!(files.len(), 1);
        let file = File::open(&files[0]).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        assert_eq!(reader.metadata().file_metadata().num_rows(), 2);
    }

    #[test]
    fn hourly_writer_rolls_file_at_hour_boundary() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut writer = HourlyArchiveWriter::new(tempdir.path().to_path_buf());
        let target = "10.1.1.1:57400";

        writer
            .append(vec![sample_update(target, 1_776_695_400_000_000_000, 1)])
            .unwrap();
        writer
            .append(vec![sample_update(target, 1_776_699_000_000_000_000, 2)])
            .unwrap();
        writer.close_all().unwrap();

        let files = parquet_files(tempdir.path());
        assert_eq!(files.len(), 2);
        let file_names: HashSet<_> = files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(file_names.contains("10.1.1.1_57400__hour-14.parquet"));
        assert!(file_names.contains("10.1.1.1_57400__hour-15.parquet"));
    }

    #[test]
    fn hourly_writer_limits_one_hour_four_targets_to_four_files() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut writer = HourlyArchiveWriter::new(tempdir.path().to_path_buf());
        let targets = [
            "10.1.1.1:57400",
            "10.1.1.2:57400",
            "10.1.1.3:57400",
            "10.1.1.4:57400",
        ];

        for flush in 0..5 {
            let updates = targets
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    sample_update(
                        target,
                        1_776_695_400_000_000_000 + (flush * 1_000_000_000) as i64,
                        index as i64,
                    )
                })
                .collect();
            writer.append(updates).unwrap();
        }
        writer.close_all().unwrap();

        let files = parquet_files(tempdir.path());
        assert_eq!(files.len(), 4, "{files:#?}");
        for path in files {
            let file = File::open(path).unwrap();
            let reader = SerializedFileReader::new(file).unwrap();
            assert_eq!(reader.metadata().file_metadata().num_rows(), 5);
        }
    }

    fn sample_update(target: &str, timestamp_ns: i64, value: i64) -> TelemetryUpdate {
        TelemetryUpdate {
            target: target.to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "leaf1".to_string(),
            timestamp_ns,
            path: "interfaces/interface[name=ethernet-1/1]/state/counters".to_string(),
            value: json!({"in-octets": value}),
        }
    }

    fn parquet_files(root: &Path) -> Vec<PathBuf> {
        let mut stack = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(path) = stack.pop() {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "parquet") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }
}

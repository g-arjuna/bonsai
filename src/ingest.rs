use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use prost::Message;
use tokio::sync::broadcast;
use tonic::Request;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tracing::{info, warn};

use crate::api::pb::{
    TelemetryIngestUpdate as ProtoTelemetryIngestUpdate, bonsai_graph_client::BonsaiGraphClient,
};
use crate::config::{CollectorQueueConfig, RuntimeTlsConfig};
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

const FORWARDER_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const COMPRESSION_STATS_INTERVAL: u64 = 1_000;
const QUEUE_DATA_FILE: &str = "queue.dat";
const QUEUE_ACK_FILE: &str = "queue.ack";
const QUEUE_RECORD_HEADER_BYTES: u64 = 12;

pub fn telemetry_to_ingest_update(
    collector_id: &str,
    update: &TelemetryUpdate,
) -> Result<ProtoTelemetryIngestUpdate> {
    Ok(ProtoTelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: update.target.clone(),
        vendor: update.vendor.clone(),
        hostname: update.hostname.clone(),
        timestamp_ns: update.timestamp_ns,
        path: update.path.clone(),
        value_msgpack: rmp_serde::to_vec(&update.value)
            .context("failed to serialize telemetry value as MessagePack")?,
    })
}

pub fn ingest_update_to_telemetry(update: ProtoTelemetryIngestUpdate) -> Result<TelemetryUpdate> {
    let value = rmp_serde::from_slice(&update.value_msgpack)
        .with_context(|| format!("invalid telemetry value_msgpack for path '{}'", update.path))?;

    Ok(TelemetryUpdate {
        target: update.target,
        vendor: update.vendor,
        hostname: update.hostname,
        timestamp_ns: update.timestamp_ns,
        path: update.path,
        value,
    })
}

pub async fn run_core_forwarder(
    bus: Arc<InProcessBus>,
    core_endpoint: String,
    collector_id: String,
    queue_config: CollectorQueueConfig,
    tls_config: RuntimeTlsConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let queue = match CollectorQueue::open(queue_config) {
        Ok(queue) => Arc::new(queue),
        Err(error) => {
            warn!(%collector_id, %error, "failed to open collector disk queue; forwarder disabled");
            return;
        }
    };
    let writer_collector_id = collector_id.clone();
    let writer_queue = Arc::clone(&queue);
    let writer_shutdown = shutdown.clone();
    let queue_writer = tokio::spawn(async move {
        if let Err(error) = queue_bus_updates(
            bus,
            writer_collector_id.clone(),
            writer_queue,
            writer_shutdown,
        )
        .await
        {
            warn!(collector_id = %writer_collector_id, %error, "collector queue writer stopped");
        }
    });
    let queue_logger = tokio::spawn(log_queue_status(
        collector_id.clone(),
        Arc::clone(&queue),
        shutdown.clone(),
    ));

    loop {
        if *shutdown.borrow() {
            queue_writer.abort();
            queue_logger.abort();
            return;
        }

        match forward_once(
            Arc::clone(&queue),
            &core_endpoint,
            &collector_id,
            &tls_config,
            shutdown.clone(),
        )
        .await
        {
            Ok(()) => return,
            Err(error) => {
                warn!(
                    %core_endpoint,
                    %collector_id,
                    %error,
                    delay = ?FORWARDER_RECONNECT_DELAY,
                    "collector forwarder disconnected"
                );
            }
        }

        tokio::select! {
            _ = shutdown.changed() => {
                queue_writer.abort();
                queue_logger.abort();
                return;
            }
            _ = tokio::time::sleep(FORWARDER_RECONNECT_DELAY) => {}
        }
    }
}

async fn forward_once(
    queue: Arc<CollectorQueue>,
    core_endpoint: &str,
    collector_id: &str,
    tls_config: &RuntimeTlsConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let channel = connect_core_channel(core_endpoint, tls_config)
        .await
        .with_context(|| format!("failed to connect to core ingest endpoint '{core_endpoint}'"))?;
    let mut client = BonsaiGraphClient::new(channel).send_compressed(CompressionEncoding::Zstd);

    info!(
        %core_endpoint,
        %collector_id,
        compression = "zstd",
        mtls = tls_config.enabled,
        "collector forwarder connected to core"
    );

    drain_queue_to_core(&mut client, queue, collector_id, shutdown).await
}

async fn drain_queue_to_core(
    client: &mut BonsaiGraphClient<Channel>,
    queue: Arc<CollectorQueue>,
    collector_id: &str,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut compression_stats = CompressionStats::default();
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }

        let batch = queue
            .next_batch(collector_id)
            .context("failed to read collector queue batch")?;
        if batch.records.is_empty() {
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                _ = queue.notified() => {}
            }
            continue;
        }

        let updates: Vec<_> = batch
            .records
            .iter()
            .map(|record| {
                compression_stats.observe(collector_id, &record.update);
                record.update.clone()
            })
            .collect();
        let response = client
            .telemetry_ingest(Request::new(tokio_stream::iter(updates)))
            .await
            .context("core telemetry ingest stream failed")?
            .into_inner();
        if !response.error.is_empty() {
            anyhow::bail!("core rejected telemetry ingest stream: {}", response.error);
        }

        let accepted = response.accepted as usize;
        if accepted == 0 {
            anyhow::bail!("core accepted zero records from non-empty telemetry ingest batch");
        }
        let ack_index = accepted.saturating_sub(1).min(batch.records.len() - 1);
        let ack_offset = batch.records[ack_index].next_offset;
        queue
            .ack(ack_offset, collector_id)
            .context("failed to ack collector queue records")?;

        info!(
            accepted = response.accepted,
            queued_remaining = queue
                .stats()
                .map(|stats| stats.pending_records)
                .unwrap_or(0),
            "collector queue batch delivered"
        );

        if accepted < batch.records.len() {
            anyhow::bail!(
                "core accepted only {accepted} of {} queued telemetry records",
                batch.records.len()
            );
        }
    }
}

async fn connect_core_channel(core_endpoint: &str, tls: &RuntimeTlsConfig) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(core_endpoint.to_string())
        .with_context(|| format!("invalid core ingest endpoint '{core_endpoint}'"))?;
    if tls.enabled {
        if !core_endpoint.trim_start().starts_with("https://") {
            anyhow::bail!(
                "runtime.tls.enabled requires runtime.core_ingest_endpoint to use https://"
            );
        }
        endpoint = endpoint
            .tls_config(client_tls_config(tls)?)
            .context("failed to configure runtime.tls for collector client")?;
    }
    endpoint
        .connect()
        .await
        .context("collector failed to connect to core")
}

fn client_tls_config(tls: &RuntimeTlsConfig) -> Result<ClientTlsConfig> {
    let ca_path = required_tls_path(tls.ca_cert.as_deref(), "runtime.tls.ca_cert")?;
    let cert_path = required_tls_path(tls.cert.as_deref(), "runtime.tls.cert")?;
    let key_path = required_tls_path(tls.key.as_deref(), "runtime.tls.key")?;
    let ca = fs::read(ca_path)
        .with_context(|| format!("failed to read runtime.tls.ca_cert '{ca_path}'"))?;
    let cert = fs::read(cert_path)
        .with_context(|| format!("failed to read runtime.tls.cert '{cert_path}'"))?;
    let key = fs::read(key_path)
        .with_context(|| format!("failed to read runtime.tls.key '{key_path}'"))?;

    let mut config = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca))
        .identity(Identity::from_pem(cert, key));
    if let Some(server_name) = tls.server_name.as_deref().filter(|value| !value.is_empty()) {
        config = config.domain_name(server_name.to_string());
    }
    Ok(config)
}

fn required_tls_path<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{field} is required when runtime.tls.enabled = true"))
}

async fn queue_bus_updates(
    bus: Arc<InProcessBus>,
    collector_id: String,
    queue: Arc<CollectorQueue>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut rx = bus.subscribe();
    loop {
        let update = tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            received = rx.recv() => received,
        };

        match update {
            Ok(update) => {
                let proto = telemetry_to_ingest_update(&collector_id, &update)?;
                queue.append(proto, &collector_id)?;
            }
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                warn!(dropped, "collector forwarder lagged on local event bus");
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

async fn log_queue_status(
    collector_id: String,
    queue: Arc<CollectorQueue>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let interval_seconds = queue.log_interval_seconds();
    if interval_seconds == 0 {
        return;
    }

    let mut interval = tokio::time::interval(Duration::from_secs(interval_seconds));
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = interval.tick() => {
                match queue.stats() {
                    Ok(stats) => log_queue_stats(&collector_id, &stats),
                    Err(error) => warn!(%collector_id, %error, "failed to read collector queue status"),
                }
            }
        }
    }
}

#[derive(Clone)]
struct QueuedRecord {
    update: ProtoTelemetryIngestUpdate,
    next_offset: u64,
}

struct QueueBatch {
    records: Vec<QueuedRecord>,
}

#[derive(Debug, Default)]
struct QueueStats {
    pending_records: u64,
    pending_bytes: u64,
    data_file_bytes: u64,
    max_bytes: u64,
    max_age_hours: u64,
}

struct CollectorQueue {
    inner: Mutex<CollectorQueueInner>,
    notify: tokio::sync::Notify,
    drain_batch_size: usize,
    log_interval_seconds: u64,
}

struct CollectorQueueInner {
    data_path: PathBuf,
    ack_path: PathBuf,
    max_bytes: u64,
    max_age_hours: u64,
}

impl CollectorQueue {
    fn open(config: CollectorQueueConfig) -> Result<Self> {
        fs::create_dir_all(&config.path)
            .with_context(|| format!("failed to create collector queue dir '{}'", config.path))?;
        let data_path = Path::new(&config.path).join(QUEUE_DATA_FILE);
        let ack_path = Path::new(&config.path).join(QUEUE_ACK_FILE);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&data_path)
            .with_context(|| {
                format!(
                    "failed to open collector queue data '{}'",
                    data_path.display()
                )
            })?;
        if !ack_path.exists() {
            fs::write(&ack_path, b"0").with_context(|| {
                format!(
                    "failed to initialize collector queue ack '{}'",
                    ack_path.display()
                )
            })?;
        }

        Ok(Self {
            inner: Mutex::new(CollectorQueueInner {
                data_path,
                ack_path,
                max_bytes: config.max_bytes,
                max_age_hours: config.max_age_hours,
            }),
            notify: tokio::sync::Notify::new(),
            drain_batch_size: config.drain_batch_size.max(1),
            log_interval_seconds: config.log_interval_seconds,
        })
    }

    fn append(&self, update: ProtoTelemetryIngestUpdate, collector_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("collector queue lock poisoned");
        inner.append(update)?;
        let stats = inner.stats()?;
        if stats.max_bytes > 0 && stats.data_file_bytes > stats.max_bytes {
            warn!(
                %collector_id,
                pending_records = stats.pending_records,
                pending_bytes = stats.pending_bytes,
                max_bytes = stats.max_bytes,
                "collector queue exceeds configured max_bytes; oldest records will be dropped on next drain"
            );
        }
        drop(inner);
        self.notify.notify_waiters();
        Ok(())
    }

    fn next_batch(&self, collector_id: &str) -> Result<QueueBatch> {
        let mut inner = self.inner.lock().expect("collector queue lock poisoned");
        inner.enforce_limits(collector_id)?;
        inner.next_batch(self.drain_batch_size, collector_id)
    }

    fn ack(&self, offset: u64, collector_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("collector queue lock poisoned");
        inner.write_ack(offset)?;
        inner.compact_reclaim_acked(collector_id)
    }

    fn stats(&self) -> Result<QueueStats> {
        let inner = self.inner.lock().expect("collector queue lock poisoned");
        inner.stats()
    }

    async fn notified(&self) {
        self.notify.notified().await;
    }

    fn log_interval_seconds(&self) -> u64 {
        self.log_interval_seconds
    }
}

impl CollectorQueueInner {
    fn append(&mut self, update: ProtoTelemetryIngestUpdate) -> Result<()> {
        let payload = update.encode_to_vec();
        if payload.len() > u32::MAX as usize {
            anyhow::bail!("collector queue record exceeds u32 length limit");
        }

        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.data_path)
            .with_context(|| {
                format!(
                    "failed to append collector queue '{}'",
                    self.data_path.display()
                )
            })?;
        file.write_all(&(payload.len() as u32).to_le_bytes())?;
        file.write_all(&now_unix_ns().to_le_bytes())?;
        file.write_all(&payload)?;
        file.sync_data()?;
        Ok(())
    }

    fn next_batch(&mut self, limit: usize, collector_id: &str) -> Result<QueueBatch> {
        let mut offset = self.read_ack()?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.data_path)
            .with_context(|| {
                format!(
                    "failed to read collector queue '{}'",
                    self.data_path.display()
                )
            })?;
        let len = file.metadata()?.len();
        if offset > len {
            warn!(%collector_id, offset, len, "collector queue ack offset exceeded data file; resetting to start");
            offset = 0;
            self.write_ack(0)?;
        }

        file.seek(SeekFrom::Start(offset))?;
        let mut records = Vec::new();
        while records.len() < limit {
            match read_record(&mut file, offset)? {
                Some(record) => {
                    offset = record.next_offset;
                    if self.is_expired(record.enqueued_at_ns) {
                        warn!(
                            %collector_id,
                            next_offset = record.next_offset,
                            "dropping expired collector queue record"
                        );
                        self.write_ack(record.next_offset)?;
                        continue;
                    }
                    records.push(QueuedRecord {
                        update: record.update,
                        next_offset: record.next_offset,
                    });
                }
                None => break,
            }
        }

        Ok(QueueBatch { records })
    }

    fn enforce_limits(&mut self, collector_id: &str) -> Result<()> {
        let records = self.unacked_records()?;
        let mut retained = Vec::new();
        let mut dropped_expired = 0_u64;
        for record in records {
            if self.is_expired(record.enqueued_at_ns) {
                dropped_expired += 1;
            } else {
                retained.push(record);
            }
        }

        let mut dropped_for_size = 0_u64;
        if self.max_bytes > 0 {
            while retained_size(&retained) > self.max_bytes {
                retained.remove(0);
                dropped_for_size += 1;
            }
        }

        if dropped_expired > 0 || dropped_for_size > 0 || self.read_ack()? > 0 {
            self.rewrite_records(&retained)?;
            warn!(
                %collector_id,
                dropped_expired,
                dropped_for_size,
                retained = retained.len(),
                "collector queue retention compacted records"
            );
        }
        Ok(())
    }

    fn compact_reclaim_acked(&mut self, collector_id: &str) -> Result<()> {
        let ack = self.read_ack()?;
        if ack == 0 {
            return Ok(());
        }
        let records = self.unacked_records()?;
        self.rewrite_records(&records)?;
        if !records.is_empty() {
            info!(
                %collector_id,
                retained = records.len(),
                "collector queue compacted acked records"
            );
        }
        Ok(())
    }

    fn unacked_records(&self) -> Result<Vec<RawQueuedRecord>> {
        let mut offset = self.read_ack()?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.data_path)?;
        let len = file.metadata()?.len();
        if offset > len {
            offset = 0;
        }
        file.seek(SeekFrom::Start(offset))?;

        let mut records = Vec::new();
        while let Some(record) = read_record(&mut file, offset)? {
            offset = record.next_offset;
            records.push(record);
        }
        Ok(records)
    }

    fn rewrite_records(&self, records: &[RawQueuedRecord]) -> Result<()> {
        let tmp_path = self.data_path.with_extension("dat.tmp");
        {
            let mut tmp = File::create(&tmp_path)?;
            for record in records {
                write_raw_record(&mut tmp, record)?;
            }
            tmp.sync_data()?;
        }
        fs::rename(&tmp_path, &self.data_path)?;
        self.write_ack(0)
    }

    fn stats(&self) -> Result<QueueStats> {
        let data_file_bytes = fs::metadata(&self.data_path)?.len();
        let records = self.unacked_records()?;
        Ok(QueueStats {
            pending_records: records.len() as u64,
            pending_bytes: retained_size(&records),
            data_file_bytes,
            max_bytes: self.max_bytes,
            max_age_hours: self.max_age_hours,
        })
    }

    fn read_ack(&self) -> Result<u64> {
        let raw = fs::read_to_string(&self.ack_path).with_context(|| {
            format!(
                "failed to read collector queue ack '{}'",
                self.ack_path.display()
            )
        })?;
        Ok(raw.trim().parse().unwrap_or(0))
    }

    fn write_ack(&self, offset: u64) -> Result<()> {
        fs::write(&self.ack_path, offset.to_string()).with_context(|| {
            format!(
                "failed to write collector queue ack '{}'",
                self.ack_path.display()
            )
        })
    }

    fn is_expired(&self, enqueued_at_ns: i64) -> bool {
        if self.max_age_hours == 0 {
            return false;
        }
        let max_age_ns = self.max_age_hours.saturating_mul(3_600_000_000_000);
        let age_ns = now_unix_ns().saturating_sub(enqueued_at_ns);
        age_ns as u64 > max_age_ns
    }
}

#[derive(Clone)]
struct RawQueuedRecord {
    enqueued_at_ns: i64,
    payload: Vec<u8>,
    update: ProtoTelemetryIngestUpdate,
    next_offset: u64,
}

fn read_record(file: &mut File, offset: u64) -> Result<Option<RawQueuedRecord>> {
    let mut len_bytes = [0_u8; 4];
    match file.read_exact(&mut len_bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            file.set_len(offset)?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let mut enqueued_bytes = [0_u8; 8];
    match file.read_exact(&mut enqueued_bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            file.set_len(offset)?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let payload_len = u32::from_le_bytes(len_bytes) as usize;
    let mut payload = vec![0_u8; payload_len];
    match file.read_exact(&mut payload) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            file.set_len(offset)?;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let update = ProtoTelemetryIngestUpdate::decode(payload.as_slice())
        .context("failed to decode collector queue protobuf record")?;
    let next_offset = offset + QUEUE_RECORD_HEADER_BYTES + payload_len as u64;

    Ok(Some(RawQueuedRecord {
        enqueued_at_ns: i64::from_le_bytes(enqueued_bytes),
        payload,
        update,
        next_offset,
    }))
}

fn write_raw_record(file: &mut File, record: &RawQueuedRecord) -> Result<()> {
    file.write_all(&(record.payload.len() as u32).to_le_bytes())?;
    file.write_all(&record.enqueued_at_ns.to_le_bytes())?;
    file.write_all(&record.payload)?;
    Ok(())
}

fn retained_size(records: &[RawQueuedRecord]) -> u64 {
    records
        .iter()
        .map(|record| QUEUE_RECORD_HEADER_BYTES + record.payload.len() as u64)
        .sum()
}

fn log_queue_stats(collector_id: &str, stats: &QueueStats) {
    let utilization = if stats.max_bytes > 0 {
        stats.data_file_bytes as f64 / stats.max_bytes as f64
    } else {
        0.0
    };
    if stats.max_bytes > 0 && utilization >= 0.80 {
        warn!(
            %collector_id,
            pending_records = stats.pending_records,
            pending_bytes = stats.pending_bytes,
            data_file_bytes = stats.data_file_bytes,
            max_bytes = stats.max_bytes,
            max_age_hours = stats.max_age_hours,
            utilization,
            "collector queue nearing max_bytes"
        );
    } else {
        info!(
            %collector_id,
            pending_records = stats.pending_records,
            pending_bytes = stats.pending_bytes,
            data_file_bytes = stats.data_file_bytes,
            max_bytes = stats.max_bytes,
            max_age_hours = stats.max_age_hours,
            "collector queue status"
        );
    }
}

fn now_unix_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}

#[derive(Default)]
struct CompressionStats {
    messages: u64,
    uncompressed_bytes: usize,
    framed_batch: Vec<u8>,
}

impl CompressionStats {
    fn observe(&mut self, collector_id: &str, update: &ProtoTelemetryIngestUpdate) {
        self.messages += 1;
        self.uncompressed_bytes += update.encoded_len();
        if let Err(error) = update.encode_length_delimited(&mut self.framed_batch) {
            warn!(%collector_id, %error, "failed to encode ingest update for compression estimate");
            return;
        }

        if self.messages >= COMPRESSION_STATS_INTERVAL {
            self.log_and_reset(collector_id);
        }
    }

    fn log_and_reset(&mut self, collector_id: &str) {
        if self.framed_batch.is_empty() {
            self.reset();
            return;
        }

        match zstd_size(&self.framed_batch) {
            Ok(compressed_bytes) if compressed_bytes > 0 => {
                let compression_ratio = self.uncompressed_bytes as f64 / compressed_bytes as f64;
                let reduction_percent =
                    100.0 * (1.0 - compressed_bytes as f64 / self.uncompressed_bytes as f64);
                info!(
                    %collector_id,
                    compression = "zstd",
                    messages = self.messages,
                    uncompressed_bytes = self.uncompressed_bytes,
                    estimated_compressed_bytes = compressed_bytes,
                    compression_ratio,
                    reduction_percent,
                    "collector ingest compression estimate"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(%collector_id, %error, "failed to estimate ingest zstd compression");
            }
        }
        self.reset();
    }

    fn reset(&mut self) {
        self.messages = 0;
        self.uncompressed_bytes = 0;
        self.framed_batch.clear();
    }
}

fn zstd_size(bytes: &[u8]) -> Result<usize> {
    Ok(zstd::bulk::compress(bytes, 3)
        .context("failed to estimate zstd-compressed ingest batch")?
        .len())
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use serde_json::json;

    use super::{
        CollectorQueue, RawQueuedRecord, client_tls_config, connect_core_channel,
        ingest_update_to_telemetry, telemetry_to_ingest_update, zstd_size,
    };
    use crate::config::{CollectorQueueConfig, RuntimeTlsConfig};
    use crate::telemetry::TelemetryUpdate;

    #[test]
    fn telemetry_ingest_proto_round_trips_msgpack_payload() {
        let update = TelemetryUpdate {
            target: "10.0.0.1:57400".to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "srl1".to_string(),
            timestamp_ns: 123,
            path: "interface[name=ethernet-1/1]/statistics".to_string(),
            value: json!({"in-packets": "42"}),
        };

        let proto = telemetry_to_ingest_update("collector-a", &update).unwrap();
        assert_eq!(proto.collector_id, "collector-a");
        assert!(!proto.value_msgpack.is_empty());

        let round_trip = ingest_update_to_telemetry(proto).unwrap();
        assert_eq!(round_trip.target, update.target);
        assert_eq!(round_trip.vendor, update.vendor);
        assert_eq!(round_trip.hostname, update.hostname);
        assert_eq!(round_trip.timestamp_ns, update.timestamp_ns);
        assert_eq!(round_trip.path, update.path);
        assert_eq!(round_trip.value, update.value);
    }

    #[test]
    fn telemetry_ingest_msgpack_counter_payload_is_smaller_than_json_baseline() {
        let mut msgpack_wire_bytes = 0;
        let mut json_wire_bytes = 0;
        let mut msgpack_payload_bytes = 0;
        let mut json_payload_bytes = 0;

        for index in 0_u64..1_000 {
            let update = TelemetryUpdate {
                target: "10.0.0.1:57400".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "srl1".to_string(),
                timestamp_ns: 1_777_777_000_000_000_000 + index as i64,
                path: "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets"
                    .to_string(),
                value: json!(1_234_567_890_123_456_789_u64 + index),
            };

            let proto = telemetry_to_ingest_update("collector-a", &update).unwrap();
            let json_payload = serde_json::to_vec(&update.value).unwrap();

            msgpack_wire_bytes += proto.encoded_len();
            msgpack_payload_bytes += proto.value_msgpack.len();
            json_payload_bytes += json_payload.len();

            let mut old_json_proto = proto.clone();
            old_json_proto.value_msgpack.clear();
            json_wire_bytes +=
                old_json_proto.encoded_len() + len_delimited_field_size(7, json_payload.len());
        }

        let payload_reduction = 1.0 - (msgpack_payload_bytes as f64 / json_payload_bytes as f64);
        assert!(
            payload_reduction >= 0.30,
            "expected MessagePack counter payloads to be at least 30% smaller; json={json_payload_bytes}, msgpack={msgpack_payload_bytes}, reduction={payload_reduction:.2}"
        );
        assert!(
            msgpack_wire_bytes < json_wire_bytes,
            "expected MessagePack ingest wire bytes to be smaller; json={json_wire_bytes}, msgpack={msgpack_wire_bytes}"
        );
    }

    #[test]
    fn telemetry_ingest_zstd_estimate_reduces_repetitive_batch() {
        let mut batch = Vec::new();
        let mut uncompressed_bytes = 0;

        for index in 0_i64..1_000 {
            let update = TelemetryUpdate {
                target: "10.0.0.1:57400".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "srl1".to_string(),
                timestamp_ns: 1_777_777_000_000_000_000 + index,
                path: "interface[name=ethernet-1/1]/statistics".to_string(),
                value: json!({
                    "in-octets": 1_234_567_890_i64 + index,
                    "out-octets": 9_876_543_210_i64 + index,
                    "in-packets": 44_000_i64 + index,
                    "out-packets": 55_000_i64 + index
                }),
            };
            let proto = telemetry_to_ingest_update("collector-a", &update).unwrap();
            uncompressed_bytes += proto.encoded_len();
            proto.encode_length_delimited(&mut batch).unwrap();
        }

        let compressed_bytes = zstd_size(&batch).unwrap();
        let reduction = 1.0 - (compressed_bytes as f64 / uncompressed_bytes as f64);
        assert!(
            reduction >= 0.40,
            "expected zstd to reduce repetitive ingest batch by at least 40%; raw={uncompressed_bytes}, zstd={compressed_bytes}, reduction={reduction:.2}"
        );
    }

    #[test]
    fn collector_queue_replays_after_restart_and_ack() {
        let tempdir = tempfile::tempdir().unwrap();
        let queue = CollectorQueue::open(queue_config(tempdir.path(), 10_000, 24)).unwrap();
        let first = sample_proto("collector-a", 1);
        let second = sample_proto("collector-a", 2);

        queue.append(first.clone(), "collector-a").unwrap();
        queue.append(second.clone(), "collector-a").unwrap();
        drop(queue);

        let reopened = CollectorQueue::open(queue_config(tempdir.path(), 10_000, 24)).unwrap();
        let batch = reopened.next_batch("collector-a").unwrap();

        assert_eq!(batch.records.len(), 2);
        assert_eq!(batch.records[0].update.timestamp_ns, first.timestamp_ns);
        assert_eq!(batch.records[1].update.timestamp_ns, second.timestamp_ns);

        reopened
            .ack(batch.records.last().unwrap().next_offset, "collector-a")
            .unwrap();
        assert_eq!(reopened.stats().unwrap().pending_records, 0);
    }

    #[test]
    fn collector_queue_retention_drops_expired_and_oversized_records() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut queue = CollectorQueue::open(queue_config(tempdir.path(), 180, 1)).unwrap();
        let old = sample_proto("collector-a", 1);
        let fresh_a = sample_proto("collector-a", 2);
        let fresh_b = sample_proto("collector-a", 3);

        {
            let inner = queue.inner.get_mut().unwrap();
            let old_payload = old.encode_to_vec();
            let fresh_a_payload = fresh_a.encode_to_vec();
            let fresh_b_payload = fresh_b.encode_to_vec();
            let very_old = super::now_unix_ns() - 2 * 3_600_000_000_000_i64;
            let now = super::now_unix_ns();
            inner
                .rewrite_records(&[
                    raw_record(old, old_payload, very_old),
                    raw_record(fresh_a.clone(), fresh_a_payload, now),
                    raw_record(fresh_b.clone(), fresh_b_payload, now),
                ])
                .unwrap();
        }

        let batch = queue.next_batch("collector-a").unwrap();

        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.records[0].update.timestamp_ns, fresh_b.timestamp_ns);
    }

    #[test]
    fn collector_tls_config_requires_ca_cert_and_identity() {
        let tls = RuntimeTlsConfig {
            enabled: true,
            ..Default::default()
        };
        let error = client_tls_config(&tls).unwrap_err().to_string();

        assert!(error.contains("runtime.tls.ca_cert"));
    }

    #[tokio::test]
    async fn collector_tls_rejects_http_endpoint_when_enabled() {
        let tls = RuntimeTlsConfig {
            enabled: true,
            ca_cert: Some("missing-ca.pem".to_string()),
            cert: Some("missing-cert.pem".to_string()),
            key: Some("missing-key.pem".to_string()),
            server_name: Some("bonsai-core.local".to_string()),
        };
        let error = connect_core_channel("http://127.0.0.1:50051", &tls)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("https://"));
    }

    fn len_delimited_field_size(field_number: u32, payload_len: usize) -> usize {
        varint_size(((field_number << 3) | 2) as u64)
            + varint_size(payload_len as u64)
            + payload_len
    }

    fn varint_size(mut value: u64) -> usize {
        let mut size = 1;
        while value >= 0x80 {
            value >>= 7;
            size += 1;
        }
        size
    }

    fn sample_proto(collector_id: &str, index: i64) -> crate::api::pb::TelemetryIngestUpdate {
        telemetry_to_ingest_update(
            collector_id,
            &TelemetryUpdate {
                target: "10.0.0.1:57400".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "srl1".to_string(),
                timestamp_ns: index,
                path: "interface[name=ethernet-1/1]/statistics".to_string(),
                value: json!({"in-packets": index}),
            },
        )
        .unwrap()
    }

    fn queue_config(
        path: &std::path::Path,
        max_bytes: u64,
        max_age_hours: u64,
    ) -> CollectorQueueConfig {
        CollectorQueueConfig {
            path: path.to_string_lossy().into_owned(),
            max_bytes,
            max_age_hours,
            drain_batch_size: 1_000,
            log_interval_seconds: 0,
        }
    }

    fn raw_record(
        update: crate::api::pb::TelemetryIngestUpdate,
        payload: Vec<u8>,
        enqueued_at_ns: i64,
    ) -> RawQueuedRecord {
        RawQueuedRecord {
            enqueued_at_ns,
            payload,
            update,
            next_offset: 0,
        }
    }
}

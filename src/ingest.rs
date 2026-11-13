use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use lru::LruCache;
use prost::Message;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tonic::Request;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tracing::{info, warn};

use crate::api::pb::{
    AssignmentUpdate, CollectorIdentity, CollectorStats, InterfaceSummary,
    TelemetryIngestUpdate as ProtoTelemetryIngestUpdate, bonsai_graph_client::BonsaiGraphClient,
};
use crate::config::{
    BackpressureConfig, CollectorConfig, CollectorFilterConfig, CollectorQueueConfig,
    RuntimeConfig, RuntimeTlsConfig,
};
use crate::counter_summarizer::CounterSummarizer;
use crate::event_bus::InProcessBus;
use crate::subscription_status::SubscriptionPlan;
use crate::telemetry::{TelemetryEvent, TelemetryUpdate};

// ── Sharded LRU cache (C-6/C-7) ──────────────────────────────────────────────

const CACHE_SHARDS: usize = 16;

/// 16-shard LRU cache that spreads Mutex contention across shards by key hash.
/// Each shard holds an independent LruCache with a per-shard capacity.
struct ShardedLruCache<V> {
    shards: Vec<Mutex<LruCache<String, V>>>,
}

impl<V> ShardedLruCache<V> {
    fn new(per_shard_cap: usize) -> Self {
        let cap = NonZeroUsize::new(per_shard_cap.max(64)).unwrap();
        let shards = (0..CACHE_SHARDS)
            .map(|_| Mutex::new(LruCache::new(cap)))
            .collect();
        Self { shards }
    }

    fn shard_idx(&self, key: &str) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        (h.finish() as usize) % CACHE_SHARDS
    }

    /// Shrink each shard's capacity to `pct` percent of its current capacity,
    /// evicting the LRU entries. Called by the resource governor under memory pressure.
    fn shrink_to_pct(&self, pct: usize) {
        let pct = pct.clamp(10, 90);
        for shard in &self.shards {
            let mut cache = shard.lock().unwrap();
            let new_cap = ((cache.cap().get() * pct) / 100).max(64);
            cache.resize(NonZeroUsize::new(new_cap).unwrap());
        }
    }

    /// Atomic check-then-update within a single shard lock.
    /// Returns `true` if `skip_if(existing)` held — caller should drop the update.
    /// Returns `false` and inserts `new_value` if the predicate did not hold or the key was absent.
    fn check_debounce(&self, key: &str, new_value: V, skip_if: impl Fn(&V) -> bool) -> bool {
        let idx = self.shard_idx(key);
        let mut cache = self.shards[idx].lock().unwrap();
        if let Some(existing) = cache.peek(key)
            && skip_if(existing)
        {
            return true;
        }
        cache.put(key.to_string(), new_value);
        false
    }
}

/// Compute a per-shard LRU capacity from a byte budget.
/// `entry_bytes` is the estimated memory cost of one entry including key String heap + value + node overhead.
fn shard_cap_from_budget(total_budget_bytes: usize, entry_bytes: usize) -> usize {
    let total_entries = total_budget_bytes / entry_bytes.max(1);
    (total_entries / CACHE_SHARDS).max(64)
}

// ── TelemetryDebouncer ────────────────────────────────────────────────────────

pub struct TelemetryDebouncer {
    last_counter_write: ShardedLruCache<Instant>,
    debounce_window: Duration,
    queue_depth: Arc<AtomicUsize>,
    queue_capacity: usize,
    backpressure: BackpressureConfig,
    last_oper_status_write: ShardedLruCache<Instant>,
    last_state_write: ShardedLruCache<(Instant, String)>,
    state_debounce_window: Duration,
}

impl TelemetryDebouncer {
    pub fn new(
        debounce_secs: u64,
        queue_depth: Arc<AtomicUsize>,
        backpressure: BackpressureConfig,
        queue_capacity: usize,
        debounce_memory_bytes: usize,
    ) -> Self {
        let state_debounce_secs = (debounce_secs / 2).clamp(10, 30);

        // Divide the RAM budget equally across three caches; estimate bytes per entry:
        //   counter/oper_status: String key ~48b + Instant 16b + LRU node ~64b = ~128b
        //   state: String key ~48b + (Instant 16b + String value ~48b) + LRU node ~64b = ~256b
        let budget_per_cache = debounce_memory_bytes / 3;
        let counter_shard_cap = shard_cap_from_budget(budget_per_cache, 128);
        let oper_shard_cap = shard_cap_from_budget(budget_per_cache, 128);
        let state_shard_cap = shard_cap_from_budget(budget_per_cache, 256);

        tracing::debug!(
            counter_total = counter_shard_cap * CACHE_SHARDS,
            oper_status_total = oper_shard_cap * CACHE_SHARDS,
            state_total = state_shard_cap * CACHE_SHARDS,
            shards = CACHE_SHARDS,
            budget_mb = debounce_memory_bytes / 1024 / 1024,
            "ingest debounce cache sizing"
        );

        Self {
            last_counter_write: ShardedLruCache::new(counter_shard_cap),
            debounce_window: Duration::from_secs(debounce_secs),
            queue_depth,
            queue_capacity,
            backpressure,
            last_oper_status_write: ShardedLruCache::new(oper_shard_cap),
            last_state_write: ShardedLruCache::new(state_shard_cap),
            state_debounce_window: Duration::from_secs(state_debounce_secs),
        }
    }

    /// Shrink all three debounce caches to `pct` percent of current capacity,
    /// evicting LRU entries. Called by the resource governor under soft/hard memory pressure.
    /// Soft pressure: shrink to 50%. Hard pressure: shrink to 25%.
    pub fn shrink_debounce_caches(&self, pct: usize) {
        self.last_counter_write.shrink_to_pct(pct);
        self.last_oper_status_write.shrink_to_pct(pct);
        self.last_state_write.shrink_to_pct(pct);
        tracing::info!(pct, "ingest: debounce caches shrunk under memory pressure");
        metrics::counter!(
            "bonsai_governance_action_total",
            "action" => "debounce_cache_shrink",
            "reason" => "memory_pressure",
        )
        .increment(1);
    }

    pub fn should_drop(&self, update: &TelemetryUpdate) -> bool {
        // T1-1: Structural pre-validation — drop updates with empty target or path.
        if update.target.is_empty() || update.path.is_empty() {
            metrics::counter!(
                "bonsai_ingest_validation_drops_total",
                "reason" => "empty_target_or_path"
            )
            .increment(1);
            return true;
        }

        let depth = self.queue_depth.load(Ordering::Relaxed);
        let fill_pct = if self.queue_capacity > 0 {
            (depth as u64 * 100) / (self.queue_capacity as u64)
        } else {
            0
        };

        let classified = update.classify();

        // Value-aware debounce: drop non-counter updates whose value hasn't changed within the window.
        // State changes (different value) always pass through immediately.
        match &classified {
            TelemetryEvent::InterfaceStats { .. } | TelemetryEvent::Ignored => {}
            _ => {
                let key = format!("{}/{}", update.target, update.path);
                let value_str = update.value.to_string();
                let now = Instant::now();
                let window = self.state_debounce_window;
                let dropped = self.last_state_write.check_debounce(
                    &key,
                    (now, value_str.clone()),
                    |(last_time, last_value)| {
                        *last_value == value_str && now.duration_since(*last_time) < window
                    },
                );
                if dropped {
                    metrics::counter!(
                        "bonsai_ingest_debounce_drops_total",
                        "reason" => "topology_unchanged"
                    )
                    .increment(1);
                    return true;
                }
            }
        }

        match &classified {
            TelemetryEvent::InterfaceStats { if_name } => {
                if fill_pct >= self.backpressure.level_1_pct {
                    metrics::counter!("bonsai_ingest_backpressure_drops_total", "reason" => "level_1_counter").increment(1);
                    return true;
                }

                if !self.debounce_window.is_zero() {
                    let key = format!("{}:{}", update.target, if_name);
                    let now = Instant::now();
                    let window = self.debounce_window;
                    if self
                        .last_counter_write
                        .check_debounce(&key, now, |t| now.duration_since(*t) < window)
                    {
                        return true;
                    }
                }
            }
            TelemetryEvent::InterfaceOperStatus { if_name, .. }
                if fill_pct >= self.backpressure.level_2_pct =>
            {
                let key = format!("{}:{}", update.target, if_name);
                let now = Instant::now();
                if self.last_oper_status_write.check_debounce(&key, now, |t| {
                    now.duration_since(*t) < Duration::from_secs(6)
                }) {
                    metrics::counter!("bonsai_ingest_backpressure_drops_total", "reason" => "level_2_oper_status").increment(1);
                    return true;
                }
            }
            _ => {}
        }

        false
    }
}

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
        role: update.role.clone(),
        site: update.site.clone(),
        timestamp_ns: update.timestamp_ns,
        path: update.path.clone(),
        value_msgpack: rmp_serde::to_vec(&update.value)
            .context("failed to serialize telemetry value as MessagePack")?,
        protocol_version: crate::api::PROTOCOL_VERSION,
        interface_summary: None,
    })
}

pub fn summary_to_ingest_update(
    collector_id: &str,
    template: Option<&TelemetryUpdate>,
    summary: InterfaceSummary,
) -> Result<ProtoTelemetryIngestUpdate> {
    // target and if_name are already on the summary; use template metadata when available.
    Ok(ProtoTelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: template.map_or_else(|| summary.target.clone(), |t| t.target.clone()),
        vendor: template.map_or_else(String::new, |t| t.vendor.clone()),
        hostname: template.map_or_else(String::new, |t| t.hostname.clone()),
        role: template.map_or_else(String::new, |t| t.role.clone()),
        site: template.map_or_else(String::new, |t| t.site.clone()),
        timestamp_ns: template.map_or(0, |t| t.timestamp_ns),
        path: template.map_or_else(String::new, |t| t.path.clone()),
        value_msgpack: Vec::new(),
        protocol_version: crate::api::PROTOCOL_VERSION,
        interface_summary: Some(summary),
    })
}

pub fn ingest_update_to_telemetry(update: ProtoTelemetryIngestUpdate) -> Result<TelemetryUpdate> {
    if let Some(summary) = update.interface_summary {
        return Ok(TelemetryUpdate {
            target: update.target,
            vendor: update.vendor,
            hostname: update.hostname,
            role: update.role,
            site: update.site,
            timestamp_ns: update.timestamp_ns,
            path: format!("{}/summary", update.path),
            value: interface_summary_to_json(summary),
        });
    }

    let value = rmp_serde::from_slice(&update.value_msgpack)
        .with_context(|| format!("invalid telemetry value_msgpack for path '{}'", update.path))?;

    Ok(TelemetryUpdate {
        target: update.target,
        vendor: update.vendor,
        hostname: update.hostname,
        role: update.role,
        site: update.site,
        timestamp_ns: update.timestamp_ns,
        path: update.path,
        value,
    })
}

fn interface_summary_to_json(summary: InterfaceSummary) -> serde_json::Value {
    let mut counters = serde_json::Map::new();
    for c in summary.counters {
        counters.insert(
            c.counter_name,
            serde_json::json!({
                "min": c.min,
                "max": c.max,
                "mean": c.mean,
                "delta": c.delta,
            }),
        );
    }
    serde_json::json!({
        "interface_summary": {
            "window_secs": summary.window_secs,
            "counters": counters,
        }
    })
}

pub async fn run_core_forwarder(
    bus: Arc<InProcessBus>,
    core_endpoint: String,
    collector_id: String,
    collector_config: CollectorConfig,
    tls_config: RuntimeTlsConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
    queue_depth_counter: Arc<AtomicU64>,
    queue_bytes_counter: Arc<AtomicU64>,
    queue_max_bytes_counter: Arc<AtomicU64>,
    diag_state: Option<crate::collector::diagnostic_server::DiagnosticState>,
) {
    let queue = match CollectorQueue::open(collector_config.queue.clone()) {
        Ok(queue) => Arc::new(queue),
        Err(error) => {
            warn!(%collector_id, %error, "failed to open collector disk queue; forwarder disabled");
            return;
        }
    };
    let writer_collector_id = collector_id.clone();
    let writer_queue = Arc::clone(&queue);
    let writer_shutdown = shutdown.clone();
    let filter_config = collector_config.filter.clone();
    let queue_writer = tokio::spawn(async move {
        if let Err(error) = queue_bus_updates(
            bus,
            writer_collector_id.clone(),
            writer_queue,
            filter_config,
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
        queue_depth_counter,
        queue_bytes_counter,
        queue_max_bytes_counter,
        diag_state,
    ));
    let mut shutdown = shutdown;

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

        // H2/R3 acknowledgement contract: the core consumes the ENTIRE
        // client stream before responding — every record is either
        // `accepted` (enqueued to the event bus) or `rejected`
        // (permanently refused: authorization denial, malformed
        // payload, protocol skew). A received response therefore means
        // the whole batch was processed — but ONLY an exactly
        // consistent accounting (`accepted + rejected == submitted`,
        // checked arithmetic) proves it. An inconsistent or impossible
        // response is a protocol violation: bail WITHOUT acking so the
        // batch is retained and replayed — a loud failure, never
        // silent record loss.
        //
        // Compatibility: pre-`rejected` cores report `accepted` only,
        // but their accepted count already equals the submitted count
        // (they had no permanent per-record rejections), so the strict
        // equality still holds for old cores. A response whose
        // protocol_version is major-skewed cannot be trusted to honor
        // the contract at all — also retained.
        match crate::api::check_protocol_compat(
            response.protocol_version,
            crate::api::PROTOCOL_VERSION_CURRENT,
        ) {
            crate::api::VersionCompat::Compatible => {}
            crate::api::VersionCompat::MinorSkew { client, server: _ } => {
                warn!(
                    %collector_id,
                    core_version = client,
                    collector_version = crate::api::PROTOCOL_VERSION_CURRENT,
                    "core ingest response protocol version minor skew"
                );
            }
            crate::api::VersionCompat::MajorSkew { client, server } => {
                anyhow::bail!(
                    "core ingest response protocol version major skew \
                     (core={client} collector={server}); batch retained for replay"
                );
            }
        }
        let accepted = response.accepted;
        let rejected = response.rejected;
        let batch_len = u64::try_from(batch.records.len())
            .context("collector queue batch length exceeds u64")?;
        let accounted = accepted.checked_add(rejected);
        if accounted != Some(batch_len) {
            anyhow::bail!(
                "core ingest accounting mismatch: accepted={accepted} \
                 rejected={rejected} submitted={batch_len}; batch retained for replay"
            );
        }
        let accepted = accepted as usize;
        let rejected = rejected as usize;
        let ack_offset = batch.records[batch.records.len() - 1].next_offset;
        queue
            .ack(ack_offset, collector_id)
            .context("failed to ack collector queue records")?;

        metrics::counter!("bonsai_queue_drained_total", "collector_id" => collector_id.to_string())
            .increment(accepted as u64);
        if rejected > 0 {
            metrics::counter!("bonsai_queue_rejected_total", "collector_id" => collector_id.to_string())
                .increment(rejected as u64);
        }

        info!(
            accepted = response.accepted,
            rejected = response.rejected,
            queued_remaining = queue
                .stats()
                .map(|stats| stats.pending_records)
                .unwrap_or(0),
            "collector queue batch delivered"
        );
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
    filter_config: CollectorFilterConfig,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let (sub, mut rx) = crate::event_bus::MpscSubscriber::new(
        "collector_forwarder",
        4096,
        crate::event_bus::OverflowPolicy::DropNewest,
    );
    bus.add_subscriber(sub).await;

    let mode = filter_config.counter_forward_mode.to_lowercase();

    if mode == "summary" {
        queue_bus_updates_summary(&mut rx, &collector_id, queue, filter_config, &mut shutdown).await
    } else {
        queue_bus_updates_debounced(&mut rx, &collector_id, queue, filter_config, &mut shutdown)
            .await
    }
}

async fn queue_bus_updates_debounced(
    rx: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<TelemetryUpdate>>,
    collector_id: &str,
    queue: Arc<CollectorQueue>,
    filter_config: CollectorFilterConfig,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut last_forward: HashMap<String, Instant> = HashMap::new();
    let debounce_window = Duration::from_secs(filter_config.counter_debounce_secs);
    let mode = filter_config.counter_forward_mode.to_lowercase();

    loop {
        let update = tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            received = rx.recv() => received,
        };

        match update {
            Some(update) => {
                if mode != "raw" {
                    let classified = update.classify();
                    if let TelemetryEvent::InterfaceStats { if_name } = classified {
                        let key = format!("{}:{}", update.target, if_name);
                        let now = Instant::now();
                        if let Some(last) = last_forward.get(&key)
                            && now.duration_since(*last) < debounce_window
                        {
                            continue;
                        }
                        last_forward.insert(key, now);
                    }
                }
                let proto = telemetry_to_ingest_update(collector_id, &update)?;
                queue.append(proto, collector_id)?;
            }
            None => return Ok(()),
        }
    }
}

async fn queue_bus_updates_summary(
    rx: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<TelemetryUpdate>>,
    collector_id: &str,
    queue: Arc<CollectorQueue>,
    filter_config: CollectorFilterConfig,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut summarizer = CounterSummarizer::new(filter_config.counter_window_secs);
    let flush_idle_secs = filter_config.counter_flush_idle_secs;
    // Timer fires slightly more often than the idle threshold to catch stale windows promptly.
    let flush_check_interval = Duration::from_secs(flush_idle_secs.max(10) / 2);
    let mut flush_timer = tokio::time::interval(flush_check_interval);

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = flush_timer.tick() => {
                for summary in summarizer.flush_stale(flush_idle_secs) {
                    let proto = summary_to_ingest_update(collector_id, None, summary)?;
                    queue.append(proto, collector_id)?;
                    metrics::counter!("bonsai_summaries_emitted_total", "collector_id" => collector_id.to_string()).increment(1);
                }
            }
            received = rx.recv() => {
                match received {
                    Some(update) => {
                        let classified = update.classify();
                        if let TelemetryEvent::InterfaceStats { .. } = classified {
                            // Counter update: feed the summarizer.
                            if let Some(summary) = summarizer.observe(&update) {
                                let proto = summary_to_ingest_update(collector_id, Some(&update), summary)?;
                                queue.append(proto, collector_id)?;
                                metrics::counter!("bonsai_summaries_emitted_total", "collector_id" => collector_id.to_string()).increment(1);
                            }
                            // Raw counter update is intentionally dropped — the summary carries the data.
                        } else {
                            // Non-counter event (state change, BGP, etc.): forward raw.
                            let proto = telemetry_to_ingest_update(collector_id, &update)?;
                            queue.append(proto, collector_id)?;
                        }
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

async fn log_queue_status(
    collector_id: String,
    queue: Arc<CollectorQueue>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    queue_depth_counter: Arc<AtomicU64>,
    queue_bytes_counter: Arc<AtomicU64>,
    queue_max_bytes_counter: Arc<AtomicU64>,
    diag_state: Option<crate::collector::diagnostic_server::DiagnosticState>,
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
                    Ok(stats) => {
                        log_queue_stats(&collector_id, &stats);
                        queue_depth_counter.store(stats.pending_records, Ordering::Relaxed);
                        queue_bytes_counter.store(stats.data_file_bytes, Ordering::Relaxed);
                        queue_max_bytes_counter.store(stats.max_bytes, Ordering::Relaxed);
                        if let Some(ref ds) = diag_state {
                            ds.update_queue_depth(stats.pending_records);
                        }
                    }
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
    metrics::gauge!("bonsai_collector_queue_depth", "collector_id" => collector_id.to_string())
        .set(stats.pending_records as f64);

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

/// Reads the resident set size of the current process in bytes.
/// Uses /proc/self/status on Linux. Returns 0 on unsupported platforms.
fn read_process_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    let kb: u64 = line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    return kb * 1024;
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
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
        create_ingest_client, drain_queue_to_core, ingest_update_to_telemetry,
        telemetry_to_ingest_update, zstd_size,
    };
    use crate::config::{CollectorQueueConfig, RuntimeConfig, RuntimeTlsConfig};
    use crate::event_bus::InProcessBus;
    use crate::telemetry::TelemetryUpdate;
    use std::time::Duration;

    #[test]
    fn telemetry_ingest_proto_round_trips_msgpack_payload() {
        let update = TelemetryUpdate {
            target: "10.0.0.1:57400".to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "srl1".to_string(),
            role: String::new(),
            site: String::new(),
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
                role: String::new(),
                site: String::new(),
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
                role: String::new(),
                site: String::new(),
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
                role: String::new(),
                site: String::new(),
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

    // ── H2: drain_queue_to_core acknowledgement contract ──────────────────
    //
    // The core consumes the ENTIRE client stream before responding, so a
    // received TelemetryIngestResponse means every record in the batch
    // was processed — `accepted` and `rejected` are truthful counts, and
    // the queue may retire the whole batch. These tests run the REAL
    // forwarder against a real (development-compat) core server; only
    // undecodable payloads are rejected there.

    struct DevCore {
        addr: std::net::SocketAddr,
        _dir: tempfile::TempDir,
        _server: tokio::task::JoinHandle<()>,
        // Keeps the never-drained subscriber channel alive for the
        // bus-saturation test; dropping it would close the channel and
        // unblock the router.
        _blocked_rx:
            Option<tokio::sync::mpsc::Receiver<std::sync::Arc<crate::telemetry::TelemetryUpdate>>>,
    }

    async fn start_dev_core() -> DevCore {
        start_dev_core_mode(None, false).await
    }

    /// `bus` injects a custom event bus (e.g. a tiny router with a
    /// permanently-blocked subscriber to force router-queue overload);
    /// `saturate` registers a BlockProducer subscriber that is never
    /// drained.
    async fn start_dev_core_mode(
        bus: Option<std::sync::Arc<InProcessBus>>,
        saturate: bool,
    ) -> DevCore {
        use crate::api::{BonsaiGraphServer, CoreService};
        use crate::assignment::CollectorManager;
        use crate::collector_auth::AuthPolicy;
        use crate::credentials::CredentialVault;
        use crate::event_bus::{InProcessBus, MpscSubscriber, OverflowPolicy};
        use crate::graph::GraphStore;
        use crate::registry::ApiRegistry;
        use crate::sidecar_registry::SidecarRegistry;
        use tokio::net::TcpListener;
        use tokio_stream::wrappers::TcpListenerStream;
        use tonic::transport::Server;

        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("BONSAI_DRAIN_TEST_VAULT_PASSPHRASE", "pw") };
        let graph = std::sync::Arc::new(
            GraphStore::open(
                dir.path().join("graph.db").to_str().unwrap(),
                256 * 1024 * 1024,
            )
            .unwrap(),
        );
        let registry = std::sync::Arc::new(
            ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap(),
        );
        let credentials = std::sync::Arc::new(
            CredentialVault::open(
                dir.path().join("vault"),
                "BONSAI_DRAIN_TEST_VAULT_PASSPHRASE",
            )
            .unwrap(),
        );
        let bus = bus.unwrap_or_else(|| InProcessBus::new(1024));
        let mut blocked_rx = None;
        if saturate {
            // A subscriber whose queue is never drained: the router
            // blocks on it once its 1-slot queue fills, so the bounded
            // router channel backs up and publish() reports overload.
            let (sub, rx) = MpscSubscriber::new("blocked", 1, OverflowPolicy::BlockProducer);
            bus.add_subscriber(sub).await;
            blocked_rx = Some(rx);
        }
        let manager = std::sync::Arc::new(CollectorManager::new(
            std::sync::Arc::clone(&registry),
            std::sync::Arc::clone(&credentials),
            vec![],
        ));
        let sidecars = std::sync::Arc::new(SidecarRegistry::new(vec![]));
        let svc = CoreService::new(
            graph,
            std::sync::Arc::clone(&registry),
            credentials,
            bus,
            None,
            Some(manager),
            sidecars,
            registry.sqlite_store(),
            None,
            None,
        )
        .with_auth_policy(AuthPolicy::DevelopmentCompat);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(BonsaiGraphServer::new(svc))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        DevCore {
            addr,
            _dir: dir,
            _server: server,
            _blocked_rx: blocked_rx,
        }
    }

    fn drain_runtime(addr: std::net::SocketAddr) -> RuntimeConfig {
        let mut cfg = RuntimeConfig::default();
        cfg.core_ingest_endpoint = format!("http://{addr}");
        cfg.tls.enabled = false;
        cfg
    }

    /// A permanently-rejected record: msgpack byte 0xc1 is reserved and
    /// fails decode on the core — the record is retired, never retried.
    fn poison_proto(collector_id: &str, index: i64) -> crate::api::pb::TelemetryIngestUpdate {
        let mut u = sample_proto(collector_id, index);
        u.value_msgpack = vec![0xc1];
        u
    }

    async fn wait_queue_drained(queue: &CollectorQueue) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if queue.stats().unwrap().pending_records == 0 {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "queue did not drain: {:?}",
                queue.stats().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn drain_acks_whole_batch_with_interleaved_rejections() {
        let core = start_dev_core().await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        // Interleaved accepted/rejected records in ONE batch: the core
        // consumes the entire stream before responding, so the whole
        // batch — including the rejected middle record — is processed
        // and retired exactly once.
        for (i, u) in [
            sample_proto("collector-a", 1),
            poison_proto("collector-a", 2),
            sample_proto("collector-a", 3),
            poison_proto("collector-a", 4),
            sample_proto("collector-a", 5),
        ]
        .into_iter()
        .enumerate()
        {
            let _ = i;
            queue.append(u, "collector-a").unwrap();
        }

        let mut client = create_ingest_client(&drain_runtime(core.addr))
            .await
            .unwrap();
        let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let q = std::sync::Arc::clone(&queue);
        let drain =
            tokio::spawn(
                async move { drain_queue_to_core(&mut client, q, "collector-a", sd_rx).await },
            );
        wait_queue_drained(&queue).await;
        sd_tx.send(true).unwrap();
        drain.await.unwrap().unwrap();
        assert_eq!(queue.stats().unwrap().pending_records, 0);
    }

    #[tokio::test]
    async fn drain_retires_fully_rejected_batch_without_blocking() {
        let core = start_dev_core().await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        // Entire batch permanently rejected (all poison): retired, not
        // replayed forever — and the NEXT legitimate batch still flows.
        queue
            .append(poison_proto("collector-a", 1), "collector-a")
            .unwrap();
        queue
            .append(poison_proto("collector-a", 2), "collector-a")
            .unwrap();
        queue
            .append(sample_proto("collector-a", 3), "collector-a")
            .unwrap();

        let mut client = create_ingest_client(&drain_runtime(core.addr))
            .await
            .unwrap();
        let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let q = std::sync::Arc::clone(&queue);
        let drain =
            tokio::spawn(
                async move { drain_queue_to_core(&mut client, q, "collector-a", sd_rx).await },
            );
        wait_queue_drained(&queue).await;
        sd_tx.send(true).unwrap();
        drain.await.unwrap().unwrap();
        assert_eq!(queue.stats().unwrap().pending_records, 0);
    }

    #[tokio::test]
    async fn drain_retains_records_on_retryable_core_failure() {
        let core = start_dev_core().await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        queue
            .append(sample_proto("collector-a", 1), "collector-a")
            .unwrap();
        queue
            .append(sample_proto("collector-a", 2), "collector-a")
            .unwrap();

        let mut client = create_ingest_client(&drain_runtime(core.addr))
            .await
            .unwrap();
        // Core dies mid-operation: the RPC fails BEFORE a response, so
        // no acknowledgement is written and both records are retained
        // for replay (at-least-once).
        core._server.abort();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (_sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let q = std::sync::Arc::clone(&queue);
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            drain_queue_to_core(&mut client, q, "collector-a", sd_rx),
        )
        .await
        .expect("drain must return on core failure, not hang");
        assert!(result.is_err(), "core outage must surface as drain error");
        assert_eq!(
            queue.stats().unwrap().pending_records,
            2,
            "unacknowledged records must survive a failed drain for replay"
        );
    }

    // ── R3: scripted-core drain contract tests ──────────────────────────
    //
    // A minimal BonsaiGraph stub that consumes the whole client stream
    // then returns a SCRIPTED TelemetryIngestResponse — used to prove
    // the collector refuses to acknowledge inconsistent, impossible or
    // protocol-incompatible accounting.

    struct ScriptedCore {
        accepted: u64,
        rejected: u64,
        protocol_version: u32,
        error: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<crate::api::pb::TelemetryIngestUpdate>>>,
    }

    #[tonic::async_trait]
    impl crate::api::BonsaiGraph for ScriptedCore {
        type RegisterCollectorStream = tokio_stream::wrappers::ReceiverStream<
            std::result::Result<crate::api::pb::AssignmentUpdate, tonic::Status>,
        >;
        type StreamEventsStream = tokio_stream::wrappers::ReceiverStream<
            std::result::Result<crate::api::pb::StateEvent, tonic::Status>,
        >;

        async fn telemetry_ingest(
            &self,
            request: tonic::Request<tonic::Streaming<crate::api::pb::TelemetryIngestUpdate>>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::TelemetryIngestResponse>,
            tonic::Status,
        > {
            let mut stream = request.into_inner();
            while let Some(update) = stream
                .message()
                .await
                .map_err(|e| tonic::Status::unavailable(e.to_string()))?
            {
                self.seen.lock().unwrap().push(update);
            }
            if !self.error.is_empty() {
                return Err(tonic::Status::unavailable(&self.error));
            }
            Ok(tonic::Response::new(
                crate::api::pb::TelemetryIngestResponse {
                    accepted: self.accepted,
                    error: String::new(),
                    protocol_version: self.protocol_version,
                    rejected: self.rejected,
                },
            ))
        }

        // All RPCs other than telemetry_ingest are unused by the drain
        // path — written out explicitly because #[async_trait] cannot
        // desugar async fns emitted by an inner macro_rules! call.
        async fn get_leader(
            &self,
            _r: tonic::Request<crate::api::pb::GetLeaderRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::GetLeaderResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("get_leader"))
        }
        async fn register_collector(
            &self,
            _r: tonic::Request<crate::api::pb::CollectorIdentity>,
        ) -> std::result::Result<tonic::Response<Self::RegisterCollectorStream>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("register_collector"))
        }
        async fn heartbeat(
            &self,
            _r: tonic::Request<crate::api::pb::CollectorStats>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::HeartbeatAck>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("heartbeat"))
        }
        async fn query(
            &self,
            _r: tonic::Request<crate::api::pb::QueryRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::QueryResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("query"))
        }
        async fn get_devices(
            &self,
            _r: tonic::Request<crate::api::pb::GetDevicesRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::GetDevicesResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("get_devices"))
        }
        async fn get_interfaces(
            &self,
            _r: tonic::Request<crate::api::pb::GetInterfacesRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::GetInterfacesResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("get_interfaces"))
        }
        async fn get_bgp_neighbors(
            &self,
            _r: tonic::Request<crate::api::pb::GetBgpNeighborsRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::GetBgpNeighborsResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("get_bgp_neighbors"))
        }
        async fn get_topology(
            &self,
            _r: tonic::Request<crate::api::pb::GetTopologyRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::GetTopologyResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("get_topology"))
        }
        async fn list_managed_devices(
            &self,
            _r: tonic::Request<crate::api::pb::ListManagedDevicesRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::ListManagedDevicesResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("list_managed_devices"))
        }
        async fn add_device(
            &self,
            _r: tonic::Request<crate::api::pb::AddDeviceRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::DeviceMutationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("add_device"))
        }
        async fn update_device(
            &self,
            _r: tonic::Request<crate::api::pb::UpdateDeviceRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::DeviceMutationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("update_device"))
        }
        async fn remove_device(
            &self,
            _r: tonic::Request<crate::api::pb::RemoveDeviceRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::DeviceMutationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("remove_device"))
        }
        async fn discover_device(
            &self,
            _r: tonic::Request<crate::api::pb::DiscoverRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::DiscoveryReport>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("discover_device"))
        }
        async fn list_sites(
            &self,
            _r: tonic::Request<crate::api::pb::ListSitesRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::ListSitesResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("list_sites"))
        }
        async fn add_site(
            &self,
            _r: tonic::Request<crate::api::pb::AddSiteRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::SiteMutationResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("add_site"))
        }
        async fn update_site(
            &self,
            _r: tonic::Request<crate::api::pb::UpdateSiteRequest>,
        ) -> std::result::Result<tonic::Response<crate::api::pb::SiteMutationResponse>, tonic::Status>
        {
            Err(tonic::Status::unimplemented("update_site"))
        }
        async fn list_credentials(
            &self,
            _r: tonic::Request<crate::api::pb::ListCredentialsRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::ListCredentialsResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("list_credentials"))
        }
        async fn add_credential(
            &self,
            _r: tonic::Request<crate::api::pb::AddCredentialRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::CredentialMutationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("add_credential"))
        }
        async fn remove_credential(
            &self,
            _r: tonic::Request<crate::api::pb::RemoveCredentialRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::CredentialMutationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("remove_credential"))
        }
        async fn stream_events(
            &self,
            _r: tonic::Request<crate::api::pb::StreamEventsRequest>,
        ) -> std::result::Result<tonic::Response<Self::StreamEventsStream>, tonic::Status> {
            Err(tonic::Status::unimplemented("stream_events"))
        }
        async fn detection_ingest(
            &self,
            _r: tonic::Request<tonic::Streaming<crate::api::pb::DetectionEventIngest>>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::DetectionIngestResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("detection_ingest"))
        }
        async fn create_detection(
            &self,
            _r: tonic::Request<crate::api::pb::CreateDetectionRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::CreateDetectionResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("create_detection"))
        }
        async fn create_remediation(
            &self,
            _r: tonic::Request<crate::api::pb::CreateRemediationRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::CreateRemediationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("create_remediation"))
        }
        async fn push_remediation(
            &self,
            _r: tonic::Request<crate::api::pb::PushRemediationRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::PushRemediationResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("push_remediation"))
        }
        async fn register_sidecar(
            &self,
            _r: tonic::Request<crate::api::pb::RegisterSidecarRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::RegisterSidecarResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("register_sidecar"))
        }
        async fn sidecar_heartbeat(
            &self,
            _r: tonic::Request<crate::api::pb::SidecarHeartbeatRequest>,
        ) -> std::result::Result<
            tonic::Response<crate::api::pb::SidecarHeartbeatResponse>,
            tonic::Status,
        > {
            Err(tonic::Status::unimplemented("sidecar_heartbeat"))
        }
    }

    struct ScriptedCoreHandle {
        addr: std::net::SocketAddr,
        seen: std::sync::Arc<std::sync::Mutex<Vec<crate::api::pb::TelemetryIngestUpdate>>>,
        _server: tokio::task::JoinHandle<()>,
    }

    async fn start_scripted_core(
        accepted: u64,
        rejected: u64,
        protocol_version: u32,
    ) -> ScriptedCoreHandle {
        use crate::api::BonsaiGraphServer;
        use tokio::net::TcpListener;
        use tokio_stream::wrappers::TcpListenerStream;
        use tonic::transport::Server;

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let svc = ScriptedCore {
            accepted,
            rejected,
            protocol_version,
            error: String::new(),
            seen: std::sync::Arc::clone(&seen),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(BonsaiGraphServer::new(svc))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        ScriptedCoreHandle {
            addr,
            seen,
            _server: server,
        }
    }

    /// Run one drain session against `addr` until the queue empties,
    /// the drain task exits (error), or the deadline passes — then
    /// signal shutdown and return the drain's own result.
    async fn run_drain_once(
        addr: std::net::SocketAddr,
        queue: &std::sync::Arc<CollectorQueue>,
    ) -> Result<(), anyhow::Error> {
        let mut client = create_ingest_client(&drain_runtime(addr)).await?;
        let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let q = std::sync::Arc::clone(queue);
        let drain =
            tokio::spawn(
                async move { drain_queue_to_core(&mut client, q, "collector-a", sd_rx).await },
            );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if drain.is_finished() || queue.stats().map(|s| s.pending_records).unwrap_or(0) == 0 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "drain did not settle: {:?}",
                queue.stats()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = sd_tx.send(true);
        drain
            .await
            .map_err(|e| anyhow::anyhow!("drain task panicked: {e}"))?
    }

    #[tokio::test]
    async fn drain_acks_consistent_scripted_batch() {
        // Old-core shape: accepted-only accounting where accepted ==
        // submitted (pre-rejected cores never permanently rejected).
        let core = start_scripted_core(3, 0, crate::api::PROTOCOL_VERSION_CURRENT).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        for i in 0..3 {
            queue
                .append(sample_proto("collector-a", i), "collector-a")
                .unwrap();
        }
        run_drain_once(core.addr, &queue).await.unwrap();
        // Drain loop continues waiting for more work — shutdown via drop
        // is fine; assert the batch was acked.
        assert_eq!(queue.stats().unwrap().pending_records, 0);
        assert_eq!(core.seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn drain_retains_batch_on_accounting_mismatch() {
        // Core claims accepted=1, rejected=0 for a 2-record batch —
        // inconsistent accounting must NOT advance the ack offset.
        let core = start_scripted_core(1, 0, crate::api::PROTOCOL_VERSION_CURRENT).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        queue
            .append(sample_proto("collector-a", 1), "collector-a")
            .unwrap();
        queue
            .append(sample_proto("collector-a", 2), "collector-a")
            .unwrap();
        let err = run_drain_once(core.addr, &queue).await.unwrap_err();
        assert!(err.to_string().contains("accounting mismatch"), "{err}");
        assert_eq!(
            queue.stats().unwrap().pending_records,
            2,
            "mismatched accounting must retain the whole batch for replay"
        );
        assert_eq!(
            core.seen.lock().unwrap().len(),
            2,
            "core consumed the stream"
        );
    }

    #[tokio::test]
    async fn drain_retains_batch_on_impossible_counts() {
        // u64::MAX + u64::MAX overflows checked arithmetic — impossible
        // accounting must be rejected without acking.
        let core =
            start_scripted_core(u64::MAX, u64::MAX, crate::api::PROTOCOL_VERSION_CURRENT).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        queue
            .append(sample_proto("collector-a", 1), "collector-a")
            .unwrap();
        let err = run_drain_once(core.addr, &queue).await.unwrap_err();
        assert!(err.to_string().contains("accounting mismatch"), "{err}");
        assert_eq!(queue.stats().unwrap().pending_records, 1);
    }

    #[tokio::test]
    async fn drain_retains_batch_on_protocol_version_skew() {
        // Response protocol_version = 0 predates version negotiation —
        // major skew, contract untrustworthy, batch retained.
        let core = start_scripted_core(1, 0, 0).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        queue
            .append(sample_proto("collector-a", 1), "collector-a")
            .unwrap();
        let err = run_drain_once(core.addr, &queue).await.unwrap_err();
        assert!(err.to_string().contains("major skew"), "{err}");
        assert_eq!(queue.stats().unwrap().pending_records, 1);
    }

    #[tokio::test]
    async fn drain_replays_retained_batch_after_core_recovers() {
        // Restart/replay: a retained batch is delivered again once the
        // core returns a consistent response.
        let bad = start_scripted_core(0, 0, crate::api::PROTOCOL_VERSION_CURRENT).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        queue
            .append(sample_proto("collector-a", 1), "collector-a")
            .unwrap();
        queue
            .append(sample_proto("collector-a", 2), "collector-a")
            .unwrap();
        // First core under-reports (accepted=0 for 2 records) → retained.
        run_drain_once(bad.addr, &queue).await.unwrap_err();
        assert_eq!(queue.stats().unwrap().pending_records, 2);
        bad._server.abort();

        // "Restarted" core reports consistent accounting → replay acks.
        let good = start_scripted_core(2, 0, crate::api::PROTOCOL_VERSION_CURRENT).await;
        run_drain_once(good.addr, &queue).await.unwrap();
        assert_eq!(queue.stats().unwrap().pending_records, 0);
        assert_eq!(
            good.seen.lock().unwrap().len(),
            2,
            "replay delivered both records"
        );
    }

    /// R3 bus semantics: when the bounded event-bus router queue is
    /// full, `publish()` reports overload and `telemetry_ingest`
    /// aborts the stream WITHOUT a response — the collector retains the
    /// entire batch for replay instead of acknowledging a silent drop.
    #[tokio::test]
    async fn drain_retains_batch_on_bus_router_overload() {
        // Tiny router (capacity 4) + permanently-blocked subscriber.
        let bus = InProcessBus::new(4);
        let core = start_dev_core_mode(Some(bus), true).await;
        let dir = tempfile::tempdir().unwrap();
        let queue = std::sync::Arc::new(
            CollectorQueue::open(queue_config(dir.path(), 1_000_000, 24)).unwrap(),
        );
        for i in 0..10 {
            queue
                .append(sample_proto("collector-a", i), "collector-a")
                .unwrap();
        }

        let mut client = create_ingest_client(&drain_runtime(core.addr))
            .await
            .unwrap();
        let (_sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let q = std::sync::Arc::clone(&queue);
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            drain_queue_to_core(&mut client, q, "collector-a", sd_rx),
        )
        .await
        .expect("drain must return on bus overload, not hang");
        assert!(result.is_err(), "bus overload must surface as drain error");
        assert_eq!(
            queue.stats().unwrap().pending_records,
            10,
            "overload must retain the whole batch — no silent delivery loss"
        );
    }
}

pub async fn run_collector_manager(
    cfg: Arc<RuntimeConfig>,
    _collector_cfg: Arc<CollectorConfig>,
    bus: Arc<InProcessBus>,
    subscription_plan_tx: Option<mpsc::Sender<SubscriptionPlan>>,
    mut shutdown: watch::Receiver<bool>,
    queue_depth_counter: Arc<AtomicU64>,
    queue_bytes_counter: Arc<AtomicU64>,
    queue_max_bytes_counter: Arc<AtomicU64>,
    diag_state: Option<crate::collector::diagnostic_server::DiagnosticState>,
    supervisor: Option<crate::receiver_supervisor::SharedReceiverSupervisor>,
) -> Result<()> {
    let collector_id = cfg.collector_id.clone();
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let startup_instant = Instant::now();
    let mut subscribers: crate::subscriber::SubscriberHandleMap = HashMap::new();

    loop {
        let mut client = match create_ingest_client(&cfg).await {
            Ok(c) => c,
            Err(error) => {
                warn!(%error, %collector_id, "failed to connect to core for assignments; retrying in 5s");
                tokio::select! {
                    _ = shutdown.changed() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                }
            }
        };

        info!(
            %collector_id,
            %hostname,
            protocol_version = crate::api::PROTOCOL_VERSION,
            "collector registering with core"
        );

        let req = CollectorIdentity {
            collector_id: collector_id.clone(),
            hostname: hostname.clone(),
            protocol_version: crate::api::PROTOCOL_VERSION,
            auth_token: std::env::var("BONSAI_COLLECTOR_TOKEN").unwrap_or_default(),
        };

        let mut stream = match client.register_collector(req).await {
            Ok(s) => {
                if let Some(ref ds) = diag_state {
                    ds.mark_registered();
                }
                s.into_inner()
            }
            Err(error) => {
                warn!(%error, %collector_id, "failed to register collector; retrying in 5s");
                tokio::select! {
                    _ = shutdown.changed() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                }
            }
        };

        let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    info!("collector manager shutting down");
                    crate::subscriber::stop_all_subscribers(&mut subscribers).await;
                    return Ok(());
                }
                msg = stream.message() => {
                    match msg {
                        Ok(Some(update)) => {
                            handle_assignment_update(
                                update,
                                &bus,
                                &subscription_plan_tx,
                                &mut subscribers,
                            ).await;
                        }
                        Ok(None) => {
                            warn!("assignment stream closed by core; reconnecting");
                            break;
                        }
                        Err(error) => {
                            warn!(%error, "assignment stream error; reconnecting");
                            break;
                        }
                    }
                }
                _ = heartbeat_interval.tick() => {
                    let sub_count = subscribers.len() as u32;
                    let queue_depth = queue_depth_counter.load(Ordering::Relaxed);
                    let queue_bytes = queue_bytes_counter.load(Ordering::Relaxed);
                    let queue_max = queue_max_bytes_counter.load(Ordering::Relaxed);
                    let queue_utilization_pct = if queue_max > 0 {
                        (queue_bytes as f32 / queue_max as f32) * 100.0
                    } else {
                        0.0_f32
                    };
                    let uptime = startup_instant.elapsed().as_secs() as i64;
                    let memory_used_bytes = read_process_memory_bytes();

                    // Snapshot receiver statuses from the supervisor.
                    let receiver_snapshots: Vec<crate::api::pb::CollectorReceiverStatus> =
                        if let Some(ref sup) = supervisor {
                            let guard = sup.read().await;
                            guard.status_snapshot().into_iter().map(|s| {
                                crate::api::pb::CollectorReceiverStatus {
                                    name: s.name,
                                    state: format!("{:?}", s.state).to_lowercase(),
                                    addr: s.addr,
                                    packet_count: s.packet_count,
                                    error_count: s.error_count,
                                    last_error: s.last_error.unwrap_or_default(),
                                }
                            }).collect()
                        } else {
                            vec![]
                        };

                    // Push receiver statuses into DiagnosticState.
                    if let Some(ref ds) = diag_state {
                        let entries: Vec<crate::collector::diagnostic_server::ReceiverStatusEntry> =
                            receiver_snapshots.iter().map(|r| {
                                crate::collector::diagnostic_server::ReceiverStatusEntry {
                                    name: r.name.clone(),
                                    state: r.state.clone(),
                                    addr: r.addr.clone(),
                                    packet_count: r.packet_count,
                                    error_count: r.error_count,
                                    last_error: if r.last_error.is_empty() { None } else { Some(r.last_error.clone()) },
                                }
                            }).collect();
                        ds.update_stats(queue_depth, sub_count, Vec::new());
                        ds.update_receiver_statuses(entries);
                    }

                    let stats = CollectorStats {
                        collector_id: collector_id.clone(),
                        queue_depth_updates: queue_depth,
                        subscription_count: sub_count,
                        uptime_secs: uptime,
                        queue_bytes,
                        queue_utilization_pct,
                        active_subscribers: sub_count,
                        failed_subscribers: 0,
                        memory_used_bytes,
                        recent_warn_count: 0,
                        recent_error_count: 0,
                        receiver_statuses: receiver_snapshots,
                    };
                    if let Err(error) = client.heartbeat(stats).await {
                        warn!(%error, "failed to send heartbeat");
                    }
                }
            }
        }
    }
}

async fn handle_assignment_update(
    update: AssignmentUpdate,
    bus: &Arc<InProcessBus>,
    subscription_plan_tx: &Option<mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut crate::subscriber::SubscriberHandleMap,
) {
    if update.is_full_sync {
        info!("full assignment sync received, stopping unassigned subscribers");
        let assigned_addresses: std::collections::HashSet<String> = update
            .assignments
            .iter()
            .filter_map(|a| a.device.as_ref().map(|d| d.address.clone()))
            .collect();

        let current_addresses: Vec<String> = subscribers.keys().cloned().collect();
        for addr in current_addresses {
            if !assigned_addresses.contains(&addr) {
                crate::subscriber::stop_subscriber(&addr, subscribers).await;
            }
        }
    }

    for assignment in update.assignments {
        let Some(device) = assignment.device else {
            continue;
        };
        let mut target = match crate::api::target_from_managed_device(Some(device)) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "invalid device in assignment");
                continue;
            }
        };

        // Core sends resolved credentials
        target.username = Some(assignment.username);
        target.password = Some(assignment.password);

        // In collector mode, we don't have a vault, so we pass a dummy/empty vault or update spawn_subscriber
        // For now, we'll pass the Arc<InProcessBus> and Option<mpsc::Sender<SubscriptionPlan>> correctly.
        if let Err(error) = crate::subscriber::spawn_subscriber_with_creds(
            target,
            bus,
            None, // debouncer is local to core
            subscription_plan_tx.as_ref(),
            subscribers,
        )
        .await
        {
            warn!(%error, "failed to start assigned subscriber");
        }
    }
}

async fn create_ingest_client(
    cfg: &RuntimeConfig,
) -> Result<BonsaiGraphClient<tonic::transport::Channel>> {
    let mut endpoint = tonic::transport::Endpoint::from_shared(cfg.core_ingest_endpoint.clone())?;

    if cfg.tls.enabled {
        let tls = client_tls_config(&cfg.tls)?;
        endpoint = endpoint.tls_config(tls)?;
    }

    let channel = endpoint.connect().await?;
    Ok(BonsaiGraphClient::new(channel))
}

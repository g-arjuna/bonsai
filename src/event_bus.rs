use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::telemetry::TelemetryUpdate;

static EVENT_BUS_DEPTH: AtomicU64 = AtomicU64::new(0);
static EVENT_BUS_RECEIVERS: AtomicU64 = AtomicU64::new(0);

const SLOW_SUBSCRIBER_WARN_THRESHOLD_PCT: u64 = 50;

#[derive(Clone, Copy, Debug, Default)]
pub struct EventBusSnapshot {
    pub depth: u64,
    pub receivers: u64,
}

/// Overflow behaviour when a subscriber's queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Drop the incoming message. Supported by MpscSubscriber.
    DropNewest,
    /// Block the router until space is available. Supported by MpscSubscriber.
    BlockProducer,
    /// Drop the oldest queued message. Use BroadcastSubscriber for this policy.
    DropOldest,
}

#[async_trait::async_trait]
pub trait BusSubscriber: Send + Sync {
    fn name(&self) -> &str;
    fn policy(&self) -> OverflowPolicy;
    async fn handle(&self, update: Arc<TelemetryUpdate>);
}

/// Mpsc-backed subscriber. Supports DropNewest and BlockProducer.
/// For DropOldest semantics use BroadcastSubscriber.
pub struct MpscSubscriber {
    name: String,
    tx: mpsc::Sender<Arc<TelemetryUpdate>>,
    policy: OverflowPolicy,
    capacity: usize,
    last_queue_warn_secs: AtomicU64,
}

impl MpscSubscriber {
    pub fn new(
        name: &str,
        capacity: usize,
        policy: OverflowPolicy,
    ) -> (Arc<Self>, mpsc::Receiver<Arc<TelemetryUpdate>>) {
        assert!(
            policy != OverflowPolicy::DropOldest,
            "MpscSubscriber does not support DropOldest — use BroadcastSubscriber instead"
        );
        let (tx, rx) = mpsc::channel(capacity);
        (
            Arc::new(Self {
                name: name.to_string(),
                tx,
                policy,
                capacity,
                last_queue_warn_secs: AtomicU64::new(0),
            }),
            rx,
        )
    }
}

#[async_trait::async_trait]
impl BusSubscriber for MpscSubscriber {
    fn name(&self) -> &str {
        &self.name
    }
    fn policy(&self) -> OverflowPolicy {
        self.policy
    }

    async fn handle(&self, update: Arc<TelemetryUpdate>) {
        let name = self.name.clone();
        match self.policy {
            OverflowPolicy::BlockProducer => {
                if self.tx.send(update).await.is_err() {
                    metrics::counter!(
                        "bonsai_event_bus_subscriber_errors_total",
                        "subscriber" => name
                    )
                    .increment(1);
                }
            }
            OverflowPolicy::DropNewest | OverflowPolicy::DropOldest => {
                if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(update) {
                    metrics::counter!(
                        "bonsai_event_bus_subscriber_drops_total",
                        "subscriber" => name,
                        "reason" => "drop_newest"
                    )
                    .increment(1);
                }
            }
        }

        let depth = (self.capacity.saturating_sub(self.tx.capacity())) as u64;
        metrics::gauge!("bonsai_subscriber_queue_depth", "subscriber" => self.name.clone())
            .set(depth as f64);

        if let Some(fill_pct) = (depth * 100).checked_div(self.capacity as u64)
            && fill_pct >= SLOW_SUBSCRIBER_WARN_THRESHOLD_PCT
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last = self.last_queue_warn_secs.load(Ordering::Relaxed);
            if now_secs.saturating_sub(last) >= 60 {
                self.last_queue_warn_secs.store(now_secs, Ordering::Relaxed);
                warn!(
                    subscriber = %self.name,
                    depth,
                    capacity = self.capacity,
                    fill_pct,
                    "subscriber queue is {}% full",
                    fill_pct
                );
            }
        }
    }
}

/// Broadcast-backed subscriber with true DropOldest semantics.
/// When the receiver lags behind capacity, the oldest queued messages are overwritten.
pub struct BroadcastSubscriber {
    name: String,
    tx: broadcast::Sender<Arc<TelemetryUpdate>>,
    capacity: usize,
}

impl BroadcastSubscriber {
    pub fn new(
        name: &str,
        capacity: usize,
    ) -> (Arc<Self>, broadcast::Receiver<Arc<TelemetryUpdate>>) {
        let (tx, rx) = broadcast::channel(capacity);
        (
            Arc::new(Self {
                name: name.to_string(),
                tx,
                capacity,
            }),
            rx,
        )
    }
}

#[async_trait::async_trait]
impl BusSubscriber for BroadcastSubscriber {
    fn name(&self) -> &str {
        &self.name
    }
    fn policy(&self) -> OverflowPolicy {
        OverflowPolicy::DropOldest
    }

    async fn handle(&self, update: Arc<TelemetryUpdate>) {
        // broadcast::send drops the oldest slot when full — true DropOldest.
        if self.tx.send(update).is_err() {
            // No active receivers — non-fatal.
        }
        let depth = self.tx.len() as u64;
        metrics::gauge!("bonsai_subscriber_queue_depth", "subscriber" => self.name.clone())
            .set(depth as f64);
        if let Some(fill_pct) = (depth * 100).checked_div(self.capacity as u64)
            && fill_pct >= SLOW_SUBSCRIBER_WARN_THRESHOLD_PCT
        {
            metrics::counter!(
                "bonsai_event_bus_subscriber_drops_total",
                "subscriber" => self.name.clone(),
                "reason" => "drop_oldest"
            )
            .increment(1);
        }
    }
}

/// The single in-process event bus. Publishers call publish(); subscribers register via
/// add_subscriber(). The internal router delivers Arc<TelemetryUpdate> to all registered
/// subscribers concurrently. Subscriber list is lock-free on the read path via ArcSwap.
pub struct InProcessBus {
    router_tx: mpsc::Sender<Arc<TelemetryUpdate>>,
    subscribers: Arc<ArcSwap<Vec<Arc<dyn BusSubscriber>>>>,
    router_capacity: usize,
    broadcast_tx: broadcast::Sender<Arc<TelemetryUpdate>>,
}

impl InProcessBus {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (router_tx, router_rx) = mpsc::channel(capacity);
        let subscribers = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let (broadcast_tx, _) = broadcast::channel(1024);

        let bus = Arc::new(Self {
            router_tx,
            subscribers: Arc::clone(&subscribers),
            router_capacity: capacity,
            broadcast_tx,
        });

        tokio::spawn(run_router(router_rx, subscribers));

        bus
    }

    /// Subscribe to all published updates via a broadcast channel.
    /// Used by lightweight adapters (SNMP, syslog) that need direct access.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<TelemetryUpdate>> {
        self.broadcast_tx.subscribe()
    }

    /// Register a subscriber. Subscribers are typically added at startup before traffic starts.
    /// The ArcSwap write is a clone of the existing Vec — this is a rare operation.
    pub async fn add_subscriber(&self, subscriber: Arc<dyn BusSubscriber>) {
        self.subscribers.rcu(|current| {
            let mut updated = current.as_ref().clone();
            updated.push(Arc::clone(&subscriber));
            Arc::new(updated)
        });
        let count = self.subscribers.load().len() as u64;
        EVENT_BUS_RECEIVERS.store(count, Ordering::Relaxed);
    }

    /// Number of registered subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.load().len()
    }

    /// Wrap in Arc and submit to the router. Publishing is non-blocking; if the router
    /// queue is full the message is dropped and a metric is incremented.
    pub fn publish(&self, update: TelemetryUpdate) {
        let arc_update = Arc::new(update);
        let _ = self.broadcast_tx.send(Arc::clone(&arc_update));
        if self.router_tx.try_send(arc_update).is_err() {
            warn!("event bus router queue full, dropping message");
            metrics::counter!("bonsai_event_bus_router_drops_total").increment(1);
        }
        let depth = (self
            .router_capacity
            .saturating_sub(self.router_tx.capacity())) as u64;
        EVENT_BUS_DEPTH.store(depth, Ordering::Relaxed);
        metrics::gauge!("bonsai_event_bus_depth").set(depth as f64);
    }

    pub fn snapshot() -> EventBusSnapshot {
        EventBusSnapshot {
            depth: EVENT_BUS_DEPTH.load(Ordering::Relaxed),
            receivers: EVENT_BUS_RECEIVERS.load(Ordering::Relaxed),
        }
    }
}

async fn run_router(
    mut rx: mpsc::Receiver<Arc<TelemetryUpdate>>,
    subs: Arc<ArcSwap<Vec<Arc<dyn BusSubscriber>>>>,
) {
    while let Some(update) = rx.recv().await {
        // Lock-free load — returns an Arc<Vec<...>> snapshot.
        let subs_snapshot = subs.load();
        if subs_snapshot.is_empty() {
            continue;
        }
        // Clone Arc pointer (8 bytes) per subscriber, not the full struct.
        let mut futures = Vec::with_capacity(subs_snapshot.len());
        for sub in subs_snapshot.iter() {
            futures.push(sub.handle(Arc::clone(&update)));
        }
        futures::future::join_all(futures).await;
    }
}

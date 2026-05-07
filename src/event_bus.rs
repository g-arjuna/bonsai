use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{broadcast, mpsc, RwLock};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// If the queue is full, drop the oldest message to make room.
    DropOldest,
    /// If the queue is full, drop the incoming message.
    DropNewest,
    /// If the queue is full, block the router until there is space.
    BlockProducer,
}

#[async_trait::async_trait]
pub trait BusSubscriber: Send + Sync {
    fn name(&self) -> &str;
    fn policy(&self) -> OverflowPolicy;
    async fn handle(&self, update: TelemetryUpdate);
}

pub trait EventBus: Send + Sync + 'static {
    fn publish(&self, update: TelemetryUpdate);
    
    /// DEPRECATED: Use register_subscriber instead for better isolation.
    fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate>;
}

/// A subscriber implementation using an mpsc channel.
pub struct MpscSubscriber {
    name: String,
    tx: mpsc::Sender<TelemetryUpdate>,
    policy: OverflowPolicy,
    capacity: usize,
    last_queue_warn_secs: AtomicU64,
}

impl MpscSubscriber {
    pub fn new(name: &str, capacity: usize, policy: OverflowPolicy) -> (Arc<Self>, mpsc::Receiver<TelemetryUpdate>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Arc::new(Self {
            name: name.to_string(),
            tx,
            policy,
            capacity,
            last_queue_warn_secs: AtomicU64::new(0),
        }), rx)
    }
}

#[async_trait::async_trait]
impl BusSubscriber for MpscSubscriber {
    fn name(&self) -> &str { &self.name }
    fn policy(&self) -> OverflowPolicy { self.policy }
    async fn handle(&self, update: TelemetryUpdate) {
        let name = self.name.clone();
        match self.policy {
            OverflowPolicy::BlockProducer => {
                if let Err(_) = self.tx.send(update).await {
                    metrics::counter!("bonsai_event_bus_subscriber_errors_total", "subscriber" => name).increment(1);
                }
            }
            OverflowPolicy::DropNewest => {
                if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(update.clone()) {
                    metrics::counter!("bonsai_event_bus_subscriber_drops_total", "subscriber" => name, "reason" => "drop_newest").increment(1);
                } else if let Err(_) = self.tx.try_send(update) {
                     // closed
                }
            }
            OverflowPolicy::DropOldest => {
                // For mpsc, we can't easily drop oldest. 
                // We'll use try_send and log failure for now.
                if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(update) {
                    metrics::counter!("bonsai_event_bus_subscriber_drops_total", "subscriber" => name, "reason" => "drop_oldest_failed").increment(1);
                }
            }
        }
        
        let depth = (self.capacity - self.tx.capacity()) as u64;
        metrics::gauge!("bonsai_subscriber_queue_depth", "subscriber" => self.name.clone()).set(depth as f64);
        
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

pub struct InProcessBus {
    router_tx: mpsc::Sender<TelemetryUpdate>,
    subscribers: Arc<RwLock<Vec<Arc<dyn BusSubscriber>>>>,
    legacy_tx: broadcast::Sender<TelemetryUpdate>,
    router_capacity: usize,
}

impl InProcessBus {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (router_tx, router_rx) = mpsc::channel(capacity);
        let (legacy_tx, _) = broadcast::channel(capacity);
        let subscribers = Arc::new(RwLock::new(Vec::new()));
        
        let bus = Arc::new(Self {
            router_tx,
            subscribers: Arc::clone(&subscribers),
            legacy_tx,
            router_capacity: capacity,
        });

        tokio::spawn(run_router(router_rx, subscribers));

        bus
    }

    pub async fn add_subscriber(&self, subscriber: Arc<dyn BusSubscriber>) {
        let mut subs = self.subscribers.write().await;
        subs.push(subscriber);
        EVENT_BUS_RECEIVERS.store(subs.len() as u64, Ordering::Relaxed);
    }

    pub fn publish(&self, update: TelemetryUpdate) {
        // 1. Send to legacy broadcast
        let _ = self.legacy_tx.send(update.clone());
        
        // 2. Send to router
        if let Err(_) = self.router_tx.try_send(update) {
            warn!("Event bus router queue full, dropping message");
            metrics::counter!("bonsai_event_bus_router_drops_total").increment(1);
        }

        let depth = (self.router_capacity - self.router_tx.capacity()) as u64;
        EVENT_BUS_DEPTH.store(depth, Ordering::Relaxed);
        metrics::gauge!("bonsai_event_bus_depth").set(depth as f64);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate> {
        self.legacy_tx.subscribe()
    }

    pub fn snapshot() -> EventBusSnapshot {
        EventBusSnapshot {
            depth: EVENT_BUS_DEPTH.load(Ordering::Relaxed),
            receivers: EVENT_BUS_RECEIVERS.load(Ordering::Relaxed),
        }
    }
}

async fn run_router(mut rx: mpsc::Receiver<TelemetryUpdate>, subs: Arc<RwLock<Vec<Arc<dyn BusSubscriber>>>>) {
    while let Some(update) = rx.recv().await {
        let subs_guard = subs.read().await;
        if subs_guard.is_empty() { continue; }
        
        // Push to all subscribers in parallel
        let mut futures = Vec::with_capacity(subs_guard.len());
        for sub in subs_guard.iter() {
            futures.push(sub.handle(update.clone()));
        }
        futures::future::join_all(futures).await;
    }
}

impl EventBus for InProcessBus {
    fn publish(&self, update: TelemetryUpdate) {
        InProcessBus::publish(self, update);
    }

    fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate> {
        InProcessBus::subscribe(self)
    }
}

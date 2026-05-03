use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;
use tracing::warn;

use crate::telemetry::TelemetryUpdate;

static EVENT_BUS_DEPTH: AtomicU64 = AtomicU64::new(0);
static EVENT_BUS_RECEIVERS: AtomicU64 = AtomicU64::new(0);

/// Log a warning when the channel is this fraction full (50 %).
const SLOW_SUBSCRIBER_WARN_THRESHOLD_PCT: u64 = 50;

#[derive(Clone, Copy, Debug, Default)]
pub struct EventBusSnapshot {
    pub depth: u64,
    pub receivers: u64,
}

/// Publish/subscribe bus for telemetry updates.
///
/// The trait lets future implementations (e.g. NATS, Kafka) replace
/// `InProcessBus` without touching callers.
pub trait EventBus: Send + Sync + 'static {
    /// Publish one update to all current subscribers. Silently drops if no
    /// subscribers or the channel is full (back-pressure is the subscriber's job).
    fn publish(&self, update: TelemetryUpdate);

    /// Return a new receiver. Each receiver sees every message published after
    /// the call — earlier messages are not replayed.
    fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate>;
}

/// Single-process broadcast bus backed by a `tokio::sync::broadcast` channel.
pub struct InProcessBus {
    tx: broadcast::Sender<TelemetryUpdate>,
    capacity: u64,
}

impl InProcessBus {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self { tx, capacity: capacity as u64 })
    }

    /// Publish one update. Silently drops if no subscribers or channel is full.
    pub fn publish(&self, update: TelemetryUpdate) {
        let _ = self.tx.send(update);
        let depth = self.tx.len() as u64;
        let capacity = self.capacity;
        let receivers = self.tx.receiver_count() as u64;
        EVENT_BUS_DEPTH.store(depth, Ordering::Relaxed);
        EVENT_BUS_RECEIVERS.store(receivers, Ordering::Relaxed);
        metrics::gauge!("bonsai_event_bus_depth").set(depth as f64);
        metrics::gauge!("bonsai_event_bus_receivers").set(receivers as f64);
        metrics::gauge!("bonsai_event_bus_capacity").set(capacity as f64);

        // Warn when a lagging subscriber is causing back-pressure.
        if let Some(fill_pct) = (depth * 100).checked_div(capacity)
            && fill_pct >= SLOW_SUBSCRIBER_WARN_THRESHOLD_PCT
        {
            warn!(
                depth,
                capacity,
                fill_pct,
                "event bus channel is {}% full — a subscriber may be lagging",
                fill_pct
            );
            metrics::counter!("bonsai_event_bus_slow_subscriber_warnings_total").increment(1);
        }
    }

    /// Return a new receiver that sees all messages published after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate> {
        let rx = self.tx.subscribe();
        EVENT_BUS_DEPTH.store(self.tx.len() as u64, Ordering::Relaxed);
        EVENT_BUS_RECEIVERS.store(self.tx.receiver_count() as u64, Ordering::Relaxed);
        rx
    }

    pub fn snapshot() -> EventBusSnapshot {
        EventBusSnapshot {
            depth: EVENT_BUS_DEPTH.load(Ordering::Relaxed),
            receivers: EVENT_BUS_RECEIVERS.load(Ordering::Relaxed),
        }
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

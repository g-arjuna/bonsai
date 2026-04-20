use std::sync::Arc;

use tokio::sync::broadcast;

use crate::telemetry::TelemetryUpdate;

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
}

impl InProcessBus {
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self { tx })
    }

    /// Publish one update. Silently drops if no subscribers or channel is full.
    pub fn publish(&self, update: TelemetryUpdate) {
        let _ = self.tx.send(update);
    }

    /// Return a new receiver that sees all messages published after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<TelemetryUpdate> {
        self.tx.subscribe()
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

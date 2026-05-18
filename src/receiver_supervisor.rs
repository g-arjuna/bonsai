//! D3-13 T1 — ReceiverSupervisor
//!
//! Tracks all protocol receivers (syslog_udp, syslog_tcp, snmp, bmp, bgp_ls,
//! otlp, netflow) as named, restartable tasks.  Each receiver is wrapped in an
//! `AbortHandle` so it can be stopped cleanly; a `watch` channel is used as a
//! per-receiver shutdown signal so the existing receiver implementations need
//! no changes.
//!
//! Shared in `AppState` as `Arc<RwLock<ReceiverSupervisor>>`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tracing::{info, warn};

// ── Public status types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverState {
    Listening,
    Stopped,
    Error,
    PortConflict,
    Disabled,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReceiverStatusSnapshot {
    pub name: String,
    pub state: ReceiverState,
    pub addr: String,
    pub packet_count: u64,
    pub error_count: u64,
    pub last_packet_at_ns: Option<i64>,
    pub last_error: Option<String>,
}

// ── Internal entry ─────────────────────────────────────────────────────────────

struct ReceiverEntry {
    abort_handle: Option<AbortHandle>,
    /// Per-receiver shutdown sender — dropping it signals the receiver to stop.
    shutdown_tx: Option<watch::Sender<bool>>,
    status: ReceiverStatusSnapshot,
}

// ── Supervisor ─────────────────────────────────────────────────────────────────

pub struct ReceiverSupervisor {
    entries: HashMap<&'static str, ReceiverEntry>,
}

impl ReceiverSupervisor {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Spawn a receiver by name.  `factory` receives a per-receiver shutdown
    /// `watch::Receiver<bool>` and must return a boxed future that resolves to
    /// `anyhow::Result<()>`.  If the bind address is already in use the factory
    /// should return an `Err` containing "in use" or "AddrInUse" in the message —
    /// the supervisor will set `PortConflict` state.
    pub fn spawn<F, Fut>(&mut self, name: &'static str, addr: String, factory: F)
    where
        F: FnOnce(watch::Receiver<bool>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.abort(name);

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let name_str = name;

        let handle = tokio::spawn(async move {
            match factory(shutdown_rx).await {
                Ok(()) => {
                    info!(receiver = name_str, "receiver exited cleanly");
                }
                Err(e) => {
                    let msg = e.to_string();
                    if is_port_conflict(&msg) {
                        warn!(receiver = name_str, error = %msg, "receiver port conflict");
                    } else {
                        warn!(receiver = name_str, error = %msg, "receiver error");
                    }
                }
            }
        });

        let abort_handle = handle.abort_handle();

        let snapshot = ReceiverStatusSnapshot {
            name: name.to_string(),
            state: ReceiverState::Listening,
            addr,
            packet_count: 0,
            error_count: 0,
            last_packet_at_ns: None,
            last_error: None,
        };

        self.entries.insert(
            name,
            ReceiverEntry {
                abort_handle: Some(abort_handle),
                shutdown_tx: Some(shutdown_tx),
                status: snapshot,
            },
        );
    }

    /// Register a receiver as disabled (not spawned).
    pub fn register_disabled(&mut self, name: &'static str, addr: String) {
        self.entries.insert(
            name,
            ReceiverEntry {
                abort_handle: None,
                shutdown_tx: None,
                status: ReceiverStatusSnapshot {
                    name: name.to_string(),
                    state: ReceiverState::Disabled,
                    addr,
                    packet_count: 0,
                    error_count: 0,
                    last_packet_at_ns: None,
                    last_error: None,
                },
            },
        );
    }

    /// Gracefully stop a receiver (signals shutdown, then aborts after a tick).
    pub fn abort(&mut self, name: &str) {
        if let Some(entry) = self.entries.get_mut(name) {
            if let Some(tx) = entry.shutdown_tx.take() {
                let _ = tx.send(true);
            }
            if let Some(handle) = entry.abort_handle.take() {
                handle.abort();
            }
            entry.status.state = ReceiverState::Stopped;
        }
    }

    /// Mark a receiver's state after a bind failure.
    pub fn mark_port_conflict(&mut self, name: &str, error: String) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status.state = ReceiverState::PortConflict;
            entry.status.last_error = Some(error);
        }
    }

    /// Mark a receiver's state as error.
    pub fn mark_error(&mut self, name: &str, error: String) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status.state = ReceiverState::Error;
            entry.status.last_error = Some(error);
        }
    }

    /// Record a received packet for metrics.
    pub fn record_packet(&mut self, name: &str) {
        if let Some(entry) = self.entries.get_mut(name) {
            entry.status.packet_count += 1;
            entry.status.last_packet_at_ns = Some(now_ns());
        }
    }

    /// Snapshot all receiver statuses for the API.
    pub fn status_snapshot(&self) -> Vec<ReceiverStatusSnapshot> {
        let mut out: Vec<ReceiverStatusSnapshot> = self
            .entries
            .values()
            .map(|e| e.status.clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Status for a single receiver by name.
    pub fn status(&self, name: &str) -> Option<&ReceiverStatusSnapshot> {
        self.entries.get(name).map(|e| &e.status)
    }
}

impl Default for ReceiverSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

// ── Convenience type alias (used in AppState) ──────────────────────────────────

pub type SharedReceiverSupervisor = Arc<tokio::sync::RwLock<ReceiverSupervisor>>;

pub fn new_shared() -> SharedReceiverSupervisor {
    Arc::new(tokio::sync::RwLock::new(ReceiverSupervisor::new()))
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn is_port_conflict(msg: &str) -> bool {
    msg.contains("in use")
        || msg.contains("AddrInUse")
        || msg.contains("address already in use")
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}

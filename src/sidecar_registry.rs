//! Sidecar registry — CV7 T4-2.
//!
//! Tracks Python (or future Rust/other) sidecar processes bound to bonsai over
//! gRPC. The registry is the source of truth for "what is currently doing
//! detection / ML / AIOps work for this bonsai instance." It is surfaced via:
//!   • `GET /api/sidecars` (HTTP) — consumed by the bonpy UI and ops scripts
//!   • `/health` (HTTP) — degraded when `BONSAI_REQUIRE_SIDECAR` is unmet
//!
//! The registry is in-memory only. After a bonsai restart the registry is
//! empty until sidecars heartbeat (or re-register). Sidecars are expected to
//! detect `reregister_required = true` on a `SidecarHeartbeat` response and
//! call `RegisterSidecar` again.
//!
//! See `docs/architecture/sidecars.md` and the 2026-05-14 ADR in
//! `DECISIONS.md` for the full architectural reasoning.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use uuid::Uuid;

/// Heartbeat cadence the sidecar is expected to maintain. Used by the
/// `status_for` time-window thresholds below.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// After this many seconds without a heartbeat, status flips from `Healthy`
/// to `Stale`. Roughly 3× the heartbeat interval.
pub const STALE_AFTER: Duration = Duration::from_secs(45);

/// After this many seconds without a heartbeat, status flips to `Lost`. The
/// entry stays visible so the operator sees "this sidecar used to be here,
/// it's gone now."
pub const LOST_AFTER: Duration = Duration::from_secs(120);

/// Grace window after bonsai startup during which a `BONSAI_REQUIRE_SIDECAR`
/// gate is suppressed. Lets sidecars register without a startup race.
pub const REQUIRE_GRACE: Duration = Duration::from_secs(60);

/// Current health of a registered sidecar relative to wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarStatus {
    Healthy,
    Stale,
    Lost,
}

/// One registered sidecar. Heartbeat counters and `status_json` are updated by
/// each `SidecarHeartbeat`; the rest of the fields are set at registration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarEntry {
    pub sidecar_id: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub address: String,
    pub registered_at_ns: u64,
    pub last_heartbeat_ns: u64,
    pub events_in_total: u64,
    pub detections_out_total: u64,
    /// Sidecar-specific JSON (rules_loaded, ml model paths, etc). Parsed by the
    /// bonpy UI; bonsai treats it as opaque.
    pub status_json: String,
}

impl SidecarEntry {
    /// Compute the current status relative to `now_ns`. Pure function so it is
    /// trivially testable.
    pub fn status_at(&self, now_ns: u64) -> SidecarStatus {
        let elapsed = now_ns.saturating_sub(self.last_heartbeat_ns);
        let elapsed_secs = elapsed / 1_000_000_000;
        if elapsed_secs >= LOST_AFTER.as_secs() {
            SidecarStatus::Lost
        } else if elapsed_secs >= STALE_AFTER.as_secs() {
            SidecarStatus::Stale
        } else {
            SidecarStatus::Healthy
        }
    }
}

/// Snapshot of a single sidecar with its computed status. This is what the
/// HTTP layer serialises to JSON.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarSnapshot {
    #[serde(flatten)]
    pub entry: SidecarEntry,
    pub status: SidecarStatus,
}

/// In-memory registry. Wrapped in `Arc<RwLock<…>>` by the caller so it can be
/// shared between the gRPC service and the HTTP layer.
#[derive(Debug, Default)]
pub struct SidecarRegistryInner {
    /// sidecar_id → entry
    entries: HashMap<String, SidecarEntry>,
    /// kind values that bonsai considers required for healthy operation.
    /// Populated from `BONSAI_REQUIRE_SIDECAR` at process start.
    required_kinds: Vec<String>,
    /// Process start time (ns since epoch). Used to compute the
    /// `REQUIRE_GRACE` window.
    started_at_ns: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SidecarRegistry {
    inner: Arc<RwLock<SidecarRegistryInner>>,
}

impl SidecarRegistry {
    /// Create a new empty registry. `required_kinds` is the parsed
    /// `BONSAI_REQUIRE_SIDECAR` list (empty = no requirement).
    pub fn new(required_kinds: Vec<String>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(SidecarRegistryInner {
                entries: HashMap::new(),
                required_kinds,
                started_at_ns: now_ns(),
            })),
        }
    }

    /// Parse the comma-separated `BONSAI_REQUIRE_SIDECAR` env value into a
    /// list of `kind` strings. Whitespace is trimmed; empty entries dropped.
    pub fn parse_required_kinds(raw: &str) -> Vec<String> {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Register a new sidecar (or replace the existing entry of the same
    /// `name+kind`). Returns the assigned `sidecar_id`.
    pub async fn register(
        &self,
        name: String,
        kind: String,
        version: String,
        capabilities: Vec<String>,
        address: String,
    ) -> String {
        let now = now_ns();
        let sidecar_id = Uuid::new_v4().to_string();
        let entry = SidecarEntry {
            sidecar_id: sidecar_id.clone(),
            name: name.clone(),
            kind: kind.clone(),
            version,
            capabilities,
            address,
            registered_at_ns: now,
            last_heartbeat_ns: now,
            events_in_total: 0,
            detections_out_total: 0,
            status_json: String::new(),
        };

        let mut guard = self.inner.write().await;
        // Replace any prior entry with the same (name, kind) — sidecar restart
        // case. The old sidecar_id is dropped.
        guard
            .entries
            .retain(|_, e| !(e.name == name && e.kind == kind));
        guard.entries.insert(sidecar_id.clone(), entry);
        sidecar_id
    }

    /// Update an existing sidecar's heartbeat counters. Returns `Ok(())` if
    /// the sidecar_id is known; `Err(())` if it is not (caller should signal
    /// `reregister_required = true` to the sidecar).
    pub async fn heartbeat(
        &self,
        sidecar_id: &str,
        events_in_total: u64,
        detections_out_total: u64,
        status_json: String,
    ) -> Result<(), ()> {
        let mut guard = self.inner.write().await;
        match guard.entries.get_mut(sidecar_id) {
            Some(entry) => {
                entry.last_heartbeat_ns = now_ns();
                entry.events_in_total = events_in_total;
                entry.detections_out_total = detections_out_total;
                entry.status_json = status_json;
                Ok(())
            }
            None => Err(()),
        }
    }

    /// Snapshot every registered sidecar with its computed status. Lost
    /// entries are kept so the operator sees what disappeared.
    pub async fn snapshot(&self) -> Vec<SidecarSnapshot> {
        let now = now_ns();
        let guard = self.inner.read().await;
        let mut out: Vec<SidecarSnapshot> = guard
            .entries
            .values()
            .map(|e| SidecarSnapshot {
                entry: e.clone(),
                status: e.status_at(now),
            })
            .collect();
        // Deterministic order: by name then kind, so the UI doesn't shuffle on
        // every poll.
        out.sort_by(|a, b| {
            a.entry
                .name
                .cmp(&b.entry.name)
                .then_with(|| a.entry.kind.cmp(&b.entry.kind))
        });
        out
    }

    /// The list of `kind` values bonsai requires (from BONSAI_REQUIRE_SIDECAR).
    pub async fn required_kinds(&self) -> Vec<String> {
        self.inner.read().await.required_kinds.clone()
    }

    /// Of the required kinds, which are currently missing or `Lost`? Returns
    /// `Some(missing)` if the gate is active, `None` if either no kinds are
    /// required or we're still inside the startup grace window.
    pub async fn missing_required(&self) -> Option<Vec<String>> {
        let now = now_ns();
        let guard = self.inner.read().await;
        if guard.required_kinds.is_empty() {
            return None;
        }
        let grace_elapsed = now.saturating_sub(guard.started_at_ns) / 1_000_000_000;
        if grace_elapsed < REQUIRE_GRACE.as_secs() {
            return None;
        }
        let mut missing = Vec::new();
        for required in &guard.required_kinds {
            let satisfied = guard.entries.values().any(|e| {
                &e.kind == required
                    && matches!(
                        e.status_at(now),
                        SidecarStatus::Healthy | SidecarStatus::Stale
                    )
            });
            if !satisfied {
                missing.push(required.clone());
            }
        }
        Some(missing)
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_at(name: &str, kind: &str, last_heartbeat_ns: u64) -> SidecarEntry {
        SidecarEntry {
            sidecar_id: format!("id-{name}"),
            name: name.into(),
            kind: kind.into(),
            version: "0.1.0".into(),
            capabilities: vec![],
            address: "localhost:50052".into(),
            registered_at_ns: 0,
            last_heartbeat_ns,
            events_in_total: 0,
            detections_out_total: 0,
            status_json: String::new(),
        }
    }

    #[test]
    fn status_healthy_within_window() {
        let e = entry_at("a", "rules", 0);
        let just_now = 10 * 1_000_000_000; // 10s elapsed
        assert_eq!(e.status_at(just_now), SidecarStatus::Healthy);
    }

    #[test]
    fn status_stale_after_45s() {
        let e = entry_at("a", "rules", 0);
        let elapsed_60s = 60 * 1_000_000_000;
        assert_eq!(e.status_at(elapsed_60s), SidecarStatus::Stale);
    }

    #[test]
    fn status_lost_after_120s() {
        let e = entry_at("a", "rules", 0);
        let elapsed_150s = 150 * 1_000_000_000;
        assert_eq!(e.status_at(elapsed_150s), SidecarStatus::Lost);
    }

    #[test]
    fn parse_required_kinds_handles_spaces_and_empties() {
        let got = SidecarRegistry::parse_required_kinds(" rules, ml-inference , ,foo ");
        assert_eq!(got, vec!["rules", "ml-inference", "foo"]);
    }

    #[test]
    fn parse_required_kinds_empty_input() {
        assert!(SidecarRegistry::parse_required_kinds("").is_empty());
        assert!(SidecarRegistry::parse_required_kinds("   ").is_empty());
    }

    #[tokio::test]
    async fn register_then_heartbeat_then_snapshot() {
        let reg = SidecarRegistry::new(vec![]);
        let id = reg
            .register(
                "rules-alpha".into(),
                "rules".into(),
                "0.1.0".into(),
                vec!["bgp_session_down".into()],
                "localhost:50052".into(),
            )
            .await;
        assert!(reg.heartbeat(&id, 5, 1, "{}".into()).await.is_ok());
        let snap = reg.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].entry.name, "rules-alpha");
        assert_eq!(snap[0].entry.events_in_total, 5);
        assert_eq!(snap[0].entry.detections_out_total, 1);
        assert_eq!(snap[0].status, SidecarStatus::Healthy);
    }

    #[tokio::test]
    async fn heartbeat_unknown_id_returns_err() {
        let reg = SidecarRegistry::new(vec![]);
        let res = reg.heartbeat("not-an-id", 0, 0, String::new()).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn re_register_same_name_kind_replaces() {
        let reg = SidecarRegistry::new(vec![]);
        let id1 = reg
            .register(
                "rules-alpha".into(),
                "rules".into(),
                "0.1.0".into(),
                vec![],
                "localhost:50052".into(),
            )
            .await;
        let id2 = reg
            .register(
                "rules-alpha".into(),
                "rules".into(),
                "0.1.1".into(),
                vec![],
                "localhost:50052".into(),
            )
            .await;
        assert_ne!(id1, id2);
        let snap = reg.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].entry.version, "0.1.1");
        // Old id is gone — heartbeat must fail.
        assert!(reg.heartbeat(&id1, 0, 0, String::new()).await.is_err());
    }

    #[tokio::test]
    async fn missing_required_is_none_without_required_kinds() {
        let reg = SidecarRegistry::new(vec![]);
        assert!(reg.missing_required().await.is_none());
    }

    #[tokio::test]
    async fn missing_required_is_none_during_grace_window() {
        // Brand-new registry — grace window is in effect → None.
        let reg = SidecarRegistry::new(vec!["rules".into()]);
        assert!(reg.missing_required().await.is_none());
    }

    #[tokio::test]
    async fn missing_required_lists_kinds_after_grace() {
        let reg = SidecarRegistry::new(vec!["rules".into(), "ml-inference".into()]);
        // Force the grace window to be considered elapsed by rewinding the
        // started_at_ns marker by 5 minutes.
        {
            let mut g = reg.inner.write().await;
            g.started_at_ns = g.started_at_ns.saturating_sub(300 * 1_000_000_000);
        }
        // Register one of the two required kinds.
        reg.register(
            "rules-alpha".into(),
            "rules".into(),
            "0.1.0".into(),
            vec![],
            "localhost:50052".into(),
        )
        .await;
        let missing = reg.missing_required().await.expect("gate active");
        assert_eq!(missing, vec!["ml-inference".to_string()]);
    }
}

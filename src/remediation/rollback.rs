//! Rollback window tracking for AutoWithNotification executions.
//!
//! When a proposal executes under AutoWithNotification trust state, a RollbackState
//! is registered with a N-second window. The UI shows a banner; the operator can
//! trigger a rollback within the window via POST /api/approvals/{id}/rollback.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct RollbackState {
    /// ID of the RemediationProposal that was executed.
    pub proposal_id: String,
    /// ID of the Remediation node written to the graph.
    pub remediation_id: String,
    /// When the execution completed (ns since epoch).
    pub executed_at_ns: i64,
    /// Window duration in seconds. After this the rollback offer expires.
    pub window_secs: u64,
    /// JSON snapshot of what was changed — used to generate inverse steps.
    /// Format: `[{"yang_path": "...", "before_value": "...", "after_value": "..."}]`
    pub snapshot_json: String,
    pub rolled_back: bool,
}

impl RollbackState {
    pub fn is_expired(&self, now_ns: i64) -> bool {
        self.rolled_back || (now_ns - self.executed_at_ns > self.window_secs as i64 * 1_000_000_000)
    }
}

pub type SharedRollbackRegistry = Arc<RwLock<RollbackRegistry>>;

#[derive(Default)]
pub struct RollbackRegistry {
    // keyed by proposal_id
    states: HashMap<String, RollbackState>,
}

impl RollbackRegistry {
    pub fn register(&mut self, state: RollbackState) {
        self.states.insert(state.proposal_id.clone(), state);
    }

    pub fn get(&self, proposal_id: &str) -> Option<&RollbackState> {
        self.states.get(proposal_id)
    }

    pub fn active_windows(&self, now_ns: i64) -> Vec<&RollbackState> {
        self.states
            .values()
            .filter(|s| !s.is_expired(now_ns))
            .collect()
    }

    pub fn mark_rolled_back(&mut self, proposal_id: &str) {
        if let Some(s) = self.states.get_mut(proposal_id) {
            s.rolled_back = true;
        }
    }

    /// Drop expired entries to keep memory bounded.
    pub fn prune(&mut self, now_ns: i64) {
        self.states.retain(|_, s| !s.is_expired(now_ns));
    }
}

pub fn new_rollback_registry() -> SharedRollbackRegistry {
    Arc::new(RwLock::new(RollbackRegistry::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(proposal_id: &str, executed_at_ns: i64, window_secs: u64) -> RollbackState {
        RollbackState {
            proposal_id: proposal_id.to_string(),
            remediation_id: format!("rem-{proposal_id}"),
            executed_at_ns,
            window_secs,
            snapshot_json: "[]".to_string(),
            rolled_back: false,
        }
    }

    const WINDOW: u64 = 60; // 60s window for most tests
    const NS: i64 = 1_000_000_000;

    #[test]
    fn not_expired_within_window() {
        let state = make_state("p1", 0, WINDOW);
        // 30 seconds later — still within 60s window
        assert!(!state.is_expired(30 * NS));
    }

    #[test]
    fn expired_after_window() {
        let state = make_state("p1", 0, WINDOW);
        // 61 seconds later — past the 60s window
        assert!(state.is_expired(61 * NS));
    }

    #[test]
    fn rolled_back_flag_causes_immediate_expiry() {
        let mut state = make_state("p1", 0, WINDOW);
        state.rolled_back = true;
        // Even within the window it is "expired" (consumed)
        assert!(state.is_expired(1 * NS));
    }

    #[test]
    fn register_and_get() {
        let mut reg = RollbackRegistry::default();
        reg.register(make_state("p1", 0, WINDOW));
        let s = reg.get("p1").unwrap();
        assert_eq!(s.proposal_id, "p1");
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn active_windows_excludes_expired() {
        let mut reg = RollbackRegistry::default();
        reg.register(make_state("fresh", 100 * NS, WINDOW));   // expires at 160s
        reg.register(make_state("expired", 0, WINDOW));        // expired at 60s

        let active = reg.active_windows(120 * NS); // now = 120s
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].proposal_id, "fresh");
    }

    #[test]
    fn mark_rolled_back_prevents_reuse() {
        let mut reg = RollbackRegistry::default();
        reg.register(make_state("p1", 0, WINDOW));
        reg.mark_rolled_back("p1");

        let active = reg.active_windows(1 * NS);
        assert!(active.is_empty());
    }

    #[test]
    fn prune_removes_expired_entries() {
        let mut reg = RollbackRegistry::default();
        reg.register(make_state("old", 0, WINDOW));
        reg.register(make_state("new", 200 * NS, WINDOW));

        reg.prune(120 * NS); // prune at t=120s: "old" (expired at 60s) is gone
        assert!(reg.get("old").is_none());
        assert!(reg.get("new").is_some());
    }
}

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

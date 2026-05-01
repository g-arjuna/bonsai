//! Human-in-the-loop graduated remediation — T3-1 through T3-6.
//!
//! Architecture:
//! - `TrustState` is per (rule_id, environment_archetype, site_id, playbook_id) tuple.
//! - `TrustStore` persists trust records to `runtime_dir/trust_state.json`.
//! - `RemediationProposal` nodes live in the graph (durable, linked to DetectionEvent).
//! - `RollbackRegistry` tracks active AutoWithNotification executions within their window.
//! - Every trust-state-affecting decision writes to the audit log (`purpose=trust_op`).

pub mod graduation;
pub mod rollback;
pub mod trust;

pub use graduation::{GraduationHint, check_graduation};
pub use rollback::{RollbackRegistry, RollbackState, SharedRollbackRegistry};
pub use trust::{SharedTrustStore, TrustKey, TrustRecord, TrustState, TrustStore};

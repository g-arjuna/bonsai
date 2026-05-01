//! Trust graduation logic — suggest when a tuple is ready to move to a less-restrictive state.
//!
//! Graduation is NEVER automatic. The system surfaces a hint; the operator decides.

use serde::Serialize;

use super::trust::{TrustRecord, TrustState};

#[derive(Debug, Clone, Serialize)]
pub struct GraduationHint {
    pub trust_key: String,
    pub from_state: String,
    pub to_state: String,
    pub reason: String,
    pub consecutive_approvals: u32,
}

/// Check whether `record` qualifies for a graduation hint.
/// Returns `Some(hint)` if the tuple has enough consecutive approvals and no recent rejections.
pub fn check_graduation(
    trust_key: &str,
    record: &TrustRecord,
    consecutive_required: u32,
) -> Option<GraduationHint> {
    // No rejections ever and sufficient consecutive successes
    if record.operator_rejections > 0 || record.consecutive_successes < consecutive_required {
        return None;
    }

    let (from, to) = match &record.state {
        TrustState::SuggestOnly if record.operator_approvals >= consecutive_required => {
            (TrustState::SuggestOnly, TrustState::ApproveEach)
        }
        TrustState::ApproveEach if record.operator_approvals >= consecutive_required => {
            (TrustState::ApproveEach, TrustState::AutoWithNotification)
        }
        _ => return None,
    };

    Some(GraduationHint {
        trust_key: trust_key.to_string(),
        from_state: from.as_str().to_string(),
        to_state: to.as_str().to_string(),
        reason: format!(
            "{} consecutive approvals, 0 rejections — ready to graduate",
            record.consecutive_successes
        ),
        consecutive_approvals: record.operator_approvals,
    })
}

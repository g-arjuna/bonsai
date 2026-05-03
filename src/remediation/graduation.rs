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

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with(
        state: TrustState,
        consecutive: u32,
        approvals: u32,
        rejections: u32,
    ) -> TrustRecord {
        TrustRecord {
            state,
            consecutive_successes: consecutive,
            operator_approvals: approvals,
            operator_rejections: rejections,
            ..Default::default()
        }
    }

    #[test]
    fn threshold_triggers_suggestion_for_approve_each() {
        let rec = record_with(TrustState::ApproveEach, 10, 10, 0);
        let hint = check_graduation("rule:env:site:pb", &rec, 10).unwrap();
        assert_eq!(hint.from_state, "approve_each");
        assert_eq!(hint.to_state, "auto_with_notification");
        assert_eq!(hint.consecutive_approvals, 10);
    }

    #[test]
    fn threshold_triggers_suggestion_for_suggest_only() {
        let rec = record_with(TrustState::SuggestOnly, 10, 10, 0);
        let hint = check_graduation("key", &rec, 10).unwrap();
        assert_eq!(hint.from_state, "suggest_only");
        assert_eq!(hint.to_state, "approve_each");
    }

    #[test]
    fn blocked_by_any_rejection() {
        let rec = record_with(TrustState::ApproveEach, 10, 10, 1);
        assert!(check_graduation("key", &rec, 10).is_none());
    }

    #[test]
    fn blocked_by_insufficient_consecutive_successes() {
        let rec = record_with(TrustState::ApproveEach, 9, 10, 0);
        assert!(check_graduation("key", &rec, 10).is_none());
    }

    #[test]
    fn auto_silent_and_auto_with_notification_never_graduate() {
        // AutoWithNotification and AutoSilent are terminal — no hint produced
        for state in [TrustState::AutoWithNotification, TrustState::AutoSilent] {
            let rec = record_with(state, 100, 100, 0);
            assert!(check_graduation("key", &rec, 10).is_none());
        }
    }

    #[test]
    fn graduation_key_preserved_in_hint() {
        let rec = record_with(TrustState::ApproveEach, 10, 10, 0);
        let hint = check_graduation("bgp-flap:data_center:dc1:restore-bgp", &rec, 10).unwrap();
        assert_eq!(hint.trust_key, "bgp-flap:data_center:dc1:restore-bgp");
    }
}

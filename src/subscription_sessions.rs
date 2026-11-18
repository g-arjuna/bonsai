//! Authoritative gNMI subscription-session registry (M5.2B).
//!
//! Device-facing subscription lifecycle is reported out-of-band from
//! telemetry (dedicated `SubscriptionLifecycle` RPC for remote
//! collectors, in-process channel for local acquisition) so that
//! coverage authority never depends on best-effort observation delivery.
//!
//! The registry owns the per-target session state machine:
//!
//! ```text
//! submitted -> synchronizing -> synchronized -> disconnected
//!     |           |                |   \-> failed
//!     +-----------+----------------+   \-> superseded
//!     \-> failed / disconnected / disabled (terminal)
//! ```
//!
//! Terminal states (`failed`, `disconnected`, `disabled`, `superseded`)
//! are final for a session identity: a terminated session is never
//! resurrected by a delayed transition, and a reconnect mints a NEW
//! `session_id` rather than reviving the old one. Only a `synchronized`
//! session with verified provenance may authorize current coverage.

use std::collections::HashMap;
use std::sync::Mutex;

/// Lifecycle states carried on `SubscriptionLifecycleEvent.state`.
pub mod state {
    pub const DESIRED: &str = "desired";
    pub const VALIDATED: &str = "validated";
    pub const CONNECTING: &str = "connecting";
    pub const SUBMITTED: &str = "submitted";
    pub const SYNCHRONIZING: &str = "synchronizing";
    pub const SYNCHRONIZED: &str = "synchronized";
    pub const FAILED: &str = "failed";
    pub const DISCONNECTED: &str = "disconnected";
    pub const DISABLED: &str = "disabled";
    pub const SUPERSEDED: &str = "superseded";
}

/// States after which a session identity can never transition again.
/// `pub(crate)` so the subscription verifier can expire pending
/// observation buffers for sessions that can never authorize coverage.
pub(crate) fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        state::FAILED | state::DISCONNECTED | state::DISABLED | state::SUPERSEDED
    )
}

/// Whether `next` is a legal forward transition from `current`.
/// Same-state re-emission is handled by the caller as an idempotent
/// refresh, not a transition. The table is strictly forward along the
/// device-facing chain — a synchronized session can never regress to
/// submitted/synchronizing under the same session identity (a real
/// resubmission mints a NEW session_id), and pre-submission states are
/// unreachable once submission has occurred.
fn transition_allowed(current: &str, next: &str) -> bool {
    use state::*;
    if current == next || is_terminal(current) {
        return false;
    }
    match next {
        VALIDATED => current == DESIRED,
        CONNECTING => matches!(current, DESIRED | VALIDATED),
        SUBMITTED => matches!(current, DESIRED | VALIDATED | CONNECTING),
        // gNMI emits exactly one SyncResponse per SubscriptionList; a
        // lost `synchronizing` transition must not deadlock the session,
        // so synchronization may follow submission directly.
        SYNCHRONIZING => current == SUBMITTED,
        SYNCHRONIZED => matches!(current, SUBMITTED | SYNCHRONIZING),
        // Terminal outcomes are reachable from any live state.
        FAILED | DISCONNECTED | DISABLED | SUPERSEDED => true,
        // `desired` is only an initial state — never a transition target.
        DESIRED => false,
        _ => false,
    }
}

/// Maximum session records retained per target. Terminal records are
/// evicted oldest-first once the bound is exceeded; the current
/// coverage session and every non-terminal record are never evicted.
const MAX_SESSIONS_PER_TARGET: usize = 64;

/// One path entry in the actual submitted Subscribe manifest carried
/// on `submitted` lifecycle events (M5.2B review B3). This is the
/// collector-attested effective path set — the core binds coverage
/// expectations to exactly these entries, never to a core-side
/// fallback prediction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecyclePathEntry {
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
}

/// One device-facing lifecycle transition, already stripped of transport
/// concerns. `collector_id` is empty for trusted in-process (local core)
/// producers; remote events carry the authenticated collector id.
#[derive(Clone, Debug)]
pub struct SessionLifecycleEvent {
    pub collector_id: String,
    pub target: String,
    pub plan_id: String,
    pub session_id: String,
    pub state: String,
    pub transition_seq: u64,
    pub authority_epoch: u64,
    pub assignment_generation: u64,
    pub occurred_at_ns: i64,
    pub detail: String,
    /// Actual submitted Subscribe manifest — populated on `submitted`
    /// and reaffirmed on `synchronized`; empty for plan-level outcomes
    /// and legacy producers.
    pub submitted_paths: Vec<LifecyclePathEntry>,
    /// Acquisition intent the manifest was built under
    /// ("automatic" | "explicit" | "disabled"); empty on legacy records.
    pub acquisition: String,
}

/// Outcome of applying one lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// State transition recorded.
    Applied,
    /// Exact duplicate (same session key, seq and state) — idempotent.
    Duplicate,
    /// Older/equal transition revision — ignored, cannot resurrect or
    /// regress a session.
    Stale,
    /// Transition violates the state machine — recorded as an invalid
    /// transition metric/log; session state is unchanged.
    InvalidTransition,
    /// Malformed event (unknown state, missing identity).
    Rejected(&'static str),
}

#[derive(Clone, Debug)]
struct SessionRecord {
    plan_id: String,
    session_id: String,
    state: String,
    transition_seq: u64,
    collector_id: String,
    /// Verified = produced by an authenticated collector bound to the
    /// device assignment, or by the trusted local core path. Only
    /// verified synchronized sessions authorize coverage.
    verified: bool,
    detail: String,
    /// Last transition time; retained for status surfacing and stale
    /// diagnostics (read by tests and future status APIs).
    #[allow(dead_code)]
    updated_at_ns: i64,
}

#[derive(Default)]
struct TargetSessions {
    /// session key -> record. Key is the session_id, or `plan:<id>` for
    /// session-less plan-level outcomes (disabled/rejected/failed plan).
    sessions: HashMap<String, SessionRecord>,
    /// Session key currently authorized to satisfy coverage, if any.
    current: Option<String>,
}

/// Process-local authoritative session registry. Remote collectors feed
/// it through the authenticated `SubscriptionLifecycle` RPC; local
/// acquisition feeds it in-process. Restart-safe by construction: the
/// registry is empty on boot, so persisted/historical session state can
/// never silently become live again — new sessions must re-register.
#[derive(Default)]
pub struct SubscriptionSessionRegistry {
    inner: Mutex<HashMap<String, TargetSessions>>,
}

impl SubscriptionSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn session_key(session_id: &str, plan_id: &str) -> String {
        if session_id.is_empty() {
            format!("plan:{plan_id}")
        } else {
            session_id.to_string()
        }
    }

    /// Apply one lifecycle event.
    ///
    /// `verified` must only be true for authenticated, assignment-bound
    /// producers (or the trusted local path); callers enforce that before
    /// invoking this.
    pub fn apply(&self, event: &SessionLifecycleEvent, verified: bool) -> ApplyOutcome {
        use state::*;
        if event.target.is_empty() || event.plan_id.is_empty() {
            return ApplyOutcome::Rejected("missing target/plan identity");
        }
        if !matches!(
            event.state.as_str(),
            DESIRED
                | VALIDATED
                | CONNECTING
                | SUBMITTED
                | SYNCHRONIZING
                | SYNCHRONIZED
                | FAILED
                | DISCONNECTED
                | DISABLED
                | SUPERSEDED
        ) {
            return ApplyOutcome::Rejected("unknown lifecycle state");
        }
        // Coverage-granting states require a real session identity;
        // plan-level outcomes (empty session_id) may only be terminal or
        // pre-submission states.
        if event.session_id.is_empty()
            && matches!(
                event.state.as_str(),
                SUBMITTED | SYNCHRONIZING | SYNCHRONIZED
            )
        {
            return ApplyOutcome::Rejected("session state without session_id");
        }

        let key = Self::session_key(&event.session_id, &event.plan_id);
        let mut targets = self.inner.lock().unwrap();
        let entry = targets.entry(event.target.clone()).or_default();

        let existing = entry.sessions.get(&key).cloned();
        if let Some(record) = &existing {
            // Ordered transitions: seq must strictly increase. seq == 0
            // is the unordered plan-level channel (disabled/rejected
            // outcomes) — latest-wins, never resurrects terminal.
            if event.transition_seq != 0 {
                if event.transition_seq < record.transition_seq {
                    return ApplyOutcome::Stale;
                }
                if event.transition_seq == record.transition_seq {
                    return if record.state == event.state {
                        ApplyOutcome::Duplicate
                    } else {
                        ApplyOutcome::InvalidTransition
                    };
                }
            } else if is_terminal(&record.state) {
                // Unordered event against a terminated session: only a
                // DIFFERENT terminal outcome may replace the record
                // (e.g. failed -> disabled on assignment withdrawal);
                // it can never reopen the session.
                if !is_terminal(&event.state) {
                    return ApplyOutcome::Stale;
                }
            }
            // Terminal→terminal refinements are allowed: a superseded
            // session's real disconnect/failure outcome is still worth
            // recording and can never affect a different (newer) key.
            if event.state != record.state
                && !(is_terminal(&record.state) && is_terminal(&event.state))
                && !transition_allowed(&record.state, &event.state)
            {
                return ApplyOutcome::InvalidTransition;
            }
            // Session identity is immutable once registered: a later
            // event under the same session key can never re-attribute
            // the session to a different collector or plan. Authority
            // fields (epoch/generation) may legitimately advance within
            // the assignment window across confirming resyncs.
            if record.collector_id != event.collector_id || record.plan_id != event.plan_id {
                return ApplyOutcome::Rejected("session identity mutation");
            }
            // An unverified event can never modify a verified session —
            // verified provenance is sticky and only ever strengthened
            // by equally-verified producers.
            if record.verified && !verified {
                return ApplyOutcome::InvalidTransition;
            }
        } else if is_terminal(&event.state) && event.session_id.is_empty() {
            // A terminal plan-level event for a plan that never
            // registered is still meaningful (disabled before submit).
        }

        // A plan-level terminal outcome (disabled/failed acquisition)
        // retires every live session of that plan for the target: the
        // core-side decision is authoritative and must not depend on
        // terminal messages from the collector.
        if event.session_id.is_empty() && matches!(event.state.as_str(), DISABLED | FAILED) {
            for (other_key, other) in entry.sessions.iter_mut() {
                if other.plan_id == event.plan_id && !is_terminal(&other.state) {
                    other.state = SUPERSEDED.to_string();
                    other.detail = event.detail.clone();
                    if entry.current.as_deref() == Some(other_key.as_str()) {
                        entry.current = None;
                    }
                }
            }
        }

        let record = SessionRecord {
            plan_id: event.plan_id.clone(),
            session_id: event.session_id.clone(),
            state: event.state.clone(),
            transition_seq: event
                .transition_seq
                .max(existing.as_ref().map(|r| r.transition_seq).unwrap_or(0)),
            collector_id: event.collector_id.clone(),
            verified: verified || existing.as_ref().is_some_and(|r| r.verified),
            detail: event.detail.clone(),
            updated_at_ns: event.occurred_at_ns,
        };

        // A session that (re)enters the device-facing lifecycle
        // supersedes every sibling session for the same target: one
        // collector owns one device-facing session at a time, and old
        // session identities must not retain authority.
        let entering_live = matches!(
            event.state.as_str(),
            SUBMITTED | SYNCHRONIZING | SYNCHRONIZED
        );
        if entering_live {
            for (other_key, other) in entry.sessions.iter_mut() {
                if *other_key != key && !is_terminal(&other.state) {
                    other.state = SUPERSEDED.to_string();
                    if entry.current.as_deref() == Some(other_key.as_str()) {
                        entry.current = None;
                    }
                }
            }
        }

        let became_current = event.state == SYNCHRONIZED && verified;
        entry.sessions.insert(key.clone(), record);
        if became_current {
            entry.current = Some(key);
        } else if entry.current.as_deref() == Some(key.as_str())
            && (is_terminal(&event.state) || !verified)
        {
            entry.current = None;
        }
        Self::evict_bounded(entry);
        ApplyOutcome::Applied
    }

    /// Bound per-target session retention. Terminal records are
    /// evicted oldest-first; the current coverage session and every
    /// non-terminal record are never evicted, so eviction can never
    /// destroy live authority. Anti-replay is preserved: an evicted
    /// session's late events re-register as new records and can never
    /// impersonate a retained newer session (identity pinning above).
    fn evict_bounded(entry: &mut TargetSessions) {
        while entry.sessions.len() > MAX_SESSIONS_PER_TARGET {
            let victim = entry
                .sessions
                .iter()
                .filter(|(k, r)| {
                    is_terminal(&r.state) && entry.current.as_deref() != Some(k.as_str())
                })
                .min_by_key(|(_, r)| r.updated_at_ns)
                .map(|(k, _)| k.clone());
            let Some(key) = victim else { break };
            entry.sessions.remove(&key);
            metrics::counter!("bonsai_subscription_session_evicted_total").increment(1);
        }
    }

    /// Current coverage-authorized session for `target`:
    /// `(plan_id, session_id)` of the synchronized, verified session,
    /// or `None` when no session may satisfy coverage.
    pub fn current_session(&self, target: &str) -> Option<(String, String)> {
        self.current_session_provenance(target)
            .map(|(plan_id, session_id, _)| (plan_id, session_id))
    }

    /// Current coverage-authorized session INCLUDING the authenticated
    /// collector identity that owns it (empty for trusted local
    /// acquisition). Coverage evidence must match all three: an
    /// observation stamped with a different collector can never satisfy
    /// this session (M5.2B review B1).
    pub fn current_session_provenance(&self, target: &str) -> Option<(String, String, String)> {
        let targets = self.inner.lock().unwrap();
        let entry = targets.get(target)?;
        let key = entry.current.as_ref()?;
        let record = entry.sessions.get(key)?;
        (record.state == state::SYNCHRONIZED && record.verified).then(|| {
            (
                record.plan_id.clone(),
                record.session_id.clone(),
                record.collector_id.clone(),
            )
        })
    }

    /// State of a specific session for status reporting.
    pub fn session_state(&self, target: &str, session_id: &str) -> Option<(String, String, bool)> {
        let targets = self.inner.lock().unwrap();
        let entry = targets.get(target)?;
        let key = Self::session_key(session_id, "");
        // session_key with empty plan_id yields "session_id" only when
        // session_id non-empty; look up directly.
        let record = entry
            .sessions
            .get(session_id)
            .or_else(|| entry.sessions.get(&key))?;
        Some((
            record.state.clone(),
            record.plan_id.clone(),
            record.verified,
        ))
    }

    /// Retire every session attributed to `collector_id` (collector
    /// disconnect/reassignment): current authority ends immediately —
    /// delayed telemetry from those sessions stays historical.
    pub fn retire_collector(&self, collector_id: &str) {
        let mut targets = self.inner.lock().unwrap();
        for entry in targets.values_mut() {
            for record in entry.sessions.values_mut() {
                if record.collector_id == collector_id && !is_terminal(&record.state) {
                    record.state = state::DISCONNECTED.to_string();
                }
            }
            if let Some(current) = &entry.current {
                let still_current = entry
                    .sessions
                    .get(current)
                    .is_some_and(|r| r.state == state::SYNCHRONIZED && r.verified);
                if !still_current {
                    entry.current = None;
                }
            }
        }
    }

    /// Retire all sessions for `target` (assignment withdrawal/disable
    /// observed core-side). Marks non-terminal sessions superseded.
    pub fn retire_target(&self, target: &str, detail: &str) {
        let mut targets = self.inner.lock().unwrap();
        if let Some(entry) = targets.get_mut(target) {
            for record in entry.sessions.values_mut() {
                if !is_terminal(&record.state) {
                    record.state = state::SUPERSEDED.to_string();
                    record.detail = detail.to_string();
                }
            }
            entry.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(state: &str, seq: u64, session: &str) -> SessionLifecycleEvent {
        SessionLifecycleEvent {
            collector_id: "c1".to_string(),
            target: "dut".to_string(),
            plan_id: "plan".to_string(),
            session_id: session.to_string(),
            state: state.to_string(),
            transition_seq: seq,
            authority_epoch: 1,
            assignment_generation: 1,
            occurred_at_ns: 0,
            detail: String::new(),
            submitted_paths: Vec::new(),
            acquisition: String::new(),
        }
    }

    #[test]
    fn synchronized_session_becomes_current_and_duplicate_is_idempotent() {
        let reg = SubscriptionSessionRegistry::new();
        assert_eq!(
            reg.apply(&ev(state::SUBMITTED, 1, "s1"), true),
            ApplyOutcome::Applied
        );
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZING, 2, "s1"), true),
            ApplyOutcome::Applied
        );
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 3, "s1"), true),
            ApplyOutcome::Applied
        );
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "s1".to_string()))
        );
        // Duplicate transition is idempotent.
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 3, "s1"), true),
            ApplyOutcome::Duplicate
        );
        // Stale transition cannot regress.
        assert_eq!(
            reg.apply(&ev(state::SUBMITTED, 2, "s1"), true),
            ApplyOutcome::Stale
        );
    }

    #[test]
    fn unverified_session_never_authorizes_coverage() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), false);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s1"), false);
        assert_eq!(reg.current_session("dut"), None);
    }

    #[test]
    fn terminal_session_cannot_be_resurrected() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), true);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s1"), true);
        reg.apply(&ev(state::DISCONNECTED, 3, "s1"), true);
        assert_eq!(reg.current_session("dut"), None);
        // Delayed forward transition on a terminated session: invalid.
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 4, "s1"), true),
            ApplyOutcome::InvalidTransition
        );
        assert_eq!(reg.current_session("dut"), None);
    }

    #[test]
    fn reconnect_mints_fresh_session_and_supersedes_old() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), true);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s1"), true);
        // Same plan, new session identity.
        reg.apply(&ev(state::SUBMITTED, 1, "s2"), true);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s2"), true);
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "s2".to_string()))
        );
        // Old session terminal event cannot disturb the new session.
        assert_eq!(
            reg.apply(&ev(state::DISCONNECTED, 3, "s1"), true),
            ApplyOutcome::Applied
        );
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "s2".to_string()))
        );
    }

    #[test]
    fn retire_collector_revokes_current_authority() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), true);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s1"), true);
        reg.retire_collector("c1");
        assert_eq!(reg.current_session("dut"), None);
    }

    #[test]
    fn updates_before_sync_do_not_imply_synchronization() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), true);
        // submitted alone never authorizes coverage.
        assert_eq!(reg.current_session("dut"), None);
        reg.apply(&ev(state::SYNCHRONIZING, 2, "s1"), true);
        assert_eq!(reg.current_session("dut"), None);
    }
}

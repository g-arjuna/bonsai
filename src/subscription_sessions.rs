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

/// Deterministic execution-order fence (M5.2B review): every
/// device-facing execution is totally ordered by
/// `(authority_epoch, authority_instance, collector_session_generation,
/// execution_generation)` — all core-issued or collector-echoed
/// monotonic counters, never wall-clock and never probabilistic.
///
/// - `authority_epoch` / `authority_instance`: the core-assigned
///   assignment-authority window (injected core-side at the
///   authenticated RPC boundary; `authority_instance` orders
///   reassignment across collectors).
/// - `collector_session_generation`: core-issued per-collector
///   registration counter, echoed by the collector; orders executions
///   across collector restarts and assignment-stream reconnects.
/// - `execution_generation`: per-(collector session, target) counter
///   minted by the subscriber each time it creates a session_id.
///
/// The fence state per target is O(1): the high-water rank, the single
/// session identity bound to it, and the bounded session history —
/// independent of reconnect count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExecRank {
    epoch: u64,
    instance: u64,
    session_gen: u64,
    exec_gen: u64,
}

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
    /// Core-issued authority-instance ordinal for the device's current
    /// assignment window — injected by the authenticated core boundary,
    /// never trusted from the wire. 0 for trusted local acquisition.
    pub authority_instance: u64,
    /// Core-issued collector session generation echoed by the
    /// collector (0 for local acquisition / legacy producers).
    pub collector_session_generation: u64,
    /// Per-(collector session, target) monotonic execution counter
    /// minted by the subscriber per session_id (0 = absent).
    pub execution_generation: u64,
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

/// Relation of an event's execution rank to the target high-water mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecRelation {
    /// Plan-level outcome — no execution identity.
    PlanLevel,
    /// Rank above the high-water mark: a NEW execution. Admitted and,
    /// once applied under verified provenance, becomes the fence head.
    New,
    /// Rank equal to the high-water mark with the bound session
    /// identity: the CURRENT execution — normal ordered transitions.
    Current,
    /// Rank below the high-water mark: a superseded execution. Its
    /// retained session records may still accept diagnostic transitions
    /// but it can never hold or regain current authority.
    Old,
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
    /// Execution rank this session was registered under (`None` for
    /// plan-level records). A session identity can never change
    /// execution — a later event claiming a different rank under the
    /// same session_id is an identity mutation.
    exec: Option<ExecRank>,
}

struct TargetSessions {
    /// session key -> record. Key is the session_id, or `plan:<id>` for
    /// session-less plan-level outcomes (disabled/rejected/failed plan).
    sessions: HashMap<String, SessionRecord>,
    /// Session key currently authorized to satisfy coverage, if any.
    current: Option<String>,
    /// Highest execution rank admitted for this target — the
    /// deterministic execution-order high-water mark. Only an
    /// execution at this rank may hold current coverage authority;
    /// every lower rank is permanently historical regardless of
    /// session-record retention (O(1) memory, no reconnect growth).
    exec_hwm: Option<ExecRank>,
    /// Session identity bound to `exec_hwm`. One execution owns exactly
    /// one session_id: a different UUID presenting the same rank is an
    /// identity conflict, and the bound session's terminal state closes
    /// the execution for authority purposes.
    exec_session: Option<String>,
}

impl Default for TargetSessions {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            current: None,
            exec_hwm: None,
            exec_session: None,
        }
    }
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

        // Deterministic execution-order fence (M5.2B): session-scoped
        // events must carry an execution identity and are ordered
        // against the per-target high-water mark. This replaces the
        // retired-identity tombstone/Bloom fence: ordering is exact,
        // bounded (O(1) per target) and independent of reconnect count.
        let session_scoped = !event.session_id.is_empty();
        let mut exec_relation = ExecRelation::PlanLevel;
        let mut rank = ExecRank {
            epoch: 0,
            instance: 0,
            session_gen: 0,
            exec_gen: 0,
        };
        if session_scoped {
            if event.execution_generation == 0 {
                metrics::counter!("bonsai_subscription_session_replay_rejected_total").increment(1);
                return ApplyOutcome::Rejected("missing execution identity");
            }
            rank = ExecRank {
                epoch: event.authority_epoch,
                instance: event.authority_instance,
                session_gen: event.collector_session_generation,
                exec_gen: event.execution_generation,
            };
            exec_relation = match entry.exec_hwm {
                None => ExecRelation::New,
                Some(hwm) if rank > hwm => ExecRelation::New,
                Some(hwm) if rank == hwm => {
                    // One execution owns exactly one session identity:
                    // a different UUID under the current generation is
                    // an identity conflict, never a new execution.
                    if entry.exec_session.as_deref() != Some(event.session_id.as_str()) {
                        metrics::counter!("bonsai_subscription_session_replay_rejected_total")
                            .increment(1);
                        return ApplyOutcome::Rejected("execution identity conflict");
                    }
                    // The bound session reaching a terminal state (or
                    // collector/target retirement) closes the
                    // execution: delayed transitions for it are
                    // replays, not authority.
                    if !Self::exec_is_open(entry) {
                        metrics::counter!("bonsai_subscription_session_replay_rejected_total")
                            .increment(1);
                        return ApplyOutcome::Rejected("retired execution replay");
                    }
                    ExecRelation::Current
                }
                Some(_) => ExecRelation::Old,
            };
            if exec_relation == ExecRelation::Old && existing.is_none() {
                // A delayed event for an older execution the core never
                // registered cannot re-register: bounded memory AND no
                // path back to authority.
                metrics::counter!("bonsai_subscription_session_replay_rejected_total").increment(1);
                return ApplyOutcome::Rejected("superseded execution");
            }
            if let Some(record) = &existing
                && record.exec != Some(rank)
            {
                return ApplyOutcome::Rejected("execution identity mutation");
            }
        }
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
            exec: session_scoped.then_some(rank),
        };

        // A session that (re)enters the device-facing lifecycle
        // supersedes every sibling session for the same target: one
        // collector owns one device-facing session at a time, and old
        // session identities must not retain authority. Only VERIFIED
        // producers may disturb sibling sessions — an unverified
        // higher-ranked event must never supersede a legitimate
        // verified current session (M5.2B review B2).
        let entering_live = matches!(
            event.state.as_str(),
            SUBMITTED | SYNCHRONIZING | SYNCHRONIZED
        );
        // A superseded execution's late live-state events are history
        // only: they must never supersede the current execution's
        // sessions or disturb current authority.
        if entering_live && verified && exec_relation != ExecRelation::Old {
            for (other_key, other) in entry.sessions.iter_mut() {
                if *other_key != key && !is_terminal(&other.state) {
                    other.state = SUPERSEDED.to_string();
                    if entry.current.as_deref() == Some(other_key.as_str()) {
                        entry.current = None;
                    }
                }
            }
        }

        // Only the fence-head execution may hold current coverage
        // authority; superseded executions are recorded for diagnostics
        // but can never become current regardless of record retention.
        let became_current =
            event.state == SYNCHRONIZED && verified && exec_relation != ExecRelation::Old;
        entry.sessions.insert(key.clone(), record);
        // Commit the high-water advance only for verified admissions:
        // an unverified producer can record diagnostics but must never
        // fence out the legitimate current execution.
        if exec_relation == ExecRelation::New && verified {
            entry.exec_hwm = Some(rank);
            entry.exec_session = Some(event.session_id.clone());
        }
        if became_current {
            entry.current = Some(key);
        } else if entry.current.as_deref() == Some(key.as_str())
            && (is_terminal(&event.state) || !verified)
        {
            entry.current = None;
        }
        // Current authority implies fence-head rank (M5.2B review B2):
        // whenever the high-water mark advanced — including a NEW
        // execution whose FIRST received event is already terminal —
        // any current session bound to a lower rank is obsolete and
        // must be invalidated immediately. It can never remain current
        // or regain authority.
        if let (Some(hwm), Some(cur)) = (entry.exec_hwm, entry.current.clone()) {
            let cur_rank = entry.sessions.get(&cur).and_then(|r| r.exec);
            if cur_rank != Some(hwm) {
                entry.current = None;
            }
        }
        Self::evict_bounded(entry);
        ApplyOutcome::Applied
    }

    /// Whether the execution bound to the high-water mark is still
    /// open: its session record exists and has not reached a terminal
    /// state. Terminal transitions (including collector/target
    /// retirement, which marks records directly) close the execution
    /// for authority — later events under the same rank are replays.
    fn exec_is_open(entry: &TargetSessions) -> bool {
        entry.exec_session.as_ref().is_some_and(|sid| {
            entry
                .sessions
                .get(sid)
                .is_some_and(|r| !is_terminal(&r.state))
        })
    }

    /// Bound per-target session retention. Terminal records are
    /// evicted oldest-first; the current coverage session and every
    /// non-terminal record are never evicted, so eviction can never
    /// destroy live authority. Replay safety no longer depends on
    /// retention at all: the execution high-water mark fences every
    /// superseded execution deterministically (O(1) per target).
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
        // Defense in depth: current authority is only ever the
        // fence-head execution. `apply` maintains this invariant, but
        // the accessor re-checks it so no record whose execution rank
        // differs from the authoritative high-water mark can ever be
        // exposed as current coverage (M5.2B review B3).
        (record.state == state::SYNCHRONIZED
            && record.verified
            && record.exec.is_some()
            && record.exec == entry.exec_hwm)
            .then(|| {
                (
                    record.plan_id.clone(),
                    record.session_id.clone(),
                    record.collector_id.clone(),
                )
            })
    }

    /// Identity `(session_id, plan_id)` bound to the target's execution
    /// high-water mark — the newest admitted execution — while that
    /// execution is still open (its record exists and has not reached a
    /// terminal state). `None` when no execution has been admitted or
    /// the head execution has closed. The coverage verifier uses this to
    /// fence delayed plan manifests and stale desired-plan previews:
    /// only the open fence-head execution may own registered
    /// expectations.
    pub fn exec_head(&self, target: &str) -> Option<(String, String)> {
        let targets = self.inner.lock().unwrap();
        let entry = targets.get(target)?;
        if !Self::exec_is_open(entry) {
            return None;
        }
        let sid = entry.exec_session.clone()?;
        let plan_id = entry.sessions.get(&sid)?.plan_id.clone();
        Some((sid, plan_id))
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

    fn evx(state: &str, seq: u64, session: &str, exec_gen: u64) -> SessionLifecycleEvent {
        SessionLifecycleEvent {
            collector_id: "c1".to_string(),
            target: "dut".to_string(),
            plan_id: "plan".to_string(),
            session_id: session.to_string(),
            state: state.to_string(),
            transition_seq: seq,
            authority_epoch: 1,
            assignment_generation: 1,
            authority_instance: 1,
            collector_session_generation: 1,
            execution_generation: exec_gen,
            occurred_at_ns: 0,
            detail: String::new(),
            submitted_paths: Vec::new(),
            acquisition: String::new(),
        }
    }

    fn ev(state: &str, seq: u64, session: &str) -> SessionLifecycleEvent {
        evx(state, seq, session, 1)
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
        // Delayed forward transition on a terminated session: the
        // execution fence rejects it as a retired-execution replay
        // before the state machine is even consulted.
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 4, "s1"), true),
            ApplyOutcome::Rejected("retired execution replay")
        );
        assert_eq!(reg.current_session("dut"), None);
    }

    #[test]
    fn reconnect_mints_fresh_session_and_supersedes_old() {
        let reg = SubscriptionSessionRegistry::new();
        reg.apply(&ev(state::SUBMITTED, 1, "s1"), true);
        reg.apply(&ev(state::SYNCHRONIZED, 2, "s1"), true);
        // Same plan, new session identity under a NEW execution.
        reg.apply(&evx(state::SUBMITTED, 1, "s2", 2), true);
        reg.apply(&evx(state::SYNCHRONIZED, 2, "s2", 2), true);
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

    /// M5.2B review C: the deterministic execution-order fence — a
    /// superseded execution can never regain authority regardless of
    /// session-record retention, and bounded memory is O(1) per target.
    #[test]
    fn superseded_execution_cannot_replay_into_authority() {
        // `ev_at` stamps occurred_at so eviction (oldest-terminal-first)
        // is deterministic: sess-a/b carry the oldest timestamps.
        let ev_at = |state: &str, seq: u64, session: &str, exec: u64, ts: i64| {
            let mut e = evx(state, seq, session, exec);
            e.occurred_at_ns = ts;
            e
        };
        let reg = SubscriptionSessionRegistry::new();
        // Session A establishes and is superseded by execution 2.
        reg.apply(&ev_at(state::SUBMITTED, 1, "sess-a", 1, 1), true);
        reg.apply(&ev_at(state::SYNCHRONIZED, 2, "sess-a", 1, 2), true);
        reg.apply(&ev_at(state::SUBMITTED, 1, "sess-b", 2, 3), true);
        reg.apply(&ev_at(state::SYNCHRONIZED, 2, "sess-b", 2, 4), true);
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "sess-b".to_string()))
        );

        // Churn past BOTH bounds: MAX_SESSIONS_PER_TARGET = 64 records.
        // sess-a's record is evicted entirely — only the execution
        // high-water mark can still fence its replay.
        for i in 0..340u64 {
            let sid = format!("churn-{i}");
            let ts = 10 + i as i64;
            let exec = 3 + i;
            assert_eq!(
                reg.apply(&ev_at(state::SUBMITTED, 1, &sid, exec, ts), true),
                ApplyOutcome::Applied
            );
            assert_eq!(
                reg.apply(&ev_at(state::SYNCHRONIZED, 2, &sid, exec, ts + 1), true),
                ApplyOutcome::Applied
            );
            assert_eq!(
                reg.apply(&ev_at(state::DISCONNECTED, 3, &sid, exec, ts + 2), true),
                ApplyOutcome::Applied
            );
        }
        // Current session Z under the newest execution.
        reg.apply(&ev_at(state::SUBMITTED, 1, "sess-z", 400, 10_000), true);
        reg.apply(&ev_at(state::SYNCHRONIZED, 2, "sess-z", 400, 10_001), true);
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "sess-z".to_string()))
        );

        {
            let targets = reg.inner.lock().unwrap();
            let entry = targets.get("dut").expect("target entry");
            assert!(entry.sessions.len() <= MAX_SESSIONS_PER_TARGET);
            // sess-a's record is gone; the fence is the exec high-water
            // mark, not any retained identity.
            assert!(entry.sessions.get("sess-a").is_none());
            assert_eq!(
                entry.exec_hwm,
                Some(ExecRank {
                    epoch: 1,
                    instance: 1,
                    session_gen: 1,
                    exec_gen: 400,
                })
            );
        }

        // Delayed replay of evicted sess-a's synchronized transition:
        // its execution rank is below the high-water mark and its
        // record is gone — rejected, sess-z remains current.
        let replay = ev_at(state::SYNCHRONIZED, 2, "sess-a", 1, 2);
        assert_eq!(
            reg.apply(&replay, true),
            ApplyOutcome::Rejected("superseded execution")
        );
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "sess-z".to_string()))
        );
        // A never-registered execution BELOW the high-water mark is
        // equally fenced — delayed first-seen evidence cannot
        // supersede the newer execution.
        let mut ghost = ev_at(state::SYNCHRONIZED, 2, "ghost-old", 250, 10_002);
        ghost.transition_seq = 2;
        assert_eq!(
            reg.apply(&ghost, true),
            ApplyOutcome::Rejected("superseded execution")
        );
        // A conflicting UUID under the CURRENT execution generation is
        // an identity conflict, not a new execution.
        let conflict = ev_at(state::SYNCHRONIZED, 2, "impostor", 400, 10_003);
        assert_eq!(
            reg.apply(&conflict, true),
            ApplyOutcome::Rejected("execution identity conflict")
        );
        // The current execution itself remains authoritative and
        // idempotent.
        assert_eq!(
            reg.apply(&ev_at(state::SYNCHRONIZED, 2, "sess-z", 400, 10_004), true),
            ApplyOutcome::Duplicate
        );
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "sess-z".to_string()))
        );
    }

    /// Availability proof (M5.2B review B): 20,000 sequential
    /// legitimate executions under UNCHANGED assignment authority must
    /// every one be admitted — deterministic fencing has no false
    /// positives. Memory stays bounded by the session-record cap, not
    /// by execution count.
    #[test]
    fn twenty_thousand_sequential_executions_all_admitted() {
        let reg = SubscriptionSessionRegistry::new();
        // Deterministic eviction order: occurred_at tracks the
        // execution generation so oldest-terminal-first eviction is
        // exact, not hash-order dependent.
        let ev = |state: &str, seq: u64, session: &str, exec: u64| {
            let mut e = evx(state, seq, session, exec);
            e.occurred_at_ns = exec as i64;
            e
        };
        // Exactly 20,000 executions; generation 19,999 is deliberately
        // skipped so the replay battery can present a delayed event for
        // an execution the core NEVER registered below the high-water.
        for i in 1..=20_001u64 {
            if i == 19_999 {
                continue;
            }
            let sid = format!("exec-{i}");
            // SUBMITTED accepted.
            assert_eq!(
                reg.apply(&ev(state::SUBMITTED, 1, &sid, i), true),
                ApplyOutcome::Applied,
                "execution {i} submitted rejected"
            );
            assert_eq!(reg.current_session("dut"), None, "submitted never current");
            // SYNCHRONIZED accepted and becomes current.
            assert_eq!(
                reg.apply(&ev(state::SYNCHRONIZING, 2, &sid, i), true),
                ApplyOutcome::Applied,
                "execution {i} synchronizing rejected"
            );
            assert_eq!(
                reg.apply(&ev(state::SYNCHRONIZED, 3, &sid, i), true),
                ApplyOutcome::Applied,
                "execution {i} synchronized rejected"
            );
            assert_eq!(
                reg.current_session("dut"),
                Some(("plan".to_string(), sid.clone())),
                "execution {i} did not become current"
            );
            // Terminal retirement behaves correctly: disconnect ends
            // authority; the NEXT execution supersedes anyway.
            if i % 2 == 0 && i != 20_001 {
                assert_eq!(
                    reg.apply(&ev(state::DISCONNECTED, 4, &sid, i), true),
                    ApplyOutcome::Applied,
                    "execution {i} disconnect rejected"
                );
                assert_eq!(reg.current_session("dut"), None);
            }
            // Bounded memory: session records capped, fence O(1).
            let targets = reg.inner.lock().unwrap();
            let entry = targets.get("dut").expect("target entry");
            assert!(entry.sessions.len() <= MAX_SESSIONS_PER_TARGET);
            drop(targets);
        }

        // Replay battery against the final state (execution 20001,
        // synchronized at seq 3 — it is current).
        let newest = "exec-20001".to_string();
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), newest.clone()))
        );
        // Oldest execution replay (record long evicted).
        assert!(matches!(
            reg.apply(&ev(state::SYNCHRONIZED, 3, "exec-1", 1), true),
            ApplyOutcome::Rejected(_)
        ));
        // An execution whose history was evicted (exec-500 — far
        // outside the 64-record retention window).
        assert!(matches!(
            reg.apply(&ev(state::SYNCHRONIZED, 3, "exec-500", 500), true),
            ApplyOutcome::Rejected(_)
        ));
        // Delayed older execution NEVER previously registered (the
        // skipped generation 19,999 — no session record can exist).
        assert!(matches!(
            reg.apply(&ev(state::SYNCHRONIZED, 1, "never-seen", 19_999), true),
            ApplyOutcome::Rejected(_)
        ));
        // Current execution duplicate stays idempotent.
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 3, &newest, 20_001), true),
            ApplyOutcome::Duplicate
        );
        // Conflicting UUID under the already-used current generation.
        assert_eq!(
            reg.apply(&ev(state::SYNCHRONIZED, 1, "impostor", 20_001), true),
            ApplyOutcome::Rejected("execution identity conflict")
        );
        // Newest legitimate execution remains authoritative.
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), newest))
        );
    }

    /// M5.2B review B2: a newer verified execution that advances the
    /// high-water mark via a FIRST-SEEN TERMINAL event must invalidate
    /// the older synchronized current session immediately — current
    /// authority always implies fence-head rank. The old session can
    /// never regain authority, and a subsequent newer execution
    /// establishes current normally.
    #[test]
    fn terminal_first_new_execution_invalidates_older_current() {
        let reg = SubscriptionSessionRegistry::new();
        // Execution A (exec_gen 1) synchronizes and becomes current.
        for (st, seq) in [
            (state::SUBMITTED, 1),
            (state::SYNCHRONIZING, 2),
            (state::SYNCHRONIZED, 3),
        ] {
            assert_eq!(
                reg.apply(&evx(st, seq, "exec-a", 1), true),
                ApplyOutcome::Applied
            );
        }
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "exec-a".to_string()))
        );

        // Execution B (exec_gen 2): its submitted/synchronizing events
        // are LOST — the first event the core sees is already terminal.
        assert_eq!(
            reg.apply(&evx(state::FAILED, 1, "exec-b", 2), true),
            ApplyOutcome::Applied
        );
        // A must NOT remain current: its rank is below the high-water
        // mark even though B never held live coverage.
        assert_eq!(reg.current_session("dut"), None);

        // A cannot regain authority: a late synchronized transition is
        // recorded as history at most — it never becomes current.
        let _ = reg.apply(&evx(state::SYNCHRONIZED, 4, "exec-a", 1), true);
        assert_eq!(reg.current_session("dut"), None);

        // Execution C (exec_gen 3) establishes current authority.
        for (st, seq) in [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)] {
            assert_eq!(
                reg.apply(&evx(st, seq, "exec-c", 3), true),
                ApplyOutcome::Applied
            );
        }
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "exec-c".to_string()))
        );
    }

    /// M5.2B review B2: an UNVERIFIED higher-ranked event must never
    /// supersede a legitimate verified current session — sibling
    /// supersession and current authority are verified-only.
    #[test]
    fn unverified_higher_rank_cannot_supersede_verified_current() {
        let reg = SubscriptionSessionRegistry::new();
        for (st, seq) in [(state::SUBMITTED, 1), (state::SYNCHRONIZED, 2)] {
            assert_eq!(
                reg.apply(&evx(st, seq, "exec-a", 1), true),
                ApplyOutcome::Applied
            );
        }
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "exec-a".to_string()))
        );

        // Unverified NEW execution entering live states: recorded for
        // diagnostics at most — the verified current session and its
        // authority are untouched.
        for (st, seq) in [
            (state::SUBMITTED, 1),
            (state::SYNCHRONIZING, 2),
            (state::SYNCHRONIZED, 3),
        ] {
            let _ = reg.apply(&evx(st, seq, "exec-rogue", 9), false);
        }
        assert_eq!(
            reg.current_session("dut"),
            Some(("plan".to_string(), "exec-a".to_string())),
            "unverified higher-ranked events must not disturb verified current"
        );
        // The verified fence head is unchanged: exec-a is still the
        // only session allowed to hold authority.
        assert_eq!(
            reg.apply(&evx(state::SYNCHRONIZED, 2, "exec-a", 1), true),
            ApplyOutcome::Duplicate
        );
    }
}

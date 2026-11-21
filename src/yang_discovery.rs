//! M6E authenticated remote-collector YANG discovery.
//!
//! Core remains the authority for schema resolution and collection
//! approval. This module implements the bounded job lifecycle:
//!
//! ```text
//! Admin request → Core validates device + assigned collector → durable
//! bounded job → authenticated collector channel (poll) → collector
//! executes read-only gNMI Capabilities/Get → bounded evidence → Core
//! validates collector/session/job/device → Core resolves applicability
//! → existing SQLite snapshot persistence.
//! ```
//!
//! Trust boundary: a discovery result is evidence ONLY when bound to a
//! Core-issued job, the authenticated collector principal, the current
//! collector session generation, the current authority epoch, the
//! current device assignment and a live deadline. A claimed collector id
//! or device address inside a payload is never authority. No gNMI Set,
//! no subscription changes, no device writes, no credentials in jobs or
//! evidence — connection material stays in the secure assignment path.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::api::pb;
use crate::assignment::CollectorManager;
use crate::config::TargetConfig;
use crate::discovery::{self, DiscoveryInput};
use crate::registry::{self, ApiRegistry};
use crate::sqlite_store::{
    SqliteStore, YANG_DISCOVERY_DEFAULT_DEADLINE_NS, YANG_DISCOVERY_JOB_HISTORY,
    YANG_DISCOVERY_MAX_ACTIVE_PER_COLLECTOR, YANG_DISCOVERY_MAX_ACTIVE_PER_DEVICE,
    YANG_DISCOVERY_MAX_ACTIVE_TOTAL, YANG_DISCOVERY_MAX_DIAGNOSTICS,
    YANG_DISCOVERY_MAX_EVIDENCE_BYTES, YANG_DISCOVERY_MAX_POLL_BATCH, YangDiscoveryJobRow,
};
use crate::yang_library::probe_yang_library;
use crate::yang_schema::applicability::{CapabilityEvidence, YangLibraryOutcome};

/// Feature flag a collector must advertise (in `CollectorIdentity.features`)
/// AND satisfy the contract protocol version to receive executable jobs.
pub const YANG_DISCOVERY_FEATURE: &str = "yang_discovery_v1";
/// Discovery contract protocol version. Equals the bumped
/// `PROTOCOL_VERSION_CURRENT`; pre-contract collectors never see jobs.
pub const YANG_DISCOVERY_CONTRACT_VERSION: u32 = 6;
/// The only supported discovery operation in M6E.
pub const OPERATION_YANG_APPLICABILITY: &str = "yang_applicability";

// Job states (durable, machine-readable).
pub const JOB_QUEUED: &str = "queued";
pub const JOB_DISPATCHED: &str = "dispatched";
pub const JOB_COMPLETED: &str = "completed";
pub const JOB_UNSUPPORTED: &str = "unsupported";
pub const JOB_DENIED: &str = "denied";
pub const JOB_TIMED_OUT: &str = "timed_out";
pub const JOB_FAILED: &str = "failed";
pub const JOB_CANCELLED: &str = "cancelled";
pub const JOB_STALE_AUTHORITY: &str = "stale_authority";
pub const JOB_SUPERSEDED: &str = "superseded";
/// In-flight single-winner admission (M6E hardening): exactly one
/// submission owns the snapshot+completion commit.
pub const JOB_ADMITTING: &str = "admitting";

/// Evidence executor classifications persisted on snapshots (M6E-G).
pub const EXECUTOR_CORE_DIRECT: &str = "core_direct";
pub const EXECUTOR_AUTHENTICATED_COLLECTOR: &str = "authenticated_collector";
pub const EXECUTOR_LEGACY_UNKNOWN: &str = "legacy_unknown";

/// Collector-side overall job deadline cap (device I/O is bounded well
/// below this; the durable job deadline is the outer bound).
pub const COLLECTOR_JOB_TIMEOUT_NS: i64 = 90 * 1_000_000_000;
/// Evidence observation timestamps may skew this far before job
/// creation (collector/device clock differences) — never further.
const OBSERVATION_SKEW_NS: i64 = 10 * 60 * 1_000_000_000;
/// Future-dated evidence tolerance (reuses the applicability window).
const OBSERVATION_FUTURE_NS: i64 = crate::yang_applicability::FUTURE_EVIDENCE_TOLERANCE_NS;
/// Per-diagnostic-entry bound on submitted detail strings.
const MAX_DIAGNOSTIC_ENTRY_CHARS: usize = 512;

fn now_ns() -> i64 {
    crate::graph::common::now_ns()
}

// ── Job creation (Core side, Admin-initiated) ────────────────────────────────

/// Why a remote-discovery request could not be queued.
#[derive(Debug)]
pub enum RequestError {
    /// Device has no assigned collector — remote discovery impossible.
    NoCollector,
    /// The assigned collector's ACTIVE session is known to predate the
    /// discovery contract or lacks the feature flag.
    CollectorUnsupported,
    /// Bounded queue limits reached.
    QueueFull(&'static str),
    /// Store/registry failure.
    Internal(anyhow::Error),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCollector => write!(f, "device has no assigned collector"),
            Self::CollectorUnsupported => write!(
                f,
                "assigned collector does not support remote yang discovery"
            ),
            Self::QueueFull(which) => write!(f, "discovery queue full: {which}"),
            Self::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

/// Queue a bounded remote-discovery job for `target`'s assigned
/// collector. Idempotent per (device, operation): an already-active job
/// is returned instead of duplicating work. The job is durable before
/// this returns — restart reconciliation and deadline expiry are
/// handled by the store/manager, never by memory alone.
pub fn request_remote_discovery(
    _registry: &ApiRegistry,
    store: &SqliteStore,
    manager: &CollectorManager,
    target: &TargetConfig,
    datastore: Option<String>,
    actor: &str,
) -> std::result::Result<YangDiscoveryJobRow, RequestError> {
    let collector_id = target
        .collector_id
        .clone()
        .ok_or(RequestError::NoCollector)?;
    let device_key = registry::device_identity_key(target);

    // Per-device dedup: an in-flight job for the same device+datastore
    // is the answer — never queue duplicates.
    let active = store
        .active_yang_discovery_jobs_for_device(&device_key)
        .map_err(|e| RequestError::Internal(e.into()))?;
    for job in &active {
        if job.datastore == datastore && job.operation == OPERATION_YANG_APPLICABILITY {
            return Ok(job.clone());
        }
    }
    if active.len() as i64 >= YANG_DISCOVERY_MAX_ACTIVE_PER_DEVICE {
        return Err(RequestError::QueueFull("per_device"));
    }

    // Known-incompatible active session → explicit unsupported outcome,
    // never silently queued work the collector cannot execute. An
    // OFFLINE collector (no session) may still be queued: it can pick
    // the job up after reconnect within the deadline.
    if let Some(supports) = manager.collector_discovery_support(&collector_id) {
        if !supports {
            return Err(RequestError::CollectorUnsupported);
        }
    }

    // Bounded outstanding work — checked inside the same store lock
    // ordering as creation to keep the bound meaningful under
    // concurrency (worst case a small overshoot is fenced by the
    // per-collector/per-device caps at dispatch).
    let per_collector = store
        .count_active_yang_discovery_jobs(Some(&collector_id), None)
        .map_err(|e| RequestError::Internal(e.into()))?;
    if per_collector >= YANG_DISCOVERY_MAX_ACTIVE_PER_COLLECTOR {
        return Err(RequestError::QueueFull("per_collector"));
    }
    let total = store
        .count_active_yang_discovery_jobs(None, None)
        .map_err(|e| RequestError::Internal(e.into()))?;
    if total >= YANG_DISCOVERY_MAX_ACTIVE_TOTAL {
        return Err(RequestError::QueueFull("global"));
    }

    let now = now_ns();
    let row = YangDiscoveryJobRow {
        id: 0,
        job_id: uuid::Uuid::new_v4().to_string(),
        device_key,
        device_address: target.address.clone(),
        collector_id,
        datastore,
        operation: OPERATION_YANG_APPLICABILITY.to_string(),
        state: JOB_QUEUED.to_string(),
        authority_epoch: manager.authority_epoch() as i64,
        assignment_generation: manager
            .latest_assignment_generation(target.collector_id.as_deref().unwrap_or(""))
            as i64,
        collector_session_generation: None,
        created_at_ns: now,
        deadline_ns: now + YANG_DISCOVERY_DEFAULT_DEADLINE_NS,
        dispatched_at_ns: None,
        finished_at_ns: None,
        attempt_count: 0,
        result_classification: None,
        reason_code: None,
        snapshot_id: None,
        evidence_fingerprint: None,
        result_digest: None,
        diagnostics_json: None,
        created_by: Some(actor.to_string()),
        updated_at_ns: now,
        device_revision: target.updated_at_ns,
        admission_claim: None,
    };
    store
        .create_yang_discovery_job(&row)
        .map_err(|e| RequestError::Internal(e.into()))?;
    Ok(row)
}

// ── Collector poll (authenticated channel) ──────────────────────────────────

pub struct PollOutcome {
    pub jobs: Vec<pb::YangDiscoveryJob>,
    pub cancelled_job_ids: Vec<String>,
    /// False for sessions that predate the discovery contract — they
    /// receive no executable work and an explicit unsupported signal.
    pub discovery_supported: bool,
}

fn job_to_proto(job: &YangDiscoveryJobRow) -> pb::YangDiscoveryJob {
    pb::YangDiscoveryJob {
        job_id: job.job_id.clone(),
        device_address: job.device_address.clone(),
        device_key: job.device_key.clone(),
        datastore: job.datastore.clone().unwrap_or_default(),
        operation: job.operation.clone(),
        authority_epoch: job.authority_epoch as u64,
        assignment_generation: job.assignment_generation as u64,
        collector_session_generation: job.collector_session_generation.unwrap_or(0) as u64,
        created_at_ns: job.created_at_ns,
        deadline_ns: job.deadline_ns,
        protocol_version: YANG_DISCOVERY_CONTRACT_VERSION,
    }
}

/// Registry-side device binding for a discovery job: device exists,
/// enabled, still assigned to the job's collector, same identity key.
/// Returns the live target on success — callers needing the
/// configuration revision use it directly.
fn job_device_binding_current(
    registry: &ApiRegistry,
    job: &YangDiscoveryJobRow,
) -> Result<Option<crate::config::TargetConfig>> {
    let Some(target) = registry.get_device(&job.device_address)? else {
        return Ok(None);
    };
    if !target.enabled
        || target.collector_id.as_deref() != Some(job.collector_id.as_str())
        || registry::device_identity_key(&target) != job.device_key
    {
        return Ok(None);
    }
    Ok(Some(target))
}

/// Whether the job's device binding AND stamped assignment authority
/// are still valid for this collector. Used for jobs that have already
/// been dispatched — their stamps were fixed at dispatch time.
/// QUEUED jobs must NOT be fenced on queue-time stamps: a job queued
/// while its collector was offline carries generation 0 and is stamped
/// with current authority at dispatch instead.
fn job_device_binding_valid(
    registry: &ApiRegistry,
    manager: &CollectorManager,
    job: &YangDiscoveryJobRow,
) -> Result<bool> {
    if job_device_binding_current(registry, job)?.is_none() {
        return Ok(false);
    }
    Ok(manager.validate_discovery_assignment(
        &job.collector_id,
        job.authority_epoch as u64,
        job.assignment_generation as u64,
    ))
}

/// Collector-initiated poll. Validates the session, lazily expires
/// dead jobs, cancels jobs whose device binding is gone, and dispatches
/// bounded pending work stamped with the CURRENT session generation.
/// Duplicate delivery is expected and safe — admission is idempotent.
pub fn poll_discovery_jobs(
    store: &SqliteStore,
    registry: &ApiRegistry,
    manager: &CollectorManager,
    collector_id: &str,
    now: i64,
) -> Result<PollOutcome> {
    let session_gen = manager.active_session_generation(collector_id);
    let Some(sgen) = session_gen else {
        return Ok(PollOutcome {
            jobs: vec![],
            cancelled_job_ids: vec![],
            discovery_supported: false,
        });
    };
    let Some(supports) = manager.collector_discovery_support(collector_id) else {
        return Ok(PollOutcome {
            jobs: vec![],
            cancelled_job_ids: vec![],
            discovery_supported: false,
        });
    };
    if !supports {
        return Ok(PollOutcome {
            jobs: vec![],
            cancelled_job_ids: vec![],
            discovery_supported: false,
        });
    }

    let mut cancelled = Vec::new();
    // Sweep this collector's active jobs: expire deadlines and cancel
    // bindings that no longer hold (device removed/reassigned/authority
    // superseded). Stale-session jobs are NOT cancelled — they are
    // redispatched under the current session below.
    let active = store.active_yang_discovery_jobs_for_collector(collector_id)?;
    for job in active {
        if job.deadline_ns <= now {
            store.finish_yang_discovery_job(
                &job.job_id,
                JOB_TIMED_OUT,
                Some("deadline_exceeded"),
                Some("deadline_exceeded"),
                None,
                None,
                None,
                None,
                now,
            )?;
            cancelled.push(job.job_id);
            continue;
        }
        // QUEUED jobs are fenced on the live device binding only —
        // their authority stamps are re-issued at dispatch. Dispatched
        // or in-flight admissions are fenced on the stamps they were
        // actually dispatched under.
        let binding_ok = match if job.state == JOB_QUEUED {
            job_device_binding_current(registry, &job).map(|t| t.is_some())
        } else {
            job_device_binding_valid(registry, manager, &job)
        } {
            Ok(ok) => ok,
            Err(_) => continue,
        };
        if !binding_ok {
            let device_gone = registry
                .get_device(&job.device_address)?
                .map(|t| !t.enabled)
                .unwrap_or(true);
            let (state, reason) = if device_gone {
                (JOB_CANCELLED, "device_removed")
            } else {
                (JOB_STALE_AUTHORITY, "device_binding_changed")
            };
            store.finish_yang_discovery_job(
                &job.job_id,
                state,
                Some(reason),
                Some(reason),
                None,
                None,
                None,
                None,
                now,
            )?;
            cancelled.push(job.job_id);
        }
    }

    // Dispatch pending work under the CURRENT session generation.
    // `dispatched` jobs stamped with an older session generation are
    // re-delivered (the previous session is gone); jobs already
    // dispatched under THIS session are not re-sent every poll.
    let pending = store.dispatchable_yang_discovery_jobs(
        collector_id,
        sgen as i64,
        now,
        YANG_DISCOVERY_MAX_POLL_BATCH,
    )?;
    let mut jobs = Vec::new();
    for mut job in pending {
        // Dispatch stamps CURRENT authority: epoch, latest assignment
        // generation and the live device configuration revision. A job
        // queued while its collector was offline (generation 0) is
        // safely dispatched once the collector registers and receives
        // its first full sync — the stamp is issued here, never
        // trusted from queue time.
        let Some(target) = job_device_binding_current(registry, &job)? else {
            continue;
        };
        let authority_epoch = manager.authority_epoch() as i64;
        let assignment_generation = manager.latest_assignment_generation(&job.collector_id) as i64;
        if assignment_generation <= 0 {
            // No assignment sync has ever been delivered to this
            // collector — dispatching now would stamp generation 0,
            // which is never authority. Leave queued; the next poll
            // after the first full sync dispatches it.
            continue;
        }
        if store.mark_yang_discovery_job_dispatched(
            &job.job_id,
            sgen as i64,
            authority_epoch,
            assignment_generation,
            target.updated_at_ns,
            now,
        )? {
            // The durable row now carries this session's generation
            // and current assignment authority; reflect them on the
            // wire copy (the pending row predates the dispatch stamp).
            job.collector_session_generation = Some(sgen as i64);
            job.authority_epoch = authority_epoch;
            job.assignment_generation = assignment_generation;
            jobs.push(job_to_proto(&job));
        }
    }
    metrics::counter!("bonsai_yang_discovery_jobs_dispatched_total").increment(jobs.len() as u64);
    Ok(PollOutcome {
        jobs,
        cancelled_job_ids: cancelled,
        discovery_supported: true,
    })
}

// ── Result admission (Core side, authenticated submission) ──────────────────

/// Outcome of `admit_discovery_result`.
#[derive(Debug)]
pub enum AdmitResult {
    /// Evidence accepted and persisted as an applicability snapshot.
    Accepted { snapshot_id: i64, duplicate: bool },
    /// A well-formed job-level failure outcome was recorded (timeout,
    /// denied, transport failure, …) — no snapshot, explicit state.
    Finished {
        state: String,
        classification: String,
    },
    /// Submission rejected; `finished_state` is the terminal job state
    /// applied when the rejection itself closes the job.
    Rejected {
        reason: String,
        finished_state: Option<String>,
    },
}

fn reject(reason: impl Into<String>, finished_state: Option<&str>) -> AdmitResult {
    AdmitResult::Rejected {
        reason: reason.into(),
        finished_state: finished_state.map(str::to_string),
    }
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(b"\x1f");
    }
    hex::encode(h.finalize())
}

fn bound_diagnostics(raw: &[String]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let kept: Vec<String> = raw
        .iter()
        .take(YANG_DISCOVERY_MAX_DIAGNOSTICS)
        .map(|d| d.chars().take(MAX_DIAGNOSTIC_ENTRY_CHARS).collect())
        .collect();
    serde_json::to_string(&kept).ok()
}

/// Diagnostics bounded BEFORE they reach resolution/persistence —
/// count AND per-entry length, never silently unbounded.
fn bounded_diagnostics(raw: &[String]) -> Vec<String> {
    raw.iter()
        .take(YANG_DISCOVERY_MAX_DIAGNOSTICS)
        .map(|d| d.chars().take(MAX_DIAGNOSTIC_ENTRY_CHARS).collect())
        .collect()
}

/// Result of the admission currency check: either the live device
/// target the job is bound to, or the terminal admission outcome
/// (which may already have finished the job durably).
enum AdmissionCurrency {
    Current(crate::config::TargetConfig),
    Rejected(AdmitResult),
}

/// Currency fences evaluated at admission AND re-evaluated at commit
/// time: deadline, active session generation, authority epoch, device
/// binding (including the claimed device address), device configuration
/// revision, and assignment authority. Every failure is explicit — a
/// job is only finished when the rejection itself is terminal.
#[allow(clippy::too_many_arguments)]
fn check_admission_currency(
    store: &SqliteStore,
    registry: &ApiRegistry,
    manager: &CollectorManager,
    job: &YangDiscoveryJobRow,
    claimed_device_address: &str,
    now: i64,
) -> Result<AdmissionCurrency> {
    // ── Deadline ─────────────────────────────────────────────────────
    if now > job.deadline_ns {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_TIMED_OUT,
            Some("deadline_exceeded"),
            Some("deadline_exceeded"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(AdmissionCurrency::Rejected(reject(
            "deadline_exceeded",
            Some(JOB_TIMED_OUT),
        )));
    }

    // ── Session + authority fencing ──────────────────────────────────
    let active_gen = manager
        .active_session_generation(&job.collector_id)
        .map(|g| g as i64);
    if active_gen != job.collector_session_generation {
        // Result stamped under a superseded session — never authority.
        // The job itself stays dispatchable under the new session.
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "stale_session")
            .increment(1);
        return Ok(AdmissionCurrency::Rejected(reject("stale_session", None)));
    }
    if manager.authority_epoch() as i64 != job.authority_epoch {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_STALE_AUTHORITY,
            Some("authority_epoch_superseded"),
            Some("authority_epoch_superseded"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(AdmissionCurrency::Rejected(reject(
            "stale_authority",
            Some(JOB_STALE_AUTHORITY),
        )));
    }

    // ── Device binding: canonical identity + current assignment +
    //    configuration revision. The revision fence rejects evidence
    //    gathered under obsolete connection settings: any device
    //    mutation (credentials, reassignment, enable/disable) bumps
    //    `updated_at_ns`, so a job dispatched against revision N can
    //    never admit evidence after the device moved to revision M.
    let device_ok = match registry.get_device(&job.device_address)? {
        Some(target) => {
            target.enabled
                && target.collector_id.as_deref() == Some(job.collector_id.as_str())
                && registry::device_identity_key(&target) == job.device_key
                && !claimed_device_address.is_empty()
                && canonical_equal(claimed_device_address, &job.device_address)
        }
        None => false,
    };
    if !device_ok {
        let gone = registry
            .get_device(&job.device_address)?
            .map(|t| !t.enabled)
            .unwrap_or(true);
        let (state, reason) = if gone {
            (JOB_CANCELLED, "device_removed")
        } else {
            (JOB_STALE_AUTHORITY, "device_binding_changed")
        };
        store.finish_yang_discovery_job(
            &job.job_id,
            state,
            Some(reason),
            Some(reason),
            None,
            None,
            None,
            None,
            now,
        )?;
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => reason)
            .increment(1);
        return Ok(AdmissionCurrency::Rejected(reject(reason, Some(state))));
    }
    let target = registry
        .get_device(&job.device_address)?
        .context("device disappeared during admission")?;
    if target.updated_at_ns != job.device_revision {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_STALE_AUTHORITY,
            Some("device_config_changed"),
            Some("device_config_changed"),
            None,
            None,
            None,
            None,
            now,
        )?;
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "device_config_changed")
            .increment(1);
        return Ok(AdmissionCurrency::Rejected(reject(
            "device_config_changed",
            Some(JOB_STALE_AUTHORITY),
        )));
    }
    if !manager.validate_discovery_assignment(
        &job.collector_id,
        job.authority_epoch as u64,
        job.assignment_generation as u64,
    ) {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_STALE_AUTHORITY,
            Some("assignment_authority_lost"),
            Some("assignment_authority_lost"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(AdmissionCurrency::Rejected(reject(
            "assignment_authority_lost",
            Some(JOB_STALE_AUTHORITY),
        )));
    }
    Ok(AdmissionCurrency::Current(target))
}

/// Validate and admit one collector-submitted discovery result.
/// `authenticated_collector_id` is the transport-attested,
/// credential-bound identity — payload claims are never trusted.
///
/// Every rejection is explicit and bounded; a valid-looking payload
/// from an unauthorized collector is never evidence authority.
#[allow(clippy::too_many_arguments)]
pub async fn admit_discovery_result(
    store: &SqliteStore,
    registry: &ApiRegistry,
    manager: &CollectorManager,
    yang_roots: Option<&(String, String, String)>,
    authenticated_collector_id: &str,
    result: &pb::YangDiscoveryResult,
    now: i64,
) -> Result<AdmitResult> {
    if result.job_id.is_empty() {
        return Ok(reject("missing_job_id", None));
    }
    let Some(job) = store.get_yang_discovery_job(&result.job_id)? else {
        return Ok(reject("unknown_job", None));
    };

    // ── Collector binding BEFORE any acknowledgement (M6E hardening):
    //    a completed job must never reveal its snapshot id to any
    //    collector other than the authenticated authorized assignee.
    if authenticated_collector_id != job.collector_id || result.collector_id != job.collector_id {
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "wrong_collector")
            .increment(1);
        return Ok(reject("wrong_collector", None));
    }

    // ── Authority stamp verification: the result must echo the exact
    //    Core-issued job stamps. Missing, zero, foreign or superseded
    //    stamps are never authority — rejected without finishing the
    //    job so legitimate redispatch can still succeed.
    if result.authority_epoch == 0 {
        return Ok(reject("missing_authority_epoch", None));
    }
    if result.authority_epoch as i64 != job.authority_epoch {
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "authority_epoch_mismatch")
            .increment(1);
        return Ok(reject("authority_epoch_mismatch", None));
    }
    if result.assignment_generation == 0 {
        return Ok(reject("missing_assignment_generation", None));
    }
    if result.assignment_generation as i64 != job.assignment_generation {
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "assignment_generation_mismatch")
            .increment(1);
        return Ok(reject("assignment_generation_mismatch", None));
    }
    if result.collector_session_generation == 0 {
        return Ok(reject("missing_session_generation", None));
    }
    // The result's echoed session generation must match the job's
    // CURRENT dispatch stamp — a result produced under a superseded
    // session can never ride a newer dispatch to authority.
    if result.collector_session_generation as i64 != job.collector_session_generation.unwrap_or(-1)
    {
        metrics::counter!("bonsai_yang_discovery_jobs_rejected_total", "reason" => "session_generation_mismatch")
            .increment(1);
        return Ok(reject("session_generation_mismatch", None));
    }

    // ── Job state / idempotent replay ────────────────────────────────
    match job.state.as_str() {
        JOB_COMPLETED => {
            // Idempotent duplicate: identical evidence digest → accept;
            // different payload for a completed job → conflict.
            let digest = result_digest(result);
            if job.result_digest.as_deref() == Some(digest.as_str()) {
                metrics::counter!("bonsai_yang_discovery_jobs_duplicate_total").increment(1);
                return Ok(AdmitResult::Accepted {
                    snapshot_id: job.snapshot_id.unwrap_or(0),
                    duplicate: true,
                });
            }
            return Ok(reject("conflicting_duplicate", None));
        }
        JOB_ADMITTING => {
            // A concurrent submission owns the admission — never a
            // second snapshot, never divergent job state.
            return Ok(reject("admission_in_progress", None));
        }
        JOB_QUEUED | JOB_DISPATCHED => {}
        other => {
            return Ok(reject(format!("job_terminal:{other}"), None));
        }
    }
    if job.state != JOB_DISPATCHED {
        return Ok(reject("job_not_dispatched", None));
    }

    // ── Currency fences (deadline/session/epoch/device/revision/
    //    assignment) — evaluated now and AGAIN at commit time. ────────
    let target = match check_admission_currency(
        store,
        registry,
        manager,
        &job,
        &result.device_address,
        now,
    )? {
        AdmissionCurrency::Current(t) => t,
        AdmissionCurrency::Rejected(outcome) => return Ok(outcome),
    };

    // ── Protocol compatibility (fail closed) ─────────────────────────
    if result.protocol_version > 0
        && matches!(
            crate::api::check_protocol_compat(
                result.protocol_version,
                crate::api::PROTOCOL_VERSION_CURRENT
            ),
            crate::api::VersionCompat::MajorSkew { .. }
        )
    {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_UNSUPPORTED,
            Some("protocol_incompatible"),
            Some("protocol_incompatible"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(reject("protocol_incompatible", Some(JOB_UNSUPPORTED)));
    }

    // ── Result observation timestamp bounds (M6E hardening): the
    //    result-level observed_at_ns must agree with the documented
    //    clock-skew policy — a missing or out-of-window timestamp is
    //    malformed evidence for ANY outcome, success or failure.
    if result.observed_at_ns <= 0
        || result.observed_at_ns < job.created_at_ns - OBSERVATION_SKEW_NS
        || result.observed_at_ns > now + OBSERVATION_FUTURE_NS
    {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_FAILED,
            Some("evidence_time_out_of_bounds"),
            Some("evidence_time_out_of_bounds"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(reject("evidence_time_out_of_bounds", Some(JOB_FAILED)));
    }

    // ── Job-level failure outcomes: explicit, never fabricated ───────
    if result.outcome != "completed" {
        let state = match result.outcome.as_str() {
            "timeout" => JOB_TIMED_OUT,
            "permission_denied" => JOB_DENIED,
            "unsupported" | "unsupported_encoding" => JOB_UNSUPPORTED,
            "cancelled" => JOB_CANCELLED,
            "stale_authority" => JOB_STALE_AUTHORITY,
            _ => JOB_FAILED,
        };
        // Outcome detail is bounded BEFORE it is classified or stored —
        // oversized failure payloads are never persisted raw.
        let outcome: String = result.outcome.chars().take(64).collect();
        let detail: String = result
            .outcome_detail
            .chars()
            .take(MAX_DIAGNOSTIC_ENTRY_CHARS)
            .collect();
        let classification = if detail.is_empty() {
            outcome.clone()
        } else {
            format!("{outcome}:{detail}")
        };
        store.finish_yang_discovery_job(
            &job.job_id,
            state,
            Some(&classification),
            Some(&outcome),
            None,
            None,
            None,
            bound_diagnostics(&result.diagnostics).as_deref(),
            now,
        )?;
        metrics::counter!("bonsai_yang_discovery_jobs_failed_total", "reason" => state)
            .increment(1);
        return Ok(AdmitResult::Finished {
            state: state.to_string(),
            classification,
        });
    }

    // ── Payload bounds + structural validity ─────────────────────────
    if result.capabilities_json.len() > YANG_DISCOVERY_MAX_EVIDENCE_BYTES
        || result.yang_library_json.len() > YANG_DISCOVERY_MAX_EVIDENCE_BYTES
        || result.capabilities_json.len() + result.yang_library_json.len()
            > 2 * YANG_DISCOVERY_MAX_EVIDENCE_BYTES
    {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_FAILED,
            Some("oversized_evidence"),
            Some("oversized_evidence"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(reject("oversized_evidence", Some(JOB_FAILED)));
    }
    if result.yang_library_json.is_empty() {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_FAILED,
            Some("missing_evidence"),
            Some("missing_evidence"),
            None,
            None,
            None,
            None,
            now,
        )?;
        return Ok(reject("missing_evidence", Some(JOB_FAILED)));
    }

    let capabilities: Option<CapabilityEvidence> = if result.capabilities_json.is_empty() {
        None
    } else {
        match serde_json::from_str(&result.capabilities_json) {
            Ok(c) => Some(c),
            Err(_) => {
                store.finish_yang_discovery_job(
                    &job.job_id,
                    JOB_FAILED,
                    Some("malformed_evidence"),
                    Some("malformed_evidence"),
                    None,
                    None,
                    None,
                    None,
                    now,
                )?;
                return Ok(reject("malformed_evidence", Some(JOB_FAILED)));
            }
        }
    };
    let yang_library: YangLibraryOutcome = match serde_json::from_str(&result.yang_library_json) {
        Ok(y) => y,
        Err(_) => {
            store.finish_yang_discovery_job(
                &job.job_id,
                JOB_FAILED,
                Some("malformed_evidence"),
                Some("malformed_evidence"),
                None,
                None,
                None,
                None,
                now,
            )?;
            return Ok(reject("malformed_evidence", Some(JOB_FAILED)));
        }
    };

    // ── Observation-time bounds ──────────────────────────────────────
    let evidence_times: Vec<i64> = [
        capabilities.as_ref().map(|c| c.observed_at_ns),
        yang_library.evidence().map(|e| e.fetched_at_ns),
    ]
    .into_iter()
    .flatten()
    .collect();
    for t in &evidence_times {
        if *t < job.created_at_ns - OBSERVATION_SKEW_NS || *t > now + OBSERVATION_FUTURE_NS {
            store.finish_yang_discovery_job(
                &job.job_id,
                JOB_FAILED,
                Some("evidence_time_out_of_bounds"),
                Some("evidence_time_out_of_bounds"),
                None,
                None,
                None,
                None,
                now,
            )?;
            return Ok(reject("evidence_time_out_of_bounds", Some(JOB_FAILED)));
        }
    }

    // ── Out-of-order fencing (early check): an older job must never
    //    supersede evidence accepted from a newer job for the same
    //    device. Re-evaluated INSIDE the atomic commit below — this
    //    pre-check only avoids wasted resolution.
    if let Some(latest_id) = store.latest_completed_yang_discovery_job_id(&job.device_key)?
        && latest_id > job.id
    {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_SUPERSEDED,
            Some("superseded_by_newer_job"),
            Some("superseded_by_newer_job"),
            None,
            None,
            None,
            bound_diagnostics(&result.diagnostics).as_deref(),
            now,
        )?;
        return Ok(AdmitResult::Finished {
            state: JOB_SUPERSEDED.to_string(),
            classification: "superseded_by_newer_job".to_string(),
        });
    }

    // ── Single-winner admission (M6E hardening): claim the job BEFORE
    //    resolution so exactly one submission owns the snapshot +
    //    completion commit. A lost claim reports the winner's durable
    //    outcome — identical evidence replays as a duplicate, anything
    //    else is an explicit conflict.
    let digest = result_digest(result);
    let admission_token = uuid::Uuid::new_v4().to_string();
    if !store.claim_yang_discovery_job_admission(&job.job_id, &admission_token, now)? {
        return Ok(match store.get_yang_discovery_job(&result.job_id)? {
            Some(current) if current.state == JOB_COMPLETED => {
                if current.result_digest.as_deref() == Some(digest.as_str()) {
                    metrics::counter!("bonsai_yang_discovery_jobs_duplicate_total").increment(1);
                    AdmitResult::Accepted {
                        snapshot_id: current.snapshot_id.unwrap_or(0),
                        duplicate: true,
                    }
                } else {
                    reject("conflicting_duplicate", None)
                }
            }
            _ => reject("admission_in_progress", None),
        });
    }

    // From here the job is 'admitting' under our token. Internal
    // failures release the claim so the job returns to dispatchable
    // state — never wedged, never half-committed.
    match finish_admission(
        store,
        registry,
        manager,
        &job,
        &target,
        authenticated_collector_id,
        capabilities,
        yang_library,
        bounded_diagnostics(&result.diagnostics),
        result,
        &digest,
        &admission_token,
        yang_roots,
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
        Err(e) => {
            let _ = store.release_yang_discovery_job_admission(&job.job_id, &admission_token, now);
            Err(e)
        }
    }
}

/// Resolve claimed evidence and commit it atomically. Runs the Core-
/// side YANG resolution BEFORE the short durable commit; currency and
/// authorization are rechecked at commit time. Any `Err` propagates to
/// the caller, which releases the admission claim.
#[allow(clippy::too_many_arguments)]
async fn finish_admission(
    store: &SqliteStore,
    registry: &ApiRegistry,
    manager: &CollectorManager,
    job: &YangDiscoveryJobRow,
    target: &crate::config::TargetConfig,
    authenticated_collector_id: &str,
    capabilities: Option<CapabilityEvidence>,
    yang_library: YangLibraryOutcome,
    diagnostics: Vec<String>,
    result: &pb::YangDiscoveryResult,
    digest: &str,
    admission_token: &str,
    yang_roots: Option<&(String, String, String)>,
) -> Result<AdmitResult> {
    // Commit-time currency recheck with a FRESH clock: authority,
    // session, device binding and assignment may have moved while
    // resolution ran. A fence failure either finished the job
    // terminally (release is then a no-op) or returns our still-held
    // claim to dispatchable state.
    let commit_now = now_ns();
    match check_admission_currency(
        store,
        registry,
        manager,
        job,
        &result.device_address,
        commit_now,
    )? {
        AdmissionCurrency::Current(_) => {}
        AdmissionCurrency::Rejected(outcome) => {
            let _ = store.release_yang_discovery_job_admission(
                &job.job_id,
                admission_token,
                commit_now,
            );
            return Ok(outcome);
        }
    }

    // Identity ambiguity is evaluated against CURRENT registry state at
    // commit time — never cached from admission start.
    let identity_ambiguous = registry
        .list_all_targets()?
        .iter()
        .filter(|t| {
            t.enabled
                && t.address != target.address
                && t.collector_id == target.collector_id
                && registry::canonical_device_key(&t.address)
                    == registry::canonical_device_key(&target.address)
        })
        .count()
        > 0;

    let Some((lib_root, cache_root, key_env)) = yang_roots else {
        store.finish_yang_discovery_job(
            &job.job_id,
            JOB_FAILED,
            Some("catalogue_unavailable"),
            Some("catalogue_unavailable"),
            None,
            None,
            None,
            None,
            commit_now,
        )?;
        return Ok(reject("catalogue_unavailable", Some(JOB_FAILED)));
    };
    let library = tokio::task::spawn_blocking({
        let (l, c, k) = (lib_root.clone(), cache_root.clone(), key_env.clone());
        move || {
            crate::yang::YangLibrary::open(std::path::Path::new(&l), std::path::Path::new(&c), &k)
        }
    })
    .await
    .context("yang library open task")?
    .context("open yang library")?;

    // resolve_applicability_snapshot does not use discovery_input —
    // pass a minimal placeholder; no device I/O happens on this path.
    let placeholder_input = DiscoveryInput {
        address: target.address.clone(),
        username: None,
        password: None,
        username_env: None,
        password_env: None,
        ca_cert_path: None,
        tls_domain: None,
        security: None,
        client_cert_path: None,
        client_key_path: None,
        role_hint: None,
        environment_archetype: None,
        vault: None,
    };
    let provenance = crate::yang_applicability::EvidenceProvenance {
        executor: EXECUTOR_AUTHENTICATED_COLLECTOR.to_string(),
        observing_collector_id: Some(authenticated_collector_id.to_string()),
        discovery_job_id: Some(job.job_id.clone()),
        collector_session_generation: job.collector_session_generation,
        authority_epoch: Some(job.authority_epoch),
        evidence_received_at_ns: Some(commit_now),
    };
    let snapshot = crate::yang_applicability::resolve_applicability_snapshot(
        &crate::yang_applicability::RefreshInput {
            target,
            library: &library,
            discovery_input: &placeholder_input,
            identity_ambiguous,
            requested_datastore: job.datastore.clone(),
            store,
        },
        capabilities,
        Some(yang_library),
        diagnostics,
        provenance,
    )
    .await
    .context("resolve remote discovery snapshot")?;
    let snapshot_row = crate::yang_applicability::snapshot_to_row(&snapshot, 0)?;

    // ── Atomic commit: snapshot insert + job completion +
    //    out-of-order recheck in ONE transaction, fenced on the claim
    //    token. A lost claim or a newer completed job can never leave
    //    an authoritative snapshot diverged from job state.
    match store.commit_yang_discovery_admission(
        &job.job_id,
        &job.device_key,
        admission_token,
        &snapshot_row,
        digest,
        bound_diagnostics(&result.diagnostics).as_deref(),
        commit_now,
    )? {
        crate::sqlite_store::YangAdmissionCommit::Completed { snapshot_id } => {
            // Same invalidation policy as direct refresh — evaluated
            // at the registry mutation boundary against CURRENT
            // binding state. Post-commit and best-effort: the
            // committed snapshot+job state is already consistent; a
            // failure here is logged, never allowed to diverge them.
            match crate::schema_binding::revoke_binding_superseded_by_evidence(
                registry,
                &target.address,
                snapshot_id,
                &snapshot.evidence_fingerprint,
                &format!("collector:{authenticated_collector_id}"),
                "yang_remote_discovery",
            ) {
                Ok(crate::schema_binding::EvidenceInvalidation::Revoked(updated)) => {
                    if let Some(cid) = updated.collector_id.as_deref() {
                        manager.resync_collector(cid).await;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(%e, job_id = %job.job_id, "schema-binding invalidation failed after admission commit");
                }
            }
            metrics::counter!("bonsai_yang_discovery_jobs_completed_total").increment(1);
            Ok(AdmitResult::Accepted {
                snapshot_id,
                duplicate: false,
            })
        }
        crate::sqlite_store::YangAdmissionCommit::Superseded => Ok(AdmitResult::Finished {
            state: JOB_SUPERSEDED.to_string(),
            classification: "superseded_by_newer_job".to_string(),
        }),
        crate::sqlite_store::YangAdmissionCommit::ClaimLost => {
            Ok(match store.get_yang_discovery_job(&job.job_id)? {
                Some(current) if current.state == JOB_COMPLETED => {
                    if current.result_digest.as_deref() == Some(digest) {
                        metrics::counter!("bonsai_yang_discovery_jobs_duplicate_total")
                            .increment(1);
                        AdmitResult::Accepted {
                            snapshot_id: current.snapshot_id.unwrap_or(0),
                            duplicate: true,
                        }
                    } else {
                        reject("conflicting_duplicate", None)
                    }
                }
                _ => reject("admission_in_progress", None),
            })
        }
    }
}

fn result_digest(result: &pb::YangDiscoveryResult) -> String {
    sha256_hex(&[
        &result.capabilities_json,
        &result.yang_library_json,
        &result.outcome,
    ])
}

fn canonical_equal(a: &str, b: &str) -> bool {
    registry::canonical_device_key(a) == registry::canonical_device_key(b)
}

// ── Restart reconciliation ──────────────────────────────────────────────────

/// Reconcile durable jobs after a Core restart: expire deadline-passed
/// jobs and cancel jobs stamped under a superseded authority epoch.
/// Results from old sessions/epochs can never become authority — job
/// session stamps are re-issued only through a fresh authenticated poll.
pub fn reconcile_jobs_on_boot(
    store: &SqliteStore,
    manager: &CollectorManager,
) -> Result<(usize, usize)> {
    store.reconcile_yang_discovery_jobs_on_boot(manager.authority_epoch() as i64, now_ns())
}

// ── Collector-side evidence acquisition ─────────────────────────────────────

/// Bounded evidence payload produced by the collector executor.
#[derive(Debug)]
pub struct DiscoverySubmission {
    pub outcome: String,
    pub outcome_detail: String,
    pub capabilities_json: String,
    pub yang_library_json: String,
    pub observed_at_ns: i64,
    pub diagnostics: Vec<String>,
}

/// Execute one discovery job against the assigned device using ONLY
/// read-only gNMI Capabilities + YANG Library Get. Runs the same
/// acquisition primitives as Core-direct discovery — the collector
/// gathers evidence; Core alone resolves and approves.
///
/// `deadline_ns` is the durable job deadline; execution is additionally
/// capped by `COLLECTOR_JOB_TIMEOUT_NS`. No credentials are persisted —
/// they arrive via the secure assignment path on `target`.
pub async fn execute_discovery_job(
    target: &TargetConfig,
    datastore: Option<&str>,
    deadline_ns: i64,
) -> DiscoverySubmission {
    let _ = datastore; // datastore is a resolver input on Core, not a probe parameter
    let remaining = (deadline_ns - now_ns()).max(0) as u64;
    let budget_ns = remaining.min(COLLECTOR_JOB_TIMEOUT_NS as u64);
    let input = DiscoveryInput {
        address: target.address.clone(),
        username: target.username.clone(),
        password: target.password.clone(),
        username_env: target.username_env.clone(),
        password_env: target.password_env.clone(),
        ca_cert_path: target.ca_cert.clone(),
        tls_domain: target.tls_domain.clone(),
        security: target.security,
        client_cert_path: target.client_cert.clone(),
        client_key_path: target.client_key.clone(),
        role_hint: target.role.clone(),
        environment_archetype: None,
        // The collector holds no credential vault; `vault:` references
        // resolve only where the assignment-delivery path materialized
        // them — unresolved refs fail closed inside connect().
        vault: None,
    };

    let work = async {
        let mut diagnostics = Vec::new();
        let capabilities = match discovery::discover_device(input.clone()).await {
            Ok(report) => {
                diagnostics.extend(report.warnings.iter().cloned());
                Some(report.capability_evidence)
            }
            Err(e) => {
                diagnostics.push(format!("capabilities discovery failed: {e:#}"));
                None
            }
        };
        let probe = probe_yang_library(&input).await;
        diagnostics.extend(probe.attempts.iter().cloned());
        let yang_library = probe.outcome;
        (capabilities, yang_library, diagnostics)
    };

    let executed =
        match tokio::time::timeout(std::time::Duration::from_nanos(budget_ns.max(1)), work).await {
            Ok(v) => v,
            Err(_) => {
                return DiscoverySubmission {
                    outcome: "timeout".to_string(),
                    outcome_detail: "job deadline exceeded".to_string(),
                    capabilities_json: String::new(),
                    yang_library_json: String::new(),
                    observed_at_ns: now_ns(),
                    diagnostics: vec!["discovery job timed out".to_string()],
                };
            }
        };

    let (capabilities, yang_library, mut diagnostics) = executed;
    diagnostics.truncate(YANG_DISCOVERY_MAX_DIAGNOSTICS);

    let capabilities_json = capabilities
        .as_ref()
        .and_then(|c| serde_json::to_string(c).ok())
        .unwrap_or_default();
    let yang_library_json = serde_json::to_string(&yang_library).unwrap_or_default();

    // Oversized evidence is rejected outright — never truncated into
    // apparently complete data.
    if capabilities_json.len() > YANG_DISCOVERY_MAX_EVIDENCE_BYTES
        || yang_library_json.len() > YANG_DISCOVERY_MAX_EVIDENCE_BYTES
    {
        return DiscoverySubmission {
            outcome: "malformed".to_string(),
            outcome_detail: "evidence exceeds size bound".to_string(),
            capabilities_json: String::new(),
            yang_library_json: String::new(),
            observed_at_ns: now_ns(),
            diagnostics,
        };
    }

    let observed_at_ns = capabilities
        .as_ref()
        .map(|c| c.observed_at_ns)
        .or_else(|| yang_library.evidence().map(|e| e.fetched_at_ns))
        .unwrap_or_else(now_ns);

    // A capabilities failure means the device could not be read at all —
    // report the explicit failure class rather than fabricating success.
    if capabilities.is_none() {
        let detail = diagnostics
            .last()
            .cloned()
            .unwrap_or_else(|| "capabilities probe failed".to_string());
        let outcome = if detail.contains("timed out") || detail.contains("timeout") {
            "timeout"
        } else if detail.contains("permission") || detail.contains("denied") {
            "permission_denied"
        } else {
            "transport_failure"
        };
        return DiscoverySubmission {
            outcome: outcome.to_string(),
            outcome_detail: detail.chars().take(256).collect(),
            capabilities_json,
            yang_library_json,
            observed_at_ns,
            diagnostics,
        };
    }

    DiscoverySubmission {
        outcome: "completed".to_string(),
        outcome_detail: String::new(),
        capabilities_json,
        yang_library_json,
        observed_at_ns,
        diagnostics,
    }
}

/// Build the wire result for one executed job.
pub fn submission_to_result(
    job: &pb::YangDiscoveryJob,
    collector_id: &str,
    sub: DiscoverySubmission,
    protocol_version: u32,
) -> pb::YangDiscoveryResult {
    pb::YangDiscoveryResult {
        job_id: job.job_id.clone(),
        collector_id: collector_id.to_string(),
        device_address: job.device_address.clone(),
        outcome: sub.outcome,
        outcome_detail: sub.outcome_detail,
        capabilities_json: sub.capabilities_json,
        yang_library_json: sub.yang_library_json,
        observed_at_ns: sub.observed_at_ns,
        diagnostics: sub.diagnostics,
        authority_epoch: job.authority_epoch,
        assignment_generation: job.assignment_generation,
        collector_session_generation: job.collector_session_generation,
        protocol_version,
    }
}

#[allow(dead_code)]
fn _history_bound() -> i64 {
    YANG_DISCOVERY_JOB_HISTORY
}

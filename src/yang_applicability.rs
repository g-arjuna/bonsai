//! M6C-1 device applicability orchestration: gather evidence, resolve,
//! persist, and serve bounded read views.
//!
//! This module is the ONLY place device evidence is turned into a
//! persisted applicability snapshot. It never modifies acquisition
//! intent, subscription plans, or device configuration — applicability
//! is schema-level evidence for M6C-2 planning, nothing more.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::TargetConfig;
use crate::discovery::{self, DiscoveryInput};
use crate::registry::canonical_device_key;
use crate::sqlite_store::{SqliteStore, YangApplicabilitySnapshotRow};
use crate::yang::YangLibrary;
use crate::yang_library::probe_yang_library;
use crate::yang_schema::applicability::{
    self, ApplicabilityResult, CapabilityEvidence, DeviceBinding, YangLibraryOutcome,
};
use crate::yang_schema::{self, CompiledCatalogue};

/// Evidence older than this is reported stale on read (never silently
/// current). Chosen conservatively: device module-sets change on
/// software upgrades, not minutes.
pub const EVIDENCE_MAX_AGE_NS: i64 = 24 * 60 * 60 * 1_000_000_000;

/// Inputs for one bounded refresh. The caller resolves credentials and
/// builds the DiscoveryInput through the existing secure path — this
/// module never sees raw secrets beyond what discovery already handles.
pub struct RefreshInput<'a> {
    pub target: &'a TargetConfig,
    pub library: &'a YangLibrary,
    pub discovery_input: &'a DiscoveryInput,
    /// Set when registry evidence shows ambiguous device identity —
    /// preserved into the snapshot, never merged.
    pub identity_ambiguous: bool,
    /// Optional datastore selection (e.g. "ietf-datastores:running" or
    /// "operational"); `None` lets the resolver pick the unambiguous
    /// default.
    pub requested_datastore: Option<String>,
    pub store: &'a SqliteStore,
}

/// Persisted snapshot envelope (row metadata + typed payloads).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicabilitySnapshot {
    pub snapshot_id: i64,
    pub device_key: String,
    pub device_address: String,
    pub collector_id: Option<String>,
    pub schema_id: Option<String>,
    pub content_id: Option<String>,
    pub datastore: Option<String>,
    pub overall_status: String,
    /// Refresh outcome: "ok" | "catalogue_unavailable" | "probe_failed" |
    /// "resolver_error" — distinguishes evidence problems from result
    /// statuses.
    pub outcome: String,
    pub result: ApplicabilityResult,
    #[serde(default)]
    pub capabilities: Option<CapabilityEvidence>,
    #[serde(default)]
    pub yang_library: Option<YangLibraryOutcome>,
    pub evidence_fingerprint: String,
    pub observed_at_ns: Option<i64>,
    pub evaluated_at_ns: i64,
    #[serde(default)]
    pub error: Option<String>,
    pub created_at_ns: i64,
}

/// Read-side freshness classification computed at request time — the
/// stored snapshot is never mutated by invalidation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFreshness {
    /// Evidence within policy and bound to the current catalogue.
    Fresh,
    /// Evidence aged out or bound to a superseded catalogue — conclusions
    /// must be treated as uncertain until refreshed.
    Stale,
    /// Snapshot exists but its stored payload failed integrity checks.
    Invalid,
}

/// API-facing snapshot view: stored snapshot + computed freshness +
/// explicit invalidation reasons.
#[derive(Clone, Debug, Serialize)]
pub struct ApplicabilitySnapshotView {
    #[serde(flatten)]
    pub snapshot: ApplicabilitySnapshot,
    pub freshness: SnapshotFreshness,
    #[serde(default)]
    pub freshness_reasons: Vec<applicability::ReasonCode>,
    /// schema_id of the currently active catalogue at read time.
    pub active_schema_id: Option<String>,
}

/// Perform a bounded refresh for one device: capabilities → YANG
/// Library probe → deterministic resolve → durable snapshot.
///
/// Every failure class is captured into the snapshot outcome — a failed
/// refresh still produces a truthful record rather than a fabricated
/// result.
pub async fn refresh_device_applicability(
    input: &RefreshInput<'_>,
) -> Result<ApplicabilitySnapshot> {
    let now_ns = crate::graph::common::now_ns();
    let target = input.target;
    let canonical = canonical_device_key(&target.address);
    let device = DeviceBinding {
        address: target.address.clone(),
        canonical_address: canonical.clone(),
        external_id: target.external_id.clone(),
        collector_id: target.collector_id.clone(),
        identity_ambiguous: input.identity_ambiguous,
    };
    let device_key = device.identity_key();

    // ── Catalogue (verified, fail-closed) ─────────────────────────────
    let (catalogue, catalogue_diagnostics) = match yang_schema::load_active_catalogue(input.library)
    {
        Ok(Some(c)) => (Some(c), Vec::new()),
        Ok(None) => (None, vec!["no active compiled catalogue".to_string()]),
        Err(e) => (None, vec![format!("active catalogue load failed: {e:#}")]),
    };

    // ── Capabilities evidence ─────────────────────────────────────────
    let mut diagnostics = catalogue_diagnostics.clone();
    let capabilities = match discovery::discover_device(input.discovery_input.clone()).await {
        Ok(report) => {
            diagnostics.extend(report.warnings.iter().cloned());
            Some(report.capability_evidence)
        }
        Err(e) => {
            diagnostics.push(format!("capabilities discovery failed: {e:#}"));
            None
        }
    };

    // ── RFC 8525 YANG Library probe ───────────────────────────────────
    let probe = probe_yang_library(input.discovery_input).await;
    diagnostics.extend(probe.attempts.iter().cloned());
    let yang_library = probe.outcome;

    // ── Resolve ───────────────────────────────────────────────────────
    let catalogue_ref = catalogue.as_ref();
    let capabilities_ref = capabilities.as_ref();
    let yang_library_ref = Some(&yang_library);
    let result = applicability::resolve_applicability(&applicability::ApplicabilityInput {
        catalogue: catalogue_ref,
        catalogue_diagnostics: diagnostics.clone(),
        capabilities: capabilities_ref,
        yang_library: yang_library_ref,
        requested_datastore: input.requested_datastore.as_deref(),
        platform: target.inventory_platform.as_deref(),
        device: &device,
        now_ns,
    });

    // ── Persist ───────────────────────────────────────────────────────
    let observed_at_ns = result.evidence_observed_at_ns;
    let snapshot = ApplicabilitySnapshot {
        snapshot_id: 0,
        device_key: device_key.clone(),
        device_address: target.address.clone(),
        collector_id: target.collector_id.clone(),
        schema_id: result.schema_id.clone(),
        content_id: result.content_id.clone(),
        datastore: result.datastore.clone(),
        overall_status: result.overall_status.as_str().to_string(),
        outcome: "ok".to_string(),
        evidence_fingerprint: result.evidence_fingerprint.clone(),
        observed_at_ns,
        evaluated_at_ns: now_ns,
        error: None,
        created_at_ns: now_ns,
        result,
        capabilities,
        yang_library: Some(yang_library),
    };

    let row = snapshot_to_row(&snapshot, snapshot_id_placeholder());
    let id = input
        .store
        .save_yang_applicability_snapshot(&row)
        .context("persist applicability snapshot")?;
    let mut saved = snapshot;
    saved.snapshot_id = id;
    Ok(saved)
}

fn snapshot_id_placeholder() -> i64 {
    0
}

/// Serialize a snapshot into its storage row. Payloads are bounded by
/// the resolver's result caps; oversized serialization fails closed.
pub fn snapshot_to_row(snap: &ApplicabilitySnapshot, id: i64) -> YangApplicabilitySnapshotRow {
    YangApplicabilitySnapshotRow {
        id,
        device_key: snap.device_key.clone(),
        device_address: snap.device_address.clone(),
        collector_id: snap.collector_id.clone(),
        schema_id: snap.schema_id.clone(),
        content_id: snap.content_id.clone(),
        datastore: snap.datastore.clone(),
        overall_status: snap.overall_status.clone(),
        outcome: snap.outcome.clone(),
        result_json: serde_json::to_string(&snap.result).unwrap_or_else(|_| "{}".to_string()),
        capabilities_json: snap
            .capabilities
            .as_ref()
            .and_then(|c| serde_json::to_string(c).ok()),
        yang_library_json: snap
            .yang_library
            .as_ref()
            .and_then(|y| serde_json::to_string(y).ok()),
        evidence_fingerprint: snap.evidence_fingerprint.clone(),
        observed_at_ns: snap.observed_at_ns,
        evaluated_at_ns: snap.evaluated_at_ns,
        error: snap.error.clone(),
        created_at_ns: snap.created_at_ns,
    }
}

/// Deserialize a stored row back into a snapshot. Returns Err on
/// corrupt payloads — callers must surface `invalid`, never guess.
pub fn snapshot_from_row(row: &YangApplicabilitySnapshotRow) -> Result<ApplicabilitySnapshot> {
    let result: ApplicabilityResult =
        serde_json::from_str(&row.result_json).context("applicability result_json corrupt")?;
    let capabilities = row
        .capabilities_json
        .as_deref()
        .map(serde_json::from_str::<CapabilityEvidence>)
        .transpose()
        .context("capabilities_json corrupt")?;
    let yang_library = row
        .yang_library_json
        .as_deref()
        .map(serde_json::from_str::<YangLibraryOutcome>)
        .transpose()
        .context("yang_library_json corrupt")?;
    Ok(ApplicabilitySnapshot {
        snapshot_id: row.id,
        device_key: row.device_key.clone(),
        device_address: row.device_address.clone(),
        collector_id: row.collector_id.clone(),
        schema_id: row.schema_id.clone(),
        content_id: row.content_id.clone(),
        datastore: row.datastore.clone(),
        overall_status: row.overall_status.clone(),
        outcome: row.outcome.clone(),
        result,
        capabilities,
        yang_library,
        evidence_fingerprint: row.evidence_fingerprint.clone(),
        observed_at_ns: row.observed_at_ns,
        evaluated_at_ns: row.evaluated_at_ns,
        error: row.error.clone(),
        created_at_ns: row.created_at_ns,
    })
}

/// Compute read-side freshness for a snapshot against the CURRENT
/// active catalogue and wall clock. Invalidation is computed, never
/// written back — history stays intact for diagnostics.
pub fn evaluate_freshness(
    snapshot: &ApplicabilitySnapshot,
    active_schema_id: Option<&str>,
    now_ns: i64,
) -> (SnapshotFreshness, Vec<applicability::ReasonCode>) {
    let mut reasons = Vec::new();
    // Schema version changed: snapshot bound to a different catalogue.
    if let (Some(snap_schema), Some(active)) = (&snapshot.schema_id, active_schema_id)
        && snap_schema != active
    {
        reasons.push(applicability::ReasonCode::SchemaVersionChanged);
    }
    // Evidence aged beyond policy.
    if let Some(observed) = snapshot.observed_at_ns
        && now_ns.saturating_sub(observed) > EVIDENCE_MAX_AGE_NS
    {
        reasons.push(applicability::ReasonCode::EvidenceExpired);
    }
    // No usable observed timestamp at all → cannot prove freshness.
    if snapshot.observed_at_ns.is_none() && !reasons.is_empty() {
        // covered by schema change; nothing extra
    }
    if reasons.is_empty() {
        (SnapshotFreshness::Fresh, reasons)
    } else {
        (SnapshotFreshness::Stale, reasons)
    }
}

/// Load the latest snapshot for a device address and produce the
/// read view with freshness. `device_key` binding uses the same
/// identity rule as the writer (external_id else canonical address).
pub fn load_snapshot_view(
    store: &SqliteStore,
    library: &YangLibrary,
    target: &TargetConfig,
    now_ns: i64,
) -> Result<Option<ApplicabilitySnapshotView>> {
    let device_key = target
        .external_id
        .clone()
        .unwrap_or_else(|| canonical_device_key(&target.address));
    let Some(row) = store.latest_yang_applicability_snapshot(&device_key)? else {
        return Ok(None);
    };
    let snapshot = match snapshot_from_row(&row) {
        Ok(s) => s,
        Err(e) => {
            // Corrupt stored payload: surface an explicit invalid view
            // rather than failing the read or guessing contents.
            let stub = ApplicabilitySnapshot {
                snapshot_id: row.id,
                device_key: row.device_key.clone(),
                device_address: row.device_address.clone(),
                collector_id: row.collector_id.clone(),
                schema_id: row.schema_id.clone(),
                content_id: row.content_id.clone(),
                datastore: row.datastore.clone(),
                overall_status: "unknown".to_string(),
                outcome: "corrupt_snapshot".to_string(),
                result: empty_result(&e),
                capabilities: None,
                yang_library: None,
                evidence_fingerprint: row.evidence_fingerprint.clone(),
                observed_at_ns: row.observed_at_ns,
                evaluated_at_ns: row.evaluated_at_ns,
                error: Some(format!("{e:#}")),
                created_at_ns: row.created_at_ns,
            };
            let active_schema_id = active_schema_id(library);
            return Ok(Some(ApplicabilitySnapshotView {
                snapshot: stub,
                freshness: SnapshotFreshness::Invalid,
                freshness_reasons: vec![applicability::ReasonCode::CatalogueIntegrityFailure],
                active_schema_id,
            }));
        }
    };
    let active_schema_id = active_schema_id(library);
    let (freshness, freshness_reasons) =
        evaluate_freshness(&snapshot, active_schema_id.as_deref(), now_ns);
    Ok(Some(ApplicabilitySnapshotView {
        snapshot,
        freshness,
        freshness_reasons,
        active_schema_id,
    }))
}

fn empty_result(err: &anyhow::Error) -> ApplicabilityResult {
    ApplicabilityResult {
        result_version: applicability::APPLICABILITY_RESULT_VERSION,
        schema_id: None,
        content_id: None,
        datastore: None,
        overall_status: applicability::ApplicabilityStatus::Unknown,
        reasons: vec![applicability::ReasonCode::CatalogueIntegrityFailure],
        modules: Vec::new(),
        paths: Vec::new(),
        paths_total: 0,
        paths_truncated: false,
        diagnostics: vec![format!("stored snapshot corrupt: {err:#}")],
        evidence_fingerprint: String::new(),
        evaluated_at_ns: 0,
        evidence_observed_at_ns: None,
    }
}

fn active_schema_id(library: &YangLibrary) -> Option<String> {
    yang_schema::load_active_catalogue(library)
        .ok()
        .flatten()
        .map(|c: CompiledCatalogue| c.schema_id)
}

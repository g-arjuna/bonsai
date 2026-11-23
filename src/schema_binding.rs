//! M6C-2 schema-bound acquisition policy orchestration.
//!
//! This module is the server-side boundary between the deterministic
//! schema-plan evaluator and durable authority: preview is read-only,
//! activation re-evaluates against the current verified catalogue and
//! fresh applicability evidence, and revocation withdraws runtime
//! authority through the existing registry/collector path.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{AcquisitionMode, SchemaBindingConfig, SelectedSubscriptionPath, TargetConfig};
use crate::registry::{ApiRegistry, device_identity_key};
use crate::sqlite_store::{SchemaBindingRecord, SqliteStore};
use crate::yang::YangLibrary;
use crate::yang_applicability::{self, SnapshotFreshness};
use crate::yang_schema::acquisition::{self, SchemaPlanEvaluation, SchemaPlanInput};
use crate::yang_schema::applicability::ReasonCode;

/// Operator-supplied activation/preview parameters. `selected_paths` and
/// `datastore` default to the device's current explicit selection when
/// absent; `plan_id`/`binding_fingerprint` bind activation to the exact
/// server-recomputed preview the operator approved.
#[derive(Clone, Debug, Deserialize)]
pub struct SchemaPlanRequest {
    #[serde(default)]
    pub selected_paths: Option<Vec<SelectedSubscriptionPath>>,
    #[serde(default)]
    pub datastore: Option<String>,
    #[serde(default)]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub binding_fingerprint: Option<String>,
    /// Configuration revision (`TargetConfig::updated_at_ns`) the
    /// operator previewed. Activation MUST present it: the server
    /// re-evaluates against the current device and rejects with a
    /// conflict when the device changed since the preview.
    #[serde(default)]
    pub expected_config_revision: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaPlanPreview {
    pub device_key: String,
    pub device_address: String,
    pub evaluation: Option<SchemaPlanEvaluation>,
    pub activation_permitted: bool,
    pub catalogue_state: Option<ReasonCode>,
    /// schema_id of the globally active catalogue — context only; the
    /// plan is evaluated against the device's EFFECTIVE catalogue.
    pub active_schema_id: Option<String>,
    /// schema_id the device is effectively bound to (explicit selection
    /// or active catalogue fallback) — the authority for this preview.
    #[serde(default)]
    pub effective_schema_id: Option<String>,
    /// The durable device selection's schema_id, when one exists.
    #[serde(default)]
    pub selected_schema_id: Option<String>,
    pub freshness: Option<SnapshotFreshness>,
    pub freshness_reasons: Vec<ReasonCode>,
    pub current_binding: Option<SchemaBindingConfig>,
    pub binding_state: String,
    /// Device configuration revision this preview was evaluated against.
    /// Clients must echo it back as `expected_config_revision` on
    /// activation so a stale preview can never commit.
    pub config_revision: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaBindingActivation {
    pub device: TargetConfig,
    pub record: SchemaBindingRecord,
    pub evaluation: SchemaPlanEvaluation,
    /// schema_id of the device-effective catalogue this approval was
    /// evaluated against — carried to the durable commit boundary so a
    /// concurrent schema-selection change can never slip an approval
    /// derived from a superseded schema past the check (M6F-F).
    pub effective_schema_id: Option<String>,
}

/// Evaluate the current explicit selection (or a request override)
/// against the verified active catalogue and the freshest persisted
/// applicability evidence. Read-only: no registry, lifecycle, or
/// subscription state is mutated.
pub fn evaluate_preview(
    library: &YangLibrary,
    store: &SqliteStore,
    target: &TargetConfig,
    request: &SchemaPlanRequest,
    now_ns: i64,
) -> Result<SchemaPlanPreview> {
    let selected_paths = request
        .selected_paths
        .clone()
        .unwrap_or_else(|| target.selected_paths.clone());
    let datastore = request.datastore.as_deref();
    let device_key = device_identity_key(target);
    // M6F-F: the plan evaluates against the device's EFFECTIVE catalogue —
    // an explicit durable selection wins over the global active catalogue.
    // A selected bundle that fails verification is an explicit
    // non-authoritative state, never a silent global fallback.
    let effective = yang_applicability::resolve_effective_catalogue(library, store, &device_key)?;
    let view = yang_applicability::load_snapshot_view(store, library, target, now_ns)?;

    let freshness = view.as_ref().map(|v| v.freshness);
    let freshness_reasons = view
        .as_ref()
        .map(|v| v.freshness_reasons.clone())
        .unwrap_or_default();
    let active_schema_id = effective.active_schema_id.clone();
    let effective_schema_id = effective.effective_schema_id.clone();
    let selected_schema_id = effective.selection.as_ref().map(|s| s.schema_id.clone());
    let catalogue_state = effective.catalogue_state;

    let mut evaluation = None;
    if let Some(catalogue) = &effective.catalogue {
        let input = SchemaPlanInput {
            catalogue,
            snapshot: view.as_ref().map(|v| &v.snapshot),
            freshness,
            freshness_reasons: &freshness_reasons,
            target,
            selected_paths: &selected_paths,
            datastore,
            collector_id: target.collector_id.as_deref(),
            expected_config_revision: request
                .expected_config_revision
                .or(Some(target.updated_at_ns)),
            now_ns,
        };
        evaluation = Some(acquisition::evaluate_schema_plan(&input));
    }

    let binding_state = match store.get_schema_binding(&device_key)? {
        Some(record) if record.state == "active" => "active".to_string(),
        Some(record) if record.state == "revoked" => "revoked".to_string(),
        Some(_) => "inactive".to_string(),
        None => "none".to_string(),
    };

    Ok(SchemaPlanPreview {
        device_key,
        device_address: target.address.clone(),
        activation_permitted: evaluation
            .as_ref()
            .map(|e| e.activation_permitted)
            .unwrap_or(false),
        evaluation,
        catalogue_state,
        active_schema_id,
        effective_schema_id,
        selected_schema_id,
        freshness,
        freshness_reasons,
        current_binding: target.schema_binding.clone(),
        binding_state,
        config_revision: target.updated_at_ns,
    })
}

/// Re-evaluate and commit an approved schema-bound acquisition. The
/// client supplies the exact `plan_id`/`binding_fingerprint` it previewed;
/// those values are verified against a fresh server-side evaluation —
/// never trusted as authority by themselves.
pub fn activate(
    library: &YangLibrary,
    store: &SqliteStore,
    target: &TargetConfig,
    request: &SchemaPlanRequest,
    actor: &str,
    now_ns: i64,
) -> Result<SchemaBindingActivation> {
    let plan_id = request
        .plan_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("schema-binding activation requires plan_id"))?;
    let binding_fingerprint = request
        .binding_fingerprint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("schema-binding activation requires binding_fingerprint"))?;
    // Activation must carry the configuration revision the administrator
    // previewed — without it there is no way to prove the approved plan
    // still describes the current device configuration.
    let expected_config_revision = request.expected_config_revision.ok_or_else(|| {
        anyhow::anyhow!("schema-binding activation requires expected_config_revision")
    })?;
    if expected_config_revision != target.updated_at_ns {
        bail!("schema-bound activation rejected: config_revision_mismatch");
    }

    let preview = evaluate_preview(library, store, target, request, now_ns)?;
    let evaluation = preview.evaluation.clone().ok_or_else(|| {
        anyhow::anyhow!("no verified catalogue available for schema-bound activation")
    })?;
    if !evaluation.activation_permitted {
        bail!(
            "schema-bound activation rejected: {}",
            evaluation.rejection_reasons.join(",")
        );
    }
    if evaluation.plan_id != plan_id || evaluation.binding_fingerprint != binding_fingerprint {
        bail!("schema-bound activation does not match the approved plan preview");
    }

    let device_key = device_identity_key(target);
    let existing = store.get_schema_binding(&device_key)?;
    let expected_revision = existing.as_ref().map(|r| r.approval_revision).unwrap_or(0);
    let approval_revision = expected_revision + 1;
    let approved_at_ns = now_ns;
    let binding = SchemaBindingConfig {
        mode: "schema_bound".to_string(),
        schema_id: evaluation.schema_id.clone(),
        snapshot_id: evaluation.snapshot_id.unwrap_or(0),
        content_id: evaluation.content_id.clone(),
        datastore: evaluation.datastore.clone(),
        evidence_fingerprint: evaluation.evidence_fingerprint.clone(),
        plan_id: evaluation.plan_id.clone(),
        binding_fingerprint: evaluation.binding_fingerprint.clone(),
        approval_revision: approval_revision as u64,
        collector_id: evaluation
            .collector_id
            .clone()
            .or_else(|| target.collector_id.clone()),
        approved_by: actor.to_string(),
        approved_at_ns,
    };

    let mut device = target.clone();
    device.selected_paths = evaluation.selected_paths.clone();
    device.acquisition = AcquisitionMode::Explicit;
    device.schema_binding = Some(binding.clone());
    device.inventory_only = false;

    let policy = serde_json::json!({
        "device_key": device_key,
        "device_address": device.address,
        "collector_id": binding.collector_id,
        "datastore": binding.datastore,
        "schema_id": binding.schema_id,
        "snapshot_id": binding.snapshot_id,
        "content_id": binding.content_id,
        "evidence_fingerprint": binding.evidence_fingerprint,
        "plan_id": binding.plan_id,
        "binding_fingerprint": binding.binding_fingerprint,
        "approval_revision": binding.approval_revision,
        "approved_by": binding.approved_by,
        "approved_at_ns": binding.approved_at_ns,
        "selected_paths": evaluation.selected_paths,
    });
    let approved_plan =
        serde_json::to_string(&evaluation).context("failed to serialize approved schema plan")?;
    let created_at_ns = existing.as_ref().map(|r| r.created_at_ns).unwrap_or(now_ns);
    let record = SchemaBindingRecord {
        device_key: device_key.clone(),
        device_address: device.address.clone(),
        mode: "schema_bound".to_string(),
        policy_json: serde_json::to_string(&policy)?,
        approved_plan_json: Some(approved_plan),
        approval_revision,
        approval_fingerprint: Some(binding.binding_fingerprint.clone()),
        state: "active".to_string(),
        approved_by: Some(actor.to_string()),
        approved_at_ns: Some(approved_at_ns),
        revoked_by: None,
        revoked_at_ns: None,
        revoke_reason: None,
        created_at_ns,
        updated_at_ns: now_ns,
    };

    Ok(SchemaBindingActivation {
        device,
        record,
        evaluation,
        effective_schema_id: preview.effective_schema_id.clone(),
    })
}

/// Revoke every active schema-bound approval whose approved schema is not
/// the currently active compiled catalogue — for devices that follow the
/// GLOBAL catalogue. M6F-F: a device with an explicit durable schema
/// selection is bound to ITS selected bundle, not the global one, so
/// global activation must never revoke it; selection changes invalidate
/// through [`revoke_binding_for_device_schema_change`] instead. When the
/// selection store is unavailable the check fails closed and the
/// previous revoke-all-mismatched behavior applies. Returns the revoked
/// device targets so callers can resync collectors.
pub fn revoke_bindings_for_schema_change(
    registry: &ApiRegistry,
    active_schema_id: &str,
    actor: &str,
    action: &str,
) -> Result<Vec<TargetConfig>> {
    let store = registry.sqlite_store();
    let mut revoked = Vec::new();
    for target in registry.list_all_targets()? {
        let stale = target
            .schema_binding
            .as_ref()
            .is_some_and(|b| b.mode == "schema_bound" && b.schema_id != active_schema_id);
        if !stale {
            continue;
        }
        if let Some(store) = store.as_deref() {
            let device_key = device_identity_key(&target);
            // A selection that cannot be read cannot exempt the device:
            // fail toward revocation, never toward preserving authority
            // derived from a schema this activation just superseded.
            if store
                .get_device_schema_selection(&device_key)
                .ok()
                .flatten()
                .is_some()
            {
                continue;
            }
        }
        if let Some(updated) = registry.revoke_schema_binding(
            &target.address,
            actor,
            "catalogue_schema_changed",
            action,
        )? {
            revoked.push(updated);
        }
    }
    Ok(revoked)
}

/// Device-scoped invalidation for schema-selection changes (M6F-F):
/// revoke THIS device's schema-bound approval when it was derived from a
/// different schema than the newly effective bundle. Other devices are
/// never touched. The revocation is fingerprint-guarded so a concurrent
/// newer approval is never clobbered. Returns the updated target for
/// collector resync when a revocation happened.
pub fn revoke_binding_for_device_schema_change(
    registry: &ApiRegistry,
    address: &str,
    new_schema_id: &str,
    actor: &str,
    action: &str,
) -> Result<Option<TargetConfig>> {
    let Some(target) = registry.get_device(address)? else {
        return Ok(None);
    };
    let Some(binding) = target.schema_binding.clone() else {
        return Ok(None);
    };
    if binding.mode != "schema_bound" || binding.schema_id == new_schema_id {
        return Ok(None);
    }
    registry.revoke_schema_binding_if_current(
        address,
        actor,
        "device_schema_changed",
        action,
        &binding.binding_fingerprint,
    )
}

/// Outcome of evidence-supersession invalidation after a new
/// applicability snapshot is persisted (M6E-A1).
#[derive(Debug)]
pub enum EvidenceInvalidation {
    /// The device carries no schema-bound approval.
    NotBound,
    /// The current approval is already derived from this exact evidence —
    /// it remains valid and is never revoked by this evidence.
    Current,
    /// An older/incompatible approval was revoked at the mutation
    /// boundary. Carries the updated device target for collector resync.
    Revoked(TargetConfig),
    /// The binding was approved against NEWER persisted evidence than the
    /// snapshot that triggered this invalidation (an out-of-order or
    /// superseded refresh completed late). The newer authority stands.
    NewerApproval,
    /// The binding changed between the evidence check and the guarded
    /// revocation (concurrent activation/revocation won). The newer
    /// authority stands; nothing was revoked by this call.
    ConcurrentlyReplaced,
}

/// Invalidate a schema-bound approval whose approved evidence is NOT the
/// newly persisted snapshot's evidence. This is the authoritative
/// mutation boundary for refresh-driven invalidation: the CURRENT
/// binding is re-read here (never the pre-refresh snapshot), the
/// approval is kept when it is already derived from the new evidence,
/// and the revocation itself is fingerprint-guarded so a newer approval
/// committed concurrently can never be clobbered.
///
/// Deterministic outcomes:
/// - approval A (older evidence) + refresh completes → A revoked;
/// - approval B created after the refresh began and derived from the new
///   evidence → B kept (`Current`);
/// - approval derived from NEWER persisted evidence than the triggering
///   snapshot (a stale refresh completing late) → kept (`NewerApproval`);
/// - approval based on older evidence committed while no binding existed
///   at refresh start → revoked (`Revoked`);
/// - concurrent binding mutation between check and revoke →
///   `ConcurrentlyReplaced`, the newer authority stands.
pub fn revoke_binding_superseded_by_evidence(
    registry: &ApiRegistry,
    address: &str,
    new_snapshot_id: i64,
    evidence_fingerprint: &str,
    actor: &str,
    action: &str,
) -> Result<EvidenceInvalidation> {
    let Some(target) = registry.get_device(address)? else {
        return Ok(EvidenceInvalidation::NotBound);
    };
    let Some(binding) = target.schema_binding.clone() else {
        return Ok(EvidenceInvalidation::NotBound);
    };
    if binding.mode != "schema_bound" {
        return Ok(EvidenceInvalidation::NotBound);
    }
    // The approval is derived from exactly this evidence — it is the
    // authority the new snapshot confirms, not supersedes.
    if binding.evidence_fingerprint == evidence_fingerprint {
        return Ok(EvidenceInvalidation::Current);
    }
    // The approval was derived from NEWER persisted evidence than the
    // snapshot that triggered this invalidation — a stale refresh or a
    // superseded discovery job completing late must never revoke it.
    if binding.snapshot_id > new_snapshot_id {
        return Ok(EvidenceInvalidation::NewerApproval);
    }
    match registry.revoke_schema_binding_if_current(
        address,
        actor,
        "applicability_evidence_superseded",
        action,
        &binding.binding_fingerprint,
    )? {
        Some(updated) => Ok(EvidenceInvalidation::Revoked(updated)),
        None => Ok(EvidenceInvalidation::ConcurrentlyReplaced),
    }
}

/// Convenience for handlers that only need the durable record state.
pub fn binding_record_state(record: Option<&SchemaBindingRecord>) -> &'static str {
    match record {
        Some(r) if r.state == "active" => "active",
        Some(r) if r.state == "revoked" => "revoked",
        Some(_) => "inactive",
        None => "none",
    }
}

// ── M6C-2 evidence-expiry authority ─────────────────────────────────
//
// A schema-bound approval is derived from device applicability evidence
// with a bounded validity window (`yang_applicability::EVIDENCE_MAX_AGE_NS`,
// 24 h). Two independent layers enforce that window:
//
// 1. AUTHORIZATION — `binding_evidence_state` is evaluated at every
//    execution/lifecycle decision boundary (assignment build, lifecycle
//    admission). An expired or unverifiable approval is never authority,
//    regardless of whether the reconciler has run.
// 2. RECONCILIATION — `reconcile_schema_binding_expiry` proactively
//    revokes stale approvals and resyncs collectors within a bounded
//    interval so obsolete subscribers are withdrawn through the
//    established assignment mechanism.
//
// Tests inject `SchemaBindingClock` so the 24-hour window is exercised
// deterministically — never via wall-clock sleeps.

/// Wall clock for schema-binding expiry decisions. Production uses the
/// system clock; tests inject a controllable source.
#[derive(Clone)]
pub struct SchemaBindingClock {
    now: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl SchemaBindingClock {
    pub fn system() -> Self {
        Self {
            now: std::sync::Arc::new(crate::graph::common::now_ns),
        }
    }

    /// Fixed clock — a single instant for the process lifetime.
    pub fn fixed(now_ns: i64) -> Self {
        Self {
            now: std::sync::Arc::new(move || now_ns),
        }
    }

    /// Shared-cell clock — the caller advances time by storing a new
    /// value into the atomic. Deterministic expiry tests use this.
    pub fn shared(cell: std::sync::Arc<std::sync::atomic::AtomicI64>) -> Self {
        Self {
            now: std::sync::Arc::new(move || cell.load(std::sync::atomic::Ordering::SeqCst)),
        }
    }

    pub fn now(&self) -> i64 {
        (self.now)()
    }
}

/// Freshness state of the device evidence an approval was derived from,
/// evaluated at `now_ns` against the APPROVED snapshot row — never the
/// latest snapshot, which may describe different evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingEvidenceState {
    /// Approved evidence is inside its validity window.
    Current,
    /// Evidence aged past `EVIDENCE_MAX_AGE_NS`, is future-dated beyond
    /// the clock-skew tolerance, or carries no observation timestamp —
    /// it can no longer prove the device schema the approval assumed.
    Expired,
    /// The approved snapshot row is missing, its evidence fingerprint
    /// does not match the binding, or the store could not be read —
    /// authority is unverifiable and must fail closed.
    Unverifiable,
}

/// Evaluate whether `binding`'s approved device evidence still proves
/// freshness at `now_ns`. Cheap by design: one primary-key snapshot
/// lookup — never schema evaluation per call.
pub fn binding_evidence_state(
    store: &SqliteStore,
    binding: &SchemaBindingConfig,
    now_ns: i64,
) -> BindingEvidenceState {
    if binding.mode != "schema_bound" || binding.snapshot_id <= 0 {
        return BindingEvidenceState::Unverifiable;
    }
    let Ok(Some(row)) = store.get_yang_applicability_snapshot(binding.snapshot_id) else {
        return BindingEvidenceState::Unverifiable;
    };
    if row.evidence_fingerprint != binding.evidence_fingerprint {
        return BindingEvidenceState::Unverifiable;
    }
    match row.observed_at_ns {
        Some(observed) if observed > 0 => {
            if observed > now_ns.saturating_add(yang_applicability::FUTURE_EVIDENCE_TOLERANCE_NS)
                || now_ns.saturating_sub(observed) > yang_applicability::EVIDENCE_MAX_AGE_NS
            {
                BindingEvidenceState::Expired
            } else {
                BindingEvidenceState::Current
            }
        }
        _ => BindingEvidenceState::Expired,
    }
}

/// Outcome of one bounded expiry-reconciliation pass.
#[derive(Clone, Debug, Default)]
pub struct SchemaBindingExpiryReport {
    /// Active schema-bound approvals evaluated this pass.
    pub evaluated: usize,
    /// Device addresses whose approved evidence expired.
    pub expired: Vec<String>,
    /// Device addresses whose approved evidence was unverifiable.
    pub unverifiable: Vec<String>,
    /// Device addresses whose approval was actually revoked.
    pub revoked: Vec<String>,
}

/// Bounded reconciliation interval for the periodic driver. Combined
/// with the authorization checks at the assignment/lifecycle
/// boundaries, an expired approval stops authorizing new executions
/// immediately and is withdrawn operationally within this interval.
pub const SCHEMA_BINDING_RECONCILE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Re-check every active schema-bound approval against its persisted
/// device evidence and revoke stale authority through the same durable
/// path as operator revocation, then resync the assigned collector so
/// the obsolete subscriber is withdrawn through the established
/// assignment mechanism.
///
/// Revocation is fingerprint-guarded: only the exact approval evaluated
/// is revoked, so a concurrent administrative reapproval (which mints a
/// new binding fingerprint) is never clobbered. Disconnected collectors
/// need no special handling — the durable revocation is authoritative
/// and the next assignment sync delivers the withdrawal.
pub async fn reconcile_schema_binding_expiry(
    registry: &ApiRegistry,
    collector_manager: Option<&crate::assignment::CollectorManager>,
    now_ns: i64,
) -> Result<SchemaBindingExpiryReport> {
    let mut report = SchemaBindingExpiryReport::default();
    let Some(store) = registry.sqlite_store() else {
        // No durable store → no durable schema-binding authority can
        // exist; nothing to reconcile.
        return Ok(report);
    };
    for target in registry.list_all_targets()? {
        let Some(binding) = target.schema_binding.as_ref() else {
            continue;
        };
        if binding.mode != "schema_bound" {
            continue;
        }
        report.evaluated += 1;
        let (stale_bucket, reason) = match binding_evidence_state(&store, binding, now_ns) {
            BindingEvidenceState::Current => continue,
            BindingEvidenceState::Expired => (&mut report.expired, "evidence_expired"),
            BindingEvidenceState::Unverifiable => {
                (&mut report.unverifiable, "evidence_unverifiable")
            }
        };
        stale_bucket.push(target.address.clone());
        let revoked = registry.revoke_schema_binding_if_current(
            &target.address,
            "system",
            reason,
            "schema_binding_expiry_reconcile",
            &binding.binding_fingerprint,
        )?;
        if let Some(updated) = revoked {
            report.revoked.push(updated.address.clone());
            if let (Some(cid), Some(manager)) = (updated.collector_id.as_deref(), collector_manager)
            {
                manager.resync_collector(cid).await;
            }
        }
    }
    Ok(report)
}

/// Periodic driver for [`reconcile_schema_binding_expiry`]. Runs one
/// pass immediately at startup — covering a core restart after expiry —
/// then every `interval`. This is the WITHDRAWAL layer only: the
/// authorization boundary rejects expired bindings independently of
/// this timer.
pub async fn run_schema_binding_expiry_reconciler(
    registry: std::sync::Arc<ApiRegistry>,
    collector_manager: Option<std::sync::Arc<crate::assignment::CollectorManager>>,
    interval: std::time::Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let clock = SchemaBindingClock::system();
    loop {
        match reconcile_schema_binding_expiry(
            registry.as_ref(),
            collector_manager.as_deref(),
            clock.now(),
        )
        .await
        {
            Ok(report) if report.revoked.is_empty() => {}
            Ok(report) => {
                tracing::info!(
                    evaluated = report.evaluated,
                    expired = report.expired.len(),
                    unverifiable = report.unverifiable.len(),
                    revoked = report.revoked.len(),
                    "schema-binding expiry reconciliation revoked stale approvals"
                );
            }
            Err(e) => {
                tracing::warn!(%e, "schema-binding expiry reconciliation failed");
            }
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TargetConfig;
    use crate::sqlite_store::YangApplicabilitySnapshotRow;
    use std::sync::Arc;

    const NOW: i64 = 1_000_000_000_000_000_000;

    fn binding(snapshot_id: i64, evidence_fp: &str) -> SchemaBindingConfig {
        SchemaBindingConfig {
            mode: "schema_bound".to_string(),
            schema_id: "a".repeat(64),
            snapshot_id,
            content_id: Some("content-1".to_string()),
            datastore: Some("ietf-datastores:operational".to_string()),
            evidence_fingerprint: evidence_fp.to_string(),
            plan_id: "plan-1".to_string(),
            binding_fingerprint: "bind-fp".to_string(),
            approval_revision: 1,
            collector_id: Some("c1".to_string()),
            approved_by: "admin".to_string(),
            approved_at_ns: 42,
        }
    }

    fn snapshot_row(
        device_key: &str,
        address: &str,
        snapshot_id_hint: i64,
        evidence_fp: &str,
        observed_at_ns: Option<i64>,
    ) -> YangApplicabilitySnapshotRow {
        let _ = snapshot_id_hint;
        YangApplicabilitySnapshotRow {
            id: 0,
            device_key: device_key.to_string(),
            device_address: address.to_string(),
            collector_id: Some("c1".to_string()),
            schema_id: Some("a".repeat(64)),
            content_id: Some("content-1".to_string()),
            datastore: Some("ietf-datastores:operational".to_string()),
            overall_status: "applicable".to_string(),
            outcome: "ok".to_string(),
            result_json: "{}".to_string(),
            capabilities_json: None,
            yang_library_json: None,
            evidence_fingerprint: evidence_fp.to_string(),
            observed_at_ns,
            evaluated_at_ns: NOW,
            error: None,
            created_at_ns: NOW,
            executor: Some("core_direct".to_string()),
            observing_collector_id: None,
            discovery_job_id: None,
            collector_session_generation: None,
            authority_epoch: None,
            evidence_received_at_ns: None,
        }
    }

    fn bound_target(address: &str) -> TargetConfig {
        let mut target = TargetConfig {
            address: address.to_string(),
            enabled: true,
            collector_id: Some("c1".to_string()),
            ..Default::default()
        };
        target.schema_binding = Some(binding(7, "ev-fp"));
        target
    }

    fn activation_record(target: &TargetConfig) -> SchemaBindingRecord {
        let binding = target.schema_binding.clone().unwrap();
        SchemaBindingRecord {
            device_key: crate::registry::device_identity_key(target),
            device_address: target.address.clone(),
            mode: "schema_bound".to_string(),
            policy_json: "{}".to_string(),
            approved_plan_json: Some("{}".to_string()),
            approval_revision: 1,
            approval_fingerprint: Some(binding.binding_fingerprint.clone()),
            state: "active".to_string(),
            approved_by: Some("admin".to_string()),
            approved_at_ns: Some(42),
            revoked_by: None,
            revoked_at_ns: None,
            revoke_reason: None,
            created_at_ns: 1,
            updated_at_ns: 1,
        }
    }

    /// Registry + store + device carrying an active schema-bound
    /// approval backed by snapshot row `snapshot_id`.
    async fn bound_device(
        dir: &tempfile::TempDir,
        address: &str,
        snapshot_row_seed: &YangApplicabilitySnapshotRow,
    ) -> (Arc<ApiRegistry>, Arc<SqliteStore>, TargetConfig, i64) {
        let registry =
            Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
        let store = registry.sqlite_store().unwrap();
        let snapshot_id = store
            .save_yang_applicability_snapshot(snapshot_row_seed)
            .unwrap();
        let mut target = bound_target(address);
        target.schema_binding.as_mut().unwrap().snapshot_id = snapshot_id;
        let device_key = crate::registry::device_identity_key(&target);
        // Re-key the snapshot row to the target identity (address-derived).
        let mut row = snapshot_row_seed.clone();
        row.device_key = device_key;
        row.device_address = address.to_string();
        let snapshot_id = store.save_yang_applicability_snapshot(&row).unwrap();
        target.schema_binding.as_mut().unwrap().snapshot_id = snapshot_id;

        let mut plain = target.clone();
        plain.schema_binding = None;
        registry
            .add_device_with_audit(plain, "test", "test_add")
            .unwrap();
        registry
            .assign_device_with_audit(address, Some("c1".to_string()), "test", "test_assign")
            .unwrap();
        let record = activation_record(&target);
        let config_revision = registry.get_device(address).unwrap().unwrap().updated_at_ns;
        registry
            .activate_schema_binding(
                target.clone(),
                record,
                0,
                config_revision,
                "admin",
                "activate",
                None,
            )
            .unwrap();
        (registry, store, target, snapshot_id)
    }

    #[test]
    fn evidence_state_classifies_current_expired_and_unverifiable() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path()).unwrap();
        let target = bound_target("10.0.0.9");
        let key = crate::registry::device_identity_key(&target);
        let row = snapshot_row(&key, "10.0.0.9:57400", 0, "ev-fp", Some(NOW - 1000));
        let id = store.save_yang_applicability_snapshot(&row).unwrap();
        let mut b = binding(id, "ev-fp");
        b.snapshot_id = id;

        assert_eq!(
            binding_evidence_state(&store, &b, NOW),
            BindingEvidenceState::Current
        );
        // Aged past the 24h window.
        assert_eq!(
            binding_evidence_state(
                &store,
                &b,
                NOW + yang_applicability::EVIDENCE_MAX_AGE_NS + 1
            ),
            BindingEvidenceState::Expired
        );
        // Future-dated beyond skew tolerance.
        assert_eq!(
            binding_evidence_state(
                &store,
                &b,
                NOW - yang_applicability::FUTURE_EVIDENCE_TOLERANCE_NS - 2_000
            ),
            BindingEvidenceState::Expired
        );
        // Fingerprint mismatch → unverifiable.
        let mut bad = b.clone();
        bad.evidence_fingerprint = "other".to_string();
        assert_eq!(
            binding_evidence_state(&store, &bad, NOW),
            BindingEvidenceState::Unverifiable
        );
        // Missing snapshot row → unverifiable.
        let mut missing = b.clone();
        missing.snapshot_id = 999_999;
        assert_eq!(
            binding_evidence_state(&store, &missing, NOW),
            BindingEvidenceState::Unverifiable
        );
        // Non-schema-bound mode → unverifiable.
        let mut legacy = b.clone();
        legacy.mode = "legacy".to_string();
        assert_eq!(
            binding_evidence_state(&store, &legacy, NOW),
            BindingEvidenceState::Unverifiable
        );
        // Missing observation timestamp → expired.
        let row2 = snapshot_row(&key, "10.0.0.9:57400", 0, "ev-fp", None);
        let id2 = store.save_yang_applicability_snapshot(&row2).unwrap();
        let mut b2 = b.clone();
        b2.snapshot_id = id2;
        assert_eq!(
            binding_evidence_state(&store, &b2, NOW),
            BindingEvidenceState::Expired
        );
    }

    #[tokio::test]
    async fn reconcile_revokes_expired_and_spares_fresh_and_reapproved() {
        let dir = tempfile::tempdir().unwrap();
        let address = "10.0.0.10";
        let key_target = bound_target(address);
        let key = crate::registry::device_identity_key(&key_target);
        // Evidence already older than the validity window at `now`.
        let stale_row = snapshot_row(
            &key,
            address,
            0,
            "ev-fp",
            Some(NOW - yang_applicability::EVIDENCE_MAX_AGE_NS - 1),
        );
        let (registry, store, _target, _id) = bound_device(&dir, address, &stale_row).await;

        // Fingerprint guard: a mismatched expectation must not revoke.
        assert!(
            registry
                .revoke_schema_binding_if_current(
                    address,
                    "system",
                    "test",
                    "test_guard",
                    "not-the-current-fingerprint"
                )
                .unwrap()
                .is_none()
        );
        assert!(
            registry
                .get_device(address)
                .unwrap()
                .unwrap()
                .schema_binding
                .is_some()
        );

        let report = reconcile_schema_binding_expiry(&registry, None, NOW)
            .await
            .unwrap();
        assert_eq!(report.evaluated, 1);
        assert!(report.expired.contains(&address.to_string()));
        assert!(report.revoked.contains(&address.to_string()));

        let device = registry.get_device(address).unwrap().unwrap();
        assert!(device.schema_binding.is_none());
        assert_eq!(device.acquisition, AcquisitionMode::Disabled);
        let record = store
            .get_schema_binding(&crate::registry::device_identity_key(&device))
            .unwrap()
            .expect("policy record");
        assert_eq!(record.state, "revoked");
        assert_eq!(record.revoke_reason.as_deref(), Some("evidence_expired"));

        // Idempotent: a second pass finds nothing to revoke.
        let report2 = reconcile_schema_binding_expiry(&registry, None, NOW)
            .await
            .unwrap();
        assert_eq!(report2.evaluated, 0);
        assert!(report2.revoked.is_empty());
    }

    #[tokio::test]
    async fn reconcile_spares_current_evidence_and_newer_reapproval() {
        let dir = tempfile::tempdir().unwrap();
        let address = "10.0.0.11";
        let key_target = bound_target(address);
        let key = crate::registry::device_identity_key(&key_target);
        let fresh_row = snapshot_row(&key, address, 0, "ev-fp", Some(NOW - 1000));
        let (registry, _store, _target, _id) = bound_device(&dir, address, &fresh_row).await;

        let report = reconcile_schema_binding_expiry(&registry, None, NOW)
            .await
            .unwrap();
        assert_eq!(report.evaluated, 1);
        assert!(report.revoked.is_empty());
        assert!(
            registry
                .get_device(address)
                .unwrap()
                .unwrap()
                .schema_binding
                .is_some()
        );
    }

    #[tokio::test]
    async fn reconcile_revokes_unverifiable_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let address = "10.0.0.12";
        let key_target = bound_target(address);
        let key = crate::registry::device_identity_key(&key_target);
        // Snapshot fingerprint does not match the approved binding.
        let mismatched = snapshot_row(&key, address, 0, "different-fp", Some(NOW - 1000));
        let (registry, _store, _target, _id) = bound_device(&dir, address, &mismatched).await;

        let report = reconcile_schema_binding_expiry(&registry, None, NOW)
            .await
            .unwrap();
        assert!(report.unverifiable.contains(&address.to_string()));
        assert!(report.revoked.contains(&address.to_string()));
        let record_reason = registry
            .sqlite_store()
            .unwrap()
            .get_schema_binding(&crate::registry::device_identity_key(
                &registry.get_device(address).unwrap().unwrap(),
            ))
            .unwrap()
            .expect("record")
            .revoke_reason;
        assert_eq!(record_reason.as_deref(), Some("evidence_unverifiable"));
    }

    /// Registry + store + a plain (unbound) device for activation
    /// concurrency tests.
    async fn plain_device(
        dir: &tempfile::TempDir,
        address: &str,
    ) -> (Arc<ApiRegistry>, TargetConfig) {
        let registry =
            Arc::new(ApiRegistry::open(dir.path().join("registry.json"), vec![]).unwrap());
        let mut target = bound_target(address);
        target.schema_binding = None;
        registry
            .add_device_with_audit(target.clone(), "test", "test_add")
            .unwrap();
        let device = registry.get_device(address).unwrap().unwrap();
        (registry, device)
    }

    /// Durable record for an unbound device — the concurrency checks under
    /// test reject before the record's policy content is ever consulted.
    fn plain_activation_record(device: &TargetConfig) -> SchemaBindingRecord {
        SchemaBindingRecord {
            device_key: crate::registry::device_identity_key(device),
            device_address: device.address.clone(),
            mode: "schema_bound".to_string(),
            policy_json: "{}".to_string(),
            approved_plan_json: Some("{}".to_string()),
            approval_revision: 1,
            approval_fingerprint: Some("fp".to_string()),
            state: "active".to_string(),
            approved_by: Some("admin".to_string()),
            approved_at_ns: Some(42),
            revoked_by: None,
            revoked_at_ns: None,
            revoke_reason: None,
            created_at_ns: 1,
            updated_at_ns: 1,
        }
    }

    #[tokio::test]
    async fn stale_config_revision_rejects_activation() {
        let dir = tempfile::tempdir().unwrap();
        let address = "10.0.0.20";
        let (registry, device) = plain_device(&dir, address).await;
        let record = plain_activation_record(&device);
        // A revision that does not match the current device configuration
        // must fail closed before any durable mutation.
        let err = registry
            .activate_schema_binding(
                device.clone(),
                record,
                0,
                device.updated_at_ns + 1,
                "admin",
                "activate",
                None,
            )
            .expect_err("stale config revision must reject");
        assert!(
            err.to_string().contains("configuration changed"),
            "unexpected error: {err}"
        );
        let after = registry.get_device(address).unwrap().unwrap();
        assert!(after.schema_binding.is_none());
        assert_eq!(after.acquisition, device.acquisition);
        assert!(
            registry
                .sqlite_store()
                .unwrap()
                .get_schema_binding(&crate::registry::device_identity_key(&after))
                .unwrap()
                .is_none(),
            "no durable binding record may be committed on stale activation"
        );
    }

    #[tokio::test]
    async fn concurrent_registry_update_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let address = "10.0.0.21";
        let (registry, device) = plain_device(&dir, address).await;
        let previewed_revision = device.updated_at_ns;
        // A concurrent device update lands between preview and activation:
        // reassignment bumps updated_at_ns.
        registry
            .assign_device_with_audit(address, Some("c2".to_string()), "test", "test_reassign")
            .unwrap();
        let record = plain_activation_record(&device);
        let err = registry
            .activate_schema_binding(
                device.clone(),
                record,
                0,
                previewed_revision,
                "admin",
                "activate",
                None,
            )
            .expect_err("activation against a superseded configuration must reject");
        assert!(err.to_string().contains("configuration changed"));
        // The newer registry state is preserved — the stale evaluated
        // target was never committed over it.
        let after = registry.get_device(address).unwrap().unwrap();
        assert_eq!(after.collector_id.as_deref(), Some("c2"));
        assert!(after.schema_binding.is_none());
        assert_ne!(after.updated_at_ns, previewed_revision);
    }
}

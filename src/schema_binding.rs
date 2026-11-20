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
use crate::yang_schema;
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
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaPlanPreview {
    pub device_key: String,
    pub device_address: String,
    pub evaluation: Option<SchemaPlanEvaluation>,
    pub activation_permitted: bool,
    pub catalogue_state: Option<ReasonCode>,
    pub active_schema_id: Option<String>,
    pub freshness: Option<SnapshotFreshness>,
    pub freshness_reasons: Vec<ReasonCode>,
    pub current_binding: Option<SchemaBindingConfig>,
    pub binding_state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaBindingActivation {
    pub device: TargetConfig,
    pub record: SchemaBindingRecord,
    pub evaluation: SchemaPlanEvaluation,
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
    let state = yang_schema::active_catalogue_state(library);
    let view = yang_applicability::load_snapshot_view(store, library, target, now_ns)?;

    let freshness = view.as_ref().map(|v| v.freshness);
    let freshness_reasons = view
        .as_ref()
        .map(|v| v.freshness_reasons.clone())
        .unwrap_or_default();
    let active_schema_id = state
        .schema_id()
        .map(str::to_string)
        .or_else(|| view.as_ref().and_then(|v| v.active_schema_id.clone()));
    let catalogue_state = state.reason_code();

    let mut evaluation = None;
    if let Some(catalogue) = state.catalogue() {
        let input = SchemaPlanInput {
            catalogue,
            snapshot: view.as_ref().map(|v| &v.snapshot),
            freshness,
            freshness_reasons: &freshness_reasons,
            target,
            selected_paths: &selected_paths,
            datastore,
            collector_id: target.collector_id.as_deref(),
            expected_config_revision: Some(target.updated_at_ns),
            now_ns,
        };
        evaluation = Some(acquisition::evaluate_schema_plan(&input));
    }

    let binding_state = match store.get_schema_binding(&device_identity_key(target))? {
        Some(record) if record.state == "active" => "active".to_string(),
        Some(record) if record.state == "revoked" => "revoked".to_string(),
        Some(_) => "inactive".to_string(),
        None => "none".to_string(),
    };

    Ok(SchemaPlanPreview {
        device_key: device_identity_key(target),
        device_address: target.address.clone(),
        activation_permitted: evaluation
            .as_ref()
            .map(|e| e.activation_permitted)
            .unwrap_or(false),
        evaluation,
        catalogue_state,
        active_schema_id,
        freshness,
        freshness_reasons,
        current_binding: target.schema_binding.clone(),
        binding_state,
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
    })
}

/// Revoke every active schema-bound approval whose approved schema is not
/// the currently active compiled catalogue. Catalogue activation is an
/// authority mutation: a binding derived from a different schema_id can
/// never remain executable. Returns the revoked device targets so callers
/// can resync collectors.
pub fn revoke_bindings_for_schema_change(
    registry: &ApiRegistry,
    active_schema_id: &str,
    actor: &str,
    action: &str,
) -> Result<Vec<TargetConfig>> {
    let mut revoked = Vec::new();
    for target in registry.list_all_targets()? {
        let stale = target
            .schema_binding
            .as_ref()
            .is_some_and(|b| b.mode == "schema_bound" && b.schema_id != active_schema_id);
        if !stale {
            continue;
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

/// Convenience for handlers that only need the durable record state.
pub fn binding_record_state(record: Option<&SchemaBindingRecord>) -> &'static str {
    match record {
        Some(r) if r.state == "active" => "active",
        Some(r) if r.state == "revoked" => "revoked",
        Some(_) => "inactive",
        None => "none",
    }
}

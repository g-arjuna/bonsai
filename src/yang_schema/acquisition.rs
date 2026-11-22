//! M6C-2 schema-bound acquisition planning.
//!
//! Deterministic, read-only evaluation of an explicit operator path
//! selection against verified catalogue + fresh device applicability
//! evidence. This module never performs I/O and never mutates registry,
//! collector, or subscription state.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{AcquisitionMode, SelectedSubscriptionPath, TargetConfig};
use crate::gnmi_path;
use crate::yang_applicability::{self, ApplicabilitySnapshot, SnapshotFreshness};
use crate::yang_schema::applicability::{ApplicabilityStatus, PathResult, ReasonCode};
use crate::yang_schema::{CompiledCatalogue, SchemaPathEntry};

pub const SCHEMA_PLAN_VERSION: u32 = 1;
pub const MAX_SELECTED_PATHS: usize = 512;
pub const MAX_PLAN_WARNINGS: usize = 128;

#[derive(Clone, Debug)]
pub struct SchemaPlanInput<'a> {
    pub catalogue: &'a CompiledCatalogue,
    pub snapshot: Option<&'a ApplicabilitySnapshot>,
    pub freshness: Option<SnapshotFreshness>,
    pub freshness_reasons: &'a [ReasonCode],
    pub target: &'a TargetConfig,
    pub selected_paths: &'a [SelectedSubscriptionPath],
    pub datastore: Option<&'a str>,
    pub collector_id: Option<&'a str>,
    pub expected_config_revision: Option<i64>,
    pub now_ns: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDecisionKind {
    Eligible,
    CandidateOnly,
    UnknownFeatureState,
    UnsupportedDeviation,
    ModuleRevisionMismatch,
    NotApplicableDatastore,
    SchemaPathAmbiguous,
    ListKeysUnresolved,
    OriginMappingUnresolved,
    ConfigurationDataExcluded,
    DuplicateConflict,
    ResourceBudgetExceeded,
    Rejected,
}

impl PathDecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::CandidateOnly => "candidate_only",
            Self::UnknownFeatureState => "unknown_feature_state",
            Self::UnsupportedDeviation => "unsupported_deviation",
            Self::ModuleRevisionMismatch => "module_revision_mismatch",
            Self::NotApplicableDatastore => "not_applicable_datastore",
            Self::SchemaPathAmbiguous => "schema_path_ambiguous",
            Self::ListKeysUnresolved => "list_keys_unresolved",
            Self::OriginMappingUnresolved => "origin_mapping_unresolved",
            Self::ConfigurationDataExcluded => "configuration_data_excluded",
            Self::DuplicateConflict => "duplicate_conflict",
            Self::ResourceBudgetExceeded => "resource_budget_exceeded",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathDecision {
    pub index: usize,
    pub selected_path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
    pub kind: PathDecisionKind,
    pub eligible: bool,
    pub schema_path: Option<String>,
    pub module: Option<String>,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposedSubscription {
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
    pub rationale: String,
    pub optional: bool,
    pub provenance: String,
    pub schema_path: String,
    pub module: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaPlanEvaluation {
    pub plan_version: u32,
    pub device_key: String,
    pub device_address: String,
    pub schema_id: String,
    pub content_id: Option<String>,
    pub snapshot_id: Option<i64>,
    pub evidence_fingerprint: String,
    pub datastore: Option<String>,
    pub collector_id: Option<String>,
    pub acquisition_mode: String,
    pub selected_paths: Vec<SelectedSubscriptionPath>,
    pub proposed_subscriptions: Vec<ProposedSubscription>,
    pub decisions: Vec<PathDecision>,
    pub rejection_reasons: Vec<String>,
    pub warnings: Vec<String>,
    pub plan_id: String,
    pub binding_fingerprint: String,
    pub activation_permitted: bool,
    /// Registry configuration revision (`TargetConfig::updated_at_ns`)
    /// this evaluation was computed against. Activation must present the
    /// same revision as `expected_config_revision`; a device update
    /// between preview and activation changes it and fails closed.
    pub config_revision: i64,
    pub estimated_initial_collection: Estimate,
    pub estimated_ongoing_collection: Estimate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Estimate {
    pub kind: String,
    pub value: u64,
    pub basis: String,
}

fn estimate(kind: &str, value: u64, basis: &str) -> Estimate {
    Estimate {
        kind: kind.to_string(),
        value,
        basis: basis.to_string(),
    }
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update(part.as_bytes());
        h.update([0]);
    }
    format!("{:x}", h.finalize())
}

fn canonical_manifest_key(entries: &[(String, String, String, u64)]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort();
    let mut out = String::new();
    for (origin, path, mode, interval) in sorted {
        out.push_str(&origin);
        out.push('\x1f');
        out.push_str(&path);
        out.push('\x1f');
        out.push_str(&mode);
        out.push('\x1f');
        out.push_str(&interval.to_string());
        out.push('\x1e');
    }
    out
}

fn plan_id_for(acquisition: AcquisitionMode, entries: &[(String, String, String, u64)]) -> String {
    crate::subscriber::acquisition_plan_id(acquisition, entries)
}

/// Structural schema-template segment: `name[key][key2]`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TemplateSegment {
    name: String,
    keys: Vec<String>,
}

fn parse_schema_template(path: &str) -> Option<Vec<TemplateSegment>> {
    let mut out = Vec::new();
    for raw in path.split('/').filter(|s| !s.is_empty()) {
        let mut name_end = raw.len();
        for (idx, ch) in raw.char_indices() {
            if ch == '[' {
                name_end = idx;
                break;
            }
        }
        let name = &raw[..name_end];
        if name.is_empty() || name.contains(']') || name.contains('=') {
            return None;
        }
        let mut keys = Vec::new();
        let mut rest = &raw[name_end..];
        while !rest.is_empty() {
            let inner = rest.strip_prefix('[')?;
            let close = inner.find(']')?;
            let key = &inner[..close];
            if key.is_empty() || key.contains('=') || key.contains('[') || key.contains(']') {
                return None;
            }
            keys.push(key.to_string());
            rest = &inner[close + 1..];
        }
        out.push(TemplateSegment {
            name: name.to_string(),
            keys,
        });
    }
    (!out.is_empty()).then_some(out)
}

fn gnmi_template_segments(path: &crate::proto::gnmi::Path) -> Option<Vec<TemplateSegment>> {
    let mut out = Vec::new();
    for elem in &path.elem {
        if elem.name.is_empty() || elem.name.contains('[') || elem.name.contains(']') {
            return None;
        }
        let mut keys: Vec<String> = elem.key.keys().cloned().collect();
        keys.sort();
        out.push(TemplateSegment {
            name: elem.name.clone(),
            keys,
        });
    }
    (!out.is_empty()).then_some(out)
}

fn template_matches(template: &str, selected: &crate::proto::gnmi::Path) -> bool {
    let Some(expected) = parse_schema_template(template) else {
        return false;
    };
    let Some(actual) = gnmi_template_segments(selected) else {
        return false;
    };
    if expected.len() != actual.len() {
        return false;
    }
    expected.iter().zip(actual.iter()).all(|(e, a)| {
        e.name == a.name
            && e.keys.len() == a.keys.len()
            && e.keys.iter().all(|k| a.keys.contains(k))
    })
}

fn matching_path_results<'a>(
    snapshot: &'a ApplicabilitySnapshot,
    selected: &crate::proto::gnmi::Path,
) -> Vec<&'a PathResult> {
    snapshot
        .result
        .paths
        .iter()
        .filter(|p| template_matches(&p.path, selected))
        .collect()
}

fn catalogue_entry_for<'a>(
    catalogue: &'a CompiledCatalogue,
    schema_path: &str,
) -> Option<&'a SchemaPathEntry> {
    catalogue.paths.iter().find(|p| p.path == schema_path)
}

fn decision(
    index: usize,
    selected: &SelectedSubscriptionPath,
    kind: PathDecisionKind,
    eligible: bool,
    schema_path: Option<String>,
    module: Option<String>,
    reasons: Vec<String>,
    warnings: Vec<String>,
) -> PathDecision {
    PathDecision {
        index,
        selected_path: selected.path.clone(),
        origin: selected.origin.clone(),
        mode: selected.mode.clone(),
        sample_interval_ns: selected.sample_interval_ns,
        kind,
        eligible,
        schema_path,
        module,
        reasons,
        warnings,
    }
}

fn mode_interval_valid(mode: &str, interval: u64) -> Result<(), String> {
    match mode.trim().to_ascii_uppercase().as_str() {
        "SAMPLE" if interval > 0 => Ok(()),
        "SAMPLE" => Err("SAMPLE requires positive sample_interval_ns".to_string()),
        "ON_CHANGE" if interval == 0 => Ok(()),
        "ON_CHANGE" => Err("ON_CHANGE must not carry sample_interval_ns".to_string()),
        other => Err(format!("unsupported mode '{other}'")),
    }
}

fn reject_reasons_from_status(status: ApplicabilityStatus) -> Vec<String> {
    match status {
        ApplicabilityStatus::Applicable => Vec::new(),
        ApplicabilityStatus::Candidate => vec!["candidate_only".to_string()],
        ApplicabilityStatus::NotApplicable => vec!["not_applicable".to_string()],
        ApplicabilityStatus::Unknown => vec!["unknown".to_string()],
        ApplicabilityStatus::Stale => vec!["stale".to_string()],
        ApplicabilityStatus::Conflicted => vec!["conflicted".to_string()],
    }
}

pub fn evaluate_schema_plan(input: &SchemaPlanInput<'_>) -> SchemaPlanEvaluation {
    let target = input.target;
    let device_key = target
        .external_id
        .clone()
        .unwrap_or_else(|| crate::registry::canonical_device_key(&target.address));
    let mut decisions = Vec::new();
    let mut proposed = Vec::new();
    let mut rejection_reasons = Vec::new();
    let mut warnings = Vec::new();
    let mut entries: Vec<(String, String, String, u64)> = Vec::new();
    // Selection identity is the FULL canonical instantiated gNMI identity:
    // origin + element names + list-key names AND values (canonical_key),
    // plus mode and sample interval. Two selections that differ only in
    // list-key VALUES are distinct concrete paths and must both be
    // allowed; the same concrete path with different subscription
    // parameters is a conflict, never a silent merge.
    let mut seen_exact = BTreeSet::new();
    let mut seen_concrete = BTreeSet::new();

    let catalogue_ok = input.catalogue.status == "complete"
        && !input.catalogue.schema_id.is_empty()
        && input.catalogue.diagnostics.is_empty()
        && input.catalogue.paths.len() <= crate::yang_schema::compiler::MAX_PATH_ENTRIES;
    if !catalogue_ok {
        rejection_reasons.push("catalogue_not_authoritative".to_string());
    }
    if target
        .collector_id
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        rejection_reasons.push("missing_collector".to_string());
    }

    let snapshot = input.snapshot;
    let fresh = matches!(input.freshness, Some(SnapshotFreshness::Fresh));
    if !fresh {
        rejection_reasons.push("snapshot_not_fresh".to_string());
        for reason in input.freshness_reasons {
            rejection_reasons.push(format!("freshness:{reason:?}"));
        }
    }
    if let Some(snap) = snapshot {
        if snap.result.result_version
            != crate::yang_schema::applicability::APPLICABILITY_RESULT_VERSION
        {
            rejection_reasons.push("unsupported_result_version".to_string());
        }
        if snap.device_key != device_key || snap.device_address != target.address {
            rejection_reasons.push("device_identity_mismatch".to_string());
        }
        if snap.collector_id != target.collector_id {
            rejection_reasons.push("collector_identity_mismatch".to_string());
        }
        if let Some(expected) = input.collector_id
            && snap.collector_id.as_deref() != Some(expected)
        {
            rejection_reasons.push("collector_identity_mismatch".to_string());
        }
        if let Some(observed) = snap.observed_at_ns {
            if observed <= 0 || observed > input.now_ns.saturating_add(5 * 60 * 1_000_000_000) {
                rejection_reasons.push("invalid_observation_timestamp".to_string());
            }
            if input.now_ns.saturating_sub(observed) > yang_applicability::EVIDENCE_MAX_AGE_NS {
                rejection_reasons.push("evidence_expired".to_string());
            }
        } else {
            rejection_reasons.push("missing_observation_timestamp".to_string());
        }
        if snap.evidence_fingerprint.is_empty() {
            rejection_reasons.push("missing_evidence_fingerprint".to_string());
        }
        if snap.result.paths_truncated {
            rejection_reasons.push("applicability_paths_truncated".to_string());
        }
        if let Some(ds) = input.datastore
            && snap.datastore.as_deref() != Some(ds)
        {
            rejection_reasons.push("datastore_mismatch".to_string());
        }
        if snap.result.overall_status != ApplicabilityStatus::Applicable {
            rejection_reasons.push(format!(
                "overall_status:{}",
                snap.result.overall_status.as_str()
            ));
        }
    } else {
        rejection_reasons.push("missing_snapshot".to_string());
    }

    if input.selected_paths.is_empty() {
        rejection_reasons.push("empty_selection".to_string());
    }
    if input.selected_paths.len() > MAX_SELECTED_PATHS {
        rejection_reasons.push("selection_budget_exceeded".to_string());
    }

    // Optimistic concurrency: when the caller pins the configuration
    // revision it previewed, the CURRENT device revision must match.
    // Any intervening device update (identity, collector, acquisition
    // mode, paths, transport security, credentials, …) bumps
    // `updated_at_ns` and makes the preview stale — never silently
    // activate a different effective configuration.
    if let Some(expected) = input.expected_config_revision
        && expected != target.updated_at_ns
    {
        rejection_reasons.push("config_revision_mismatch".to_string());
    }

    for (index, selected) in input.selected_paths.iter().enumerate() {
        let parsed = gnmi_path::parse_path(&selected.origin, &selected.path);
        let Ok(path) = parsed else {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::Rejected,
                false,
                None,
                None,
                vec!["malformed_gnmi_path".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:malformed_gnmi_path"));
            continue;
        };
        if let Err(error) = mode_interval_valid(&selected.mode, selected.sample_interval_ns) {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::Rejected,
                false,
                None,
                None,
                vec![error],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:invalid_mode_interval"));
            continue;
        }
        let mode = selected.mode.trim().to_ascii_uppercase();
        let exact_key = format!(
            "{}\x1f{}\x1f{}",
            gnmi_path::canonical_key(&path),
            mode,
            selected.sample_interval_ns
        );
        if !seen_exact.insert(exact_key) {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::DuplicateConflict,
                false,
                None,
                None,
                vec!["duplicate_path".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:duplicate_path"));
            continue;
        }
        // The concrete-path identity preserves every list-key value, so
        // distinct instances of one schema template never collapse. A
        // second selection of the SAME concrete path with different mode
        // or interval is a conflicting duplicate.
        let concrete_key = gnmi_path::canonical_key(&path);
        if !seen_concrete.insert(concrete_key) {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::DuplicateConflict,
                false,
                None,
                None,
                vec!["conflicting_duplicate_path".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:conflicting_duplicate_path"));
            continue;
        }

        let Some(snap) = snapshot else {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::Rejected,
                false,
                None,
                None,
                vec!["missing_snapshot".to_string()],
                Vec::new(),
            ));
            continue;
        };
        let matches = matching_path_results(snap, &path);
        if matches.is_empty() {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::SchemaPathAmbiguous,
                false,
                None,
                None,
                vec!["no_exact_schema_path_result".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:no_exact_schema_path_result"));
            continue;
        }
        if matches.len() > 1 {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::SchemaPathAmbiguous,
                false,
                None,
                None,
                vec!["ambiguous_schema_path".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:ambiguous_schema_path"));
            continue;
        }
        let result = matches[0];
        let schema_path = result.path.clone();
        let module = result.module.clone();
        if result.config {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::ConfigurationDataExcluded,
                false,
                Some(schema_path),
                Some(module),
                vec!["configuration_data_excluded".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:configuration_data_excluded"));
            continue;
        }
        if result.status != ApplicabilityStatus::Applicable {
            let kind = match result.status {
                ApplicabilityStatus::Candidate => PathDecisionKind::CandidateOnly,
                ApplicabilityStatus::Unknown => PathDecisionKind::UnknownFeatureState,
                ApplicabilityStatus::NotApplicable => PathDecisionKind::NotApplicableDatastore,
                ApplicabilityStatus::Stale => PathDecisionKind::Rejected,
                ApplicabilityStatus::Conflicted => PathDecisionKind::Rejected,
                ApplicabilityStatus::Applicable => unreachable!(),
            };
            decisions.push(decision(
                index,
                selected,
                kind,
                false,
                Some(schema_path),
                Some(module),
                reject_reasons_from_status(result.status),
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:{}", result.status.as_str()));
            continue;
        }
        // List-key correctness is enforced by `template_matches`: a selected
        // gNMI path may only carry key names the schema template declares, and
        // a list segment selected without keys matches no template at all
        // (`no_exact_schema_path_result`). `PathResult::keys` only describes
        // the matched node's own list keys, so it cannot gate ancestor keys.
        let entry = catalogue_entry_for(input.catalogue, &schema_path);
        let mut path_warnings = Vec::new();
        if entry.is_none() {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::SchemaPathAmbiguous,
                false,
                Some(schema_path),
                Some(module),
                vec!["schema_path_not_in_catalogue".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:schema_path_not_in_catalogue"));
            continue;
        }
        if !selected.origin.is_empty() && !gnmi_path::valid_origin(&selected.origin) {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::OriginMappingUnresolved,
                false,
                Some(schema_path),
                Some(module),
                vec!["invalid_origin".to_string()],
                Vec::new(),
            ));
            rejection_reasons.push(format!("path[{index}]:invalid_origin"));
            continue;
        }
        if selected.origin.is_empty() {
            path_warnings.push("origin_unspecified_device_default".to_string());
            warnings.push(format!("path[{index}]:origin_unspecified_device_default"));
        } else if entry.map(|e| e.module_prefix.as_str()) != Some(selected.origin.as_str()) {
            path_warnings.push("origin_operator_declared_not_schema_proven".to_string());
            warnings.push(format!(
                "path[{index}]:origin_operator_declared_not_schema_proven"
            ));
        }
        if result.module_revision != entry.map(|e| e.module_revision.as_str()).unwrap_or("") {
            decisions.push(decision(
                index,
                selected,
                PathDecisionKind::ModuleRevisionMismatch,
                false,
                Some(schema_path),
                Some(module),
                vec!["module_revision_mismatch".to_string()],
                path_warnings,
            ));
            rejection_reasons.push(format!("path[{index}]:module_revision_mismatch"));
            continue;
        }
        let interval = selected.sample_interval_ns;
        entries.push((
            path.origin.clone(),
            crate::subscriber::path_to_string(&path),
            mode.clone(),
            interval,
        ));
        proposed.push(ProposedSubscription {
            path: selected.path.clone(),
            origin: selected.origin.clone(),
            mode,
            sample_interval_ns: interval,
            rationale: selected.rationale.clone(),
            optional: selected.optional,
            provenance: selected.provenance.clone(),
            schema_path: schema_path.clone(),
            module: module.clone(),
        });
        decisions.push(decision(
            index,
            selected,
            PathDecisionKind::Eligible,
            true,
            Some(schema_path),
            Some(module),
            Vec::new(),
            path_warnings,
        ));
    }

    let plan_id = plan_id_for(AcquisitionMode::Explicit, &entries);
    let binding_fingerprint = sha256_hex(&[
        &input.catalogue.schema_id,
        snapshot
            .map(|s| s.evidence_fingerprint.as_str())
            .unwrap_or(""),
        snapshot
            .map(|s| s.snapshot_id.to_string())
            .unwrap_or_default()
            .as_str(),
        snapshot.and_then(|s| s.datastore.as_deref()).unwrap_or(""),
        target.collector_id.as_deref().unwrap_or(""),
        &device_key,
        &canonical_manifest_key(&entries),
        AcquisitionMode::Explicit.as_str(),
    ]);
    let activation_permitted = rejection_reasons.is_empty() && !proposed.is_empty();
    let eligible_count = proposed.len() as u64;
    let max_interval = proposed
        .iter()
        .map(|p| p.sample_interval_ns)
        .max()
        .unwrap_or(0);
    SchemaPlanEvaluation {
        plan_version: SCHEMA_PLAN_VERSION,
        device_key,
        device_address: target.address.clone(),
        schema_id: input.catalogue.schema_id.clone(),
        content_id: snapshot.and_then(|s| s.content_id.clone()),
        snapshot_id: snapshot.map(|s| s.snapshot_id),
        evidence_fingerprint: snapshot
            .map(|s| s.evidence_fingerprint.clone())
            .unwrap_or_default(),
        datastore: input
            .datastore
            .map(str::to_string)
            .or_else(|| snapshot.and_then(|s| s.datastore.clone())),
        collector_id: target
            .collector_id
            .clone()
            .or_else(|| input.collector_id.map(str::to_string)),
        acquisition_mode: AcquisitionMode::Explicit.as_str().to_string(),
        selected_paths: input.selected_paths.to_vec(),
        proposed_subscriptions: proposed,
        decisions,
        rejection_reasons: rejection_reasons.iter().take(256).cloned().collect(),
        warnings: warnings.iter().take(MAX_PLAN_WARNINGS).cloned().collect(),
        plan_id,
        binding_fingerprint,
        activation_permitted,
        config_revision: target.updated_at_ns,
        estimated_initial_collection: estimate(
            "subscription_entries",
            eligible_count,
            "count of approved Subscribe paths; not measured bytes or device CPU",
        ),
        estimated_ongoing_collection: estimate(
            "max_sample_interval_ns",
            max_interval,
            "largest approved SAMPLE interval; ON_CHANGE paths have no periodic sample cost",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yang_schema::applicability::{
        ApplicabilityResult, ApplicabilityStatus, ModuleResult,
    };

    fn target() -> TargetConfig {
        TargetConfig {
            address: "10.0.0.1:57400".to_string(),
            enabled: true,
            collector_id: Some("collector-a".to_string()),
            ..Default::default()
        }
    }

    fn catalogue() -> CompiledCatalogue {
        CompiledCatalogue {
            schema_version: crate::yang_schema::compiler::COMPILED_SCHEMA_VERSION,
            schema_id: "a".repeat(64),
            compiled_at_ns: 1,
            module_count: 1,
            path_count: 1,
            status: "complete".to_string(),
            modules: Vec::new(),
            paths: vec![SchemaPathEntry {
                module: "openconfig-interfaces".to_string(),
                module_revision: "2024-01-01".to_string(),
                namespace: "https://openconfig.net/yang/interfaces".to_string(),
                module_prefix: "oc-if".to_string(),
                path: "/interfaces/interface[name]/state/counters/in-octets".to_string(),
                kind: "leaf".to_string(),
                config: false,
                declared_config: Some(false),
                mandatory: false,
                presence: None,
                description: None,
                type_name: Some("uint64".to_string()),
                resolved_type: None,
                units: Some("bytes".to_string()),
                default: None,
                keys: Vec::new(),
                min_elements: None,
                max_elements: None,
                ordered_by: None,
                required_features: Vec::new(),
                deviated: false,
                deviation_notes: Vec::new(),
                status: None,
            }],
            diagnostics: Vec::new(),
            module_selection: Default::default(),
        }
    }

    fn snapshot() -> ApplicabilitySnapshot {
        ApplicabilitySnapshot {
            snapshot_id: 7,
            device_key: "10.0.0.1".to_string(),
            device_address: "10.0.0.1:57400".to_string(),
            collector_id: Some("collector-a".to_string()),
            schema_id: Some("a".repeat(64)),
            content_id: Some("device-content-1".to_string()),
            datastore: Some("ietf-datastores:operational".to_string()),
            overall_status: "applicable".to_string(),
            outcome: "ok".to_string(),
            result: ApplicabilityResult {
                result_version: crate::yang_schema::applicability::APPLICABILITY_RESULT_VERSION,
                schema_id: Some("a".repeat(64)),
                content_id: Some("device-content-1".to_string()),
                datastore: Some("ietf-datastores:operational".to_string()),
                overall_status: ApplicabilityStatus::Applicable,
                reasons: Vec::new(),
                modules: vec![ModuleResult {
                    name: "openconfig-interfaces".to_string(),
                    catalogue_revision: "2024-01-01".to_string(),
                    device_revision: Some("2024-01-01".to_string()),
                    organization: String::new(),
                    namespace: "https://openconfig.net/yang/interfaces".to_string(),
                    classification:
                        crate::yang_schema::applicability::ModuleClassification::Implemented,
                    enabled_features: Vec::new(),
                    features_state_known: true,
                    deviations: Vec::new(),
                    status: ApplicabilityStatus::Applicable,
                    reasons: Vec::new(),
                }],
                paths: vec![PathResult {
                    path: "/interfaces/interface[name]/state/counters/in-octets".to_string(),
                    module: "openconfig-interfaces".to_string(),
                    module_revision: "2024-01-01".to_string(),
                    namespace: "https://openconfig.net/yang/interfaces".to_string(),
                    kind: "leaf".to_string(),
                    config: false,
                    keys: Vec::new(),
                    type_name: Some("uint64".to_string()),
                    resolved_type: None,
                    units: Some("bytes".to_string()),
                    required_features: Vec::new(),
                    status: ApplicabilityStatus::Applicable,
                    reasons: Vec::new(),
                }],
                paths_total: 1,
                paths_truncated: false,
                diagnostics: Vec::new(),
                evidence_fingerprint: "b".repeat(64),
                evaluated_at_ns: 10,
                evidence_observed_at_ns: Some(10),
            },
            capabilities: None,
            yang_library: None,
            evidence_fingerprint: "b".repeat(64),
            observed_at_ns: Some(10),
            evaluated_at_ns: 10,
            error: None,
            created_at_ns: 10,
            provenance: Default::default(),
        }
    }

    fn selected() -> Vec<SelectedSubscriptionPath> {
        vec![SelectedSubscriptionPath {
            path: "/interfaces/interface[name=ethernet-1/1]/state/counters/in-octets".to_string(),
            origin: "oc-if".to_string(),
            provenance: String::new(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 30_000_000_000,
            rationale: "test".to_string(),
            optional: false,
        }]
    }

    #[test]
    fn positive_evaluation_is_deterministic() {
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        let sel = selected();
        let input = SchemaPlanInput {
            catalogue: &cat,
            snapshot: Some(&snap),
            freshness: Some(SnapshotFreshness::Fresh),
            freshness_reasons: &[],
            target: &tgt,
            selected_paths: &sel,
            datastore: Some("ietf-datastores:operational"),
            collector_id: Some("collector-a"),
            expected_config_revision: None,
            now_ns: 20,
        };
        let first = evaluate_schema_plan(&input);
        let second = evaluate_schema_plan(&input);
        assert!(first.activation_permitted, "{:?}", first.rejection_reasons);
        assert_eq!(first.plan_id, second.plan_id);
        assert_eq!(first.binding_fingerprint, second.binding_fingerprint);
        // The approved plan identity must be the same normalized manifest
        // identity the subscriber submits on the wire — never a raw
        // selected_paths spelling that lifecycle fencing cannot match.
        let manifest_entries = vec![(
            "oc-if".to_string(),
            "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets".to_string(),
            "SAMPLE".to_string(),
            30_000_000_000_u64,
        )];
        assert_eq!(
            first.plan_id,
            crate::subscriber::acquisition_plan_id(
                crate::config::AcquisitionMode::Explicit,
                &manifest_entries
            )
        );
        assert_eq!(first.proposed_subscriptions.len(), 1);
        assert_eq!(first.decisions[0].kind, PathDecisionKind::Eligible);
    }

    #[test]
    fn stale_snapshot_rejects_activation() {
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        let sel = selected();
        let reasons = vec![ReasonCode::EvidenceExpired];
        let input = SchemaPlanInput {
            catalogue: &cat,
            snapshot: Some(&snap),
            freshness: Some(SnapshotFreshness::Stale),
            freshness_reasons: &reasons,
            target: &tgt,
            selected_paths: &sel,
            datastore: None,
            collector_id: None,
            expected_config_revision: None,
            now_ns: 20,
        };
        let eval = evaluate_schema_plan(&input);
        assert!(!eval.activation_permitted);
        assert!(
            eval.rejection_reasons
                .iter()
                .any(|r| r == "snapshot_not_fresh")
        );
    }

    #[test]
    fn config_path_is_excluded() {
        let mut cat = catalogue();
        cat.paths[0].path = "/interfaces/interface[name]/config/name".to_string();
        cat.paths[0].config = true;
        let mut snap = snapshot();
        snap.result.paths[0].path = "/interfaces/interface[name]/config/name".to_string();
        snap.result.paths[0].config = true;
        let tgt = target();
        let sel = vec![SelectedSubscriptionPath {
            path: "/interfaces/interface[name=ethernet-1/1]/config/name".to_string(),
            origin: "oc-if".to_string(),
            provenance: String::new(),
            mode: "ON_CHANGE".to_string(),
            sample_interval_ns: 0,
            rationale: String::new(),
            optional: false,
        }];
        let input = SchemaPlanInput {
            catalogue: &cat,
            snapshot: Some(&snap),
            freshness: Some(SnapshotFreshness::Fresh),
            freshness_reasons: &[],
            target: &tgt,
            selected_paths: &sel,
            datastore: None,
            collector_id: None,
            expected_config_revision: None,
            now_ns: 20,
        };
        let eval = evaluate_schema_plan(&input);
        assert!(!eval.activation_permitted);
        assert_eq!(
            eval.decisions[0].kind,
            PathDecisionKind::ConfigurationDataExcluded
        );
    }

    fn plan_input<'a>(
        cat: &'a CompiledCatalogue,
        snap: &'a ApplicabilitySnapshot,
        tgt: &'a TargetConfig,
        sel: &'a [SelectedSubscriptionPath],
    ) -> SchemaPlanInput<'a> {
        SchemaPlanInput {
            catalogue: cat,
            snapshot: Some(snap),
            freshness: Some(SnapshotFreshness::Fresh),
            freshness_reasons: &[],
            target: tgt,
            selected_paths: sel,
            datastore: Some("ietf-datastores:operational"),
            collector_id: Some("collector-a"),
            expected_config_revision: None,
            now_ns: 20,
        }
    }

    fn iface_selected(path: &str, mode: &str, interval: u64) -> SelectedSubscriptionPath {
        SelectedSubscriptionPath {
            path: path.to_string(),
            origin: "oc-if".to_string(),
            provenance: String::new(),
            mode: mode.to_string(),
            sample_interval_ns: interval,
            rationale: "test".to_string(),
            optional: false,
        }
    }

    #[test]
    fn two_distinct_list_instances_are_eligible() {
        // M6D-A1: distinct list-key VALUES are distinct concrete paths —
        // the same schema template must not collapse them into a
        // duplicate.
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        let sel = vec![
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
            iface_selected(
                "/interfaces/interface[name=Ethernet2]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
        ];
        let eval = evaluate_schema_plan(&plan_input(&cat, &snap, &tgt, &sel));
        assert!(eval.activation_permitted, "{:?}", eval.rejection_reasons);
        assert_eq!(eval.proposed_subscriptions.len(), 2);
        assert!(
            eval.decisions
                .iter()
                .all(|d| d.kind == PathDecisionKind::Eligible)
        );
        // Key ordering inside one segment must not create false identity.
        let sel_reordered = vec![
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
            iface_selected(
                "/interfaces/interface[name=Ethernet2]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
        ];
        let eval2 = evaluate_schema_plan(&plan_input(&cat, &snap, &tgt, &sel_reordered));
        assert_eq!(eval.plan_id, eval2.plan_id);
    }

    #[test]
    fn duplicate_concrete_path_rejected() {
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        // Exact duplicate (same path, mode, interval) → duplicate_path.
        let exact = vec![
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
        ];
        let eval = evaluate_schema_plan(&plan_input(&cat, &snap, &tgt, &exact));
        assert!(!eval.activation_permitted);
        assert!(
            eval.decisions
                .iter()
                .any(|d| d.kind == PathDecisionKind::DuplicateConflict
                    && d.reasons.iter().any(|r| r == "duplicate_path"))
        );
        // Same concrete path, different interval → conflicting duplicate.
        let conflict = vec![
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                60_000_000_000,
            ),
        ];
        let eval = evaluate_schema_plan(&plan_input(&cat, &snap, &tgt, &conflict));
        assert!(!eval.activation_permitted);
        assert!(
            eval.decisions
                .iter()
                .any(|d| d.kind == PathDecisionKind::DuplicateConflict
                    && d.reasons.iter().any(|r| r == "conflicting_duplicate_path"))
        );
        // Same concrete path, different mode → conflicting duplicate.
        let mode_conflict = vec![
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "SAMPLE",
                30_000_000_000,
            ),
            iface_selected(
                "/interfaces/interface[name=Ethernet1]/state/counters/in-octets",
                "ON_CHANGE",
                0,
            ),
        ];
        let eval = evaluate_schema_plan(&plan_input(&cat, &snap, &tgt, &mode_conflict));
        assert!(!eval.activation_permitted);
        assert!(
            eval.decisions
                .iter()
                .any(|d| d.kind == PathDecisionKind::DuplicateConflict)
        );
    }

    #[test]
    fn stale_config_revision_rejects_evaluation() {
        // A2: an evaluation pinned to a stale configuration revision is
        // never activation-permitted.
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        let sel = selected();
        let mut input = plan_input(&cat, &snap, &tgt, &sel);
        input.expected_config_revision = Some(tgt.updated_at_ns + 1);
        let eval = evaluate_schema_plan(&input);
        assert!(!eval.activation_permitted);
        assert!(
            eval.rejection_reasons
                .iter()
                .any(|r| r == "config_revision_mismatch")
        );
        // The exposed config_revision is the CURRENT device revision.
        assert_eq!(eval.config_revision, tgt.updated_at_ns);
    }

    #[test]
    fn keyless_list_segment_is_rejected() {
        let cat = catalogue();
        let snap = snapshot();
        let tgt = target();
        let sel = vec![SelectedSubscriptionPath {
            path: "/interfaces/interface/state/counters/in-octets".to_string(),
            origin: "oc-if".to_string(),
            provenance: String::new(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 30_000_000_000,
            rationale: "test".to_string(),
            optional: false,
        }];
        let input = SchemaPlanInput {
            catalogue: &cat,
            snapshot: Some(&snap),
            freshness: Some(SnapshotFreshness::Fresh),
            freshness_reasons: &[],
            target: &tgt,
            selected_paths: &sel,
            datastore: Some("ietf-datastores:operational"),
            collector_id: Some("collector-a"),
            expected_config_revision: None,
            now_ns: 20,
        };
        let eval = evaluate_schema_plan(&input);
        assert!(!eval.activation_permitted);
        assert!(
            eval.rejection_reasons
                .iter()
                .any(|r| r == "path[0]:no_exact_schema_path_result")
        );
    }
}

//! Deterministic device-effective YANG applicability resolver (M6C-1).
//!
//! Answers: "for THIS device, which schema paths in the verified compiled
//! catalogue can Bonsai justify as applicable, based on WHICH exact device
//! and catalogue evidence — and which conclusions remain uncertain?"
//!
//! Hard rules encoded here:
//! - Only integrity-verified `complete` catalogues establish positive
//!   applicability. `partial`/`failed`/`unknown`/missing catalogues cap
//!   every result below authoritative.
//! - A gNMI `ModelData.version` is NEVER a YANG module revision. Revision
//!   identity comes only from RFC 8525 YANG Library evidence (or stays
//!   unresolved).
//! - Capabilities advertisement alone yields `candidate`, never
//!   `applicable`: it proves the device claims a model, not that the
//!   compiled schema matches the device's effective module set.
//! - Unknown feature state is never treated as enabled; unresolvable
//!   feature expressions never create positive applicability.
//! - `deviate not-supported` is already pruned by the compiler; device-
//!   advertised deviations whose semantics M6B cannot evaluate cap paths
//!   at `unknown` — never silently valid.
//! - Import-only modules supply referenced definitions but are not
//!   implemented data trees.
//! - Schema list-key placeholders (`[name]`) stay templates; this module
//!   never invents instance keys.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::compiler::{CompiledCatalogue, CompiledModule, FeatureTruth};
use super::{SchemaPathEntry, eval_feature_expr_tristate};

pub const APPLICABILITY_RESULT_VERSION: u32 = 1;
/// Bound on path-level results stored/emitted per snapshot. Larger
/// catalogues are summarized with `paths_truncated = true` and an
/// explicit total — never silently dropped.
pub const MAX_PATH_RESULTS: usize = 20_000;
/// Bound on module-level results (catalogues are far smaller; this is a
/// defensive cap against pathological inputs).
pub const MAX_MODULE_RESULTS: usize = 8_192;
/// Bound on enabled-feature names recorded per module result.
pub const MAX_FEATURES_RECORDED: usize = 512;

// ---------------------------------------------------------------------------
// Status and reason model
// ---------------------------------------------------------------------------

/// Limited, documented applicability status model. The distinction that
/// matters: a path must not receive a definitive positive or negative
/// classification without sufficient evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicabilityStatus {
    /// Positive applicability established by sufficient evidence.
    Applicable,
    /// Plausible but not proven — weaker evidence tier.
    Candidate,
    /// Definitive negative evidence for the evaluated scope.
    NotApplicable,
    /// Evidence insufficient or ambiguous — conclusion cannot be drawn.
    Unknown,
    /// Previously positive evidence whose freshness/integrity lapsed.
    Stale,
    /// Contradictory evidence — no safe resolution.
    Conflicted,
}

impl ApplicabilityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applicable => "applicable",
            Self::Candidate => "candidate",
            Self::NotApplicable => "not_applicable",
            Self::Unknown => "unknown",
            Self::Stale => "stale",
            Self::Conflicted => "conflicted",
        }
    }
}

/// Machine-readable reason codes. Serialized as snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    CatalogueUnavailable,
    CataloguePartial,
    CatalogueFailed,
    CatalogueUnknown,
    CatalogueIntegrityFailure,
    ModuleMissingFromEffectiveSet,
    ModuleRevisionUnresolved,
    ModuleOrganizationConflict,
    ModuleOrganizationMismatch,
    ModuleNamespaceConflict,
    CapabilitiesOnlyEvidence,
    DeviceLibraryUnavailable,
    YangLibraryIncomplete,
    YangLibraryMalformed,
    YangLibraryUnsupported,
    YangLibraryTimeout,
    YangLibraryTransportFailure,
    YangLibraryPermissionDenied,
    YangLibraryUnsupportedEncoding,
    YangLibraryConflictingModules,
    DatastoreSchemaMissing,
    DatastoreSchemaAmbiguous,
    DatastoreSchemaUnverified,
    DatastoreMismatch,
    FeatureDisabled,
    FeatureStateUnknown,
    FeatureExpressionUnresolved,
    DeviationExcludesPath,
    DeviationSemanticsUnsupported,
    ImportOnlyModule,
    SchemaVersionChanged,
    EvidenceExpired,
    IdentityAmbiguity,
    CollectorDiscoveryUnavailable,
    EvidenceTruncated,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        // Explicit table so diagnostics are stable regardless of serde.
        match self {
            Self::CatalogueUnavailable => "catalogue_unavailable",
            Self::CataloguePartial => "catalogue_partial",
            Self::CatalogueFailed => "catalogue_failed",
            Self::CatalogueUnknown => "catalogue_unknown",
            Self::CatalogueIntegrityFailure => "catalogue_integrity_failure",
            Self::ModuleMissingFromEffectiveSet => "module_missing_from_effective_set",
            Self::ModuleRevisionUnresolved => "module_revision_unresolved",
            Self::ModuleOrganizationConflict => "module_organization_conflict",
            Self::ModuleOrganizationMismatch => "module_organization_mismatch",
            Self::ModuleNamespaceConflict => "module_namespace_conflict",
            Self::CapabilitiesOnlyEvidence => "capabilities_only_evidence",
            Self::DeviceLibraryUnavailable => "device_library_unavailable",
            Self::YangLibraryIncomplete => "yang_library_incomplete",
            Self::YangLibraryMalformed => "yang_library_malformed",
            Self::YangLibraryUnsupported => "yang_library_unsupported",
            Self::YangLibraryTimeout => "yang_library_timeout",
            Self::YangLibraryTransportFailure => "yang_library_transport_failure",
            Self::YangLibraryPermissionDenied => "yang_library_permission_denied",
            Self::YangLibraryUnsupportedEncoding => "yang_library_unsupported_encoding",
            Self::YangLibraryConflictingModules => "yang_library_conflicting_modules",
            Self::DatastoreSchemaMissing => "datastore_schema_missing",
            Self::DatastoreSchemaAmbiguous => "datastore_schema_ambiguous",
            Self::DatastoreSchemaUnverified => "datastore_schema_unverified",
            Self::DatastoreMismatch => "datastore_mismatch",
            Self::FeatureDisabled => "feature_disabled",
            Self::FeatureStateUnknown => "feature_state_unknown",
            Self::FeatureExpressionUnresolved => "feature_expression_unresolved",
            Self::DeviationExcludesPath => "deviation_excludes_path",
            Self::DeviationSemanticsUnsupported => "deviation_semantics_unsupported",
            Self::ImportOnlyModule => "import_only_module",
            Self::SchemaVersionChanged => "schema_version_changed",
            Self::EvidenceExpired => "evidence_expired",
            Self::IdentityAmbiguity => "identity_ambiguity",
            Self::CollectorDiscoveryUnavailable => "collector_discovery_unavailable",
            Self::EvidenceTruncated => "evidence_truncated",
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence model (transport-neutral; populated by the probe layer)
// ---------------------------------------------------------------------------

/// One advertised gNMI `ModelData` entry. `version` is the gNMI model
/// version — provenance only, NEVER a YANG module revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvertisedModel {
    pub name: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub version: String,
}

/// gNMI Capabilities evidence for one observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEvidence {
    pub observed_at_ns: i64,
    #[serde(default)]
    pub gnmi_version: String,
    #[serde(default)]
    pub encodings: Vec<String>,
    /// Effective transport security label (e.g. "tls_verify", "mtls",
    /// "plaintext_explicit") — never credentials or auth metadata.
    #[serde(default)]
    pub security_mode: String,
    /// Sorted, deduplicated advertised models.
    #[serde(default)]
    pub models: Vec<AdvertisedModel>,
    /// Assigned collector identity at observation time, when known.
    #[serde(default)]
    pub collector_id: Option<String>,
}

/// Which RFC 8525 tree shape the evidence came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YangLibraryTree {
    /// Current NMDA `/yang-library` container (module-sets + schemas +
    /// datastore associations).
    Nmda,
    /// Deprecated RFC 7895 `/modules-state` container — implemented vs
    /// import-only via `conformance-type`, but NO datastore→schema link.
    Legacy,
}

/// One module entry inside a YANG Library module-set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YangLibraryModule {
    pub name: String,
    /// YANG module revision date when the device reports one. `None`
    /// means the module declares no revision (or none was returned).
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    /// Feature names the device reports as supported/enabled (local
    /// YANG identifiers, unqualified).
    #[serde(default)]
    pub features: BTreeSet<String>,
    /// Names of deviation modules the device reports for this module.
    #[serde(default)]
    pub deviations: Vec<String>,
    /// Submodule names belonging to this module (RFC 8525 `submodule`
    /// list). Preserved for provenance; submodules are not separately
    /// applicability-resolved in M6C-1.
    #[serde(default)]
    pub submodules: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YangLibraryModuleSet {
    pub name: String,
    #[serde(default)]
    pub modules: Vec<YangLibraryModule>,
    #[serde(default)]
    pub import_only_modules: Vec<YangLibraryModule>,
}

/// NMDA schema: a named bundle of module-set references.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YangLibrarySchema {
    pub name: String,
    #[serde(default)]
    pub module_sets: Vec<String>,
}

/// NMDA datastore → schema association.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YangLibraryDatastore {
    /// Datastore identity as reported (e.g. "ietf-datastores:running").
    pub name: String,
    /// Referenced schema name.
    pub schema: String,
}

/// Parsed RFC 8525 (or legacy RFC 7895) YANG Library evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct YangLibraryEvidence {
    pub tree: YangLibraryTree,
    /// `content-id` (NMDA) or `module-set-id` (legacy); `None` when the
    /// device omitted it — weaker freshness binding, still usable.
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(default)]
    pub module_sets: Vec<YangLibraryModuleSet>,
    #[serde(default)]
    pub schemas: Vec<YangLibrarySchema>,
    #[serde(default)]
    pub datastores: Vec<YangLibraryDatastore>,
    /// gNMI origin/path the evidence was retrieved under.
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub path: String,
    pub fetched_at_ns: i64,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

/// Classification of the YANG Library probe. None of these may be
/// silently promoted to a complete module set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum YangLibraryOutcome {
    /// Full RFC 8525 NMDA library parsed and internally consistent.
    Complete {
        evidence: YangLibraryEvidence,
    },
    /// Only the deprecated RFC 7895 `/modules-state` tree was available.
    /// Module implementation is known; datastore→schema association is
    /// NOT established by this tree.
    Legacy {
        evidence: YangLibraryEvidence,
    },
    /// Device answered but exposed no YANG Library data — Capabilities
    /// remain the only schema evidence.
    Unsupported,
    PermissionDenied,
    Incomplete {
        reason: String,
    },
    Malformed {
        reason: String,
    },
    Timeout,
    TransportFailure {
        reason: String,
    },
    ConflictingModuleDefinitions {
        reason: String,
    },
    UnsupportedEncoding,
    DatastoreSchemaMissing {
        reason: String,
    },
    DatastoreSchemaAmbiguous {
        reason: String,
    },
}

impl YangLibraryOutcome {
    /// Evidence usable for module-level resolution, when present.
    pub fn evidence(&self) -> Option<&YangLibraryEvidence> {
        match self {
            Self::Complete { evidence } | Self::Legacy { evidence } => Some(evidence),
            _ => None,
        }
    }

    /// Reason code summarizing this outcome for result propagation.
    pub fn reason_code(&self) -> ReasonCode {
        match self {
            Self::Complete { .. } => ReasonCode::YangLibraryUnsupported, // unused
            Self::Legacy { .. } => ReasonCode::DatastoreSchemaUnverified,
            Self::Unsupported => ReasonCode::YangLibraryUnsupported,
            Self::PermissionDenied => ReasonCode::YangLibraryPermissionDenied,
            Self::Incomplete { .. } => ReasonCode::YangLibraryIncomplete,
            Self::Malformed { .. } => ReasonCode::YangLibraryMalformed,
            Self::Timeout => ReasonCode::YangLibraryTimeout,
            Self::TransportFailure { .. } => ReasonCode::YangLibraryTransportFailure,
            Self::ConflictingModuleDefinitions { .. } => ReasonCode::YangLibraryConflictingModules,
            Self::UnsupportedEncoding => ReasonCode::YangLibraryUnsupportedEncoding,
            Self::DatastoreSchemaMissing { .. } => ReasonCode::DatastoreSchemaMissing,
            Self::DatastoreSchemaAmbiguous { .. } => ReasonCode::DatastoreSchemaAmbiguous,
        }
    }
}

/// Per-module merged device evidence (capabilities + YANG Library).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceModuleEvidence {
    pub name: String,
    /// gNMI-advertised organization (provenance; matched against the
    /// catalogue's organization statement — never assumed equal).
    #[serde(default)]
    pub organization: Option<String>,
    /// gNMI `ModelData.version` — model version provenance ONLY. This is
    /// not a YANG revision and is never compared as one.
    #[serde(default)]
    pub catalog_version: Option<String>,
    /// Independently reported YANG module revision (YANG Library only).
    #[serde(default)]
    pub revision: Option<String>,
    /// Independently reported YANG namespace (YANG Library only).
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub source: ModuleEvidenceSource,
    /// True when the device reports this module as import-only — it
    /// supplies referenced definitions but implements no data tree.
    #[serde(default)]
    pub import_only: bool,
    /// Feature local-names the device reports as supported/enabled.
    #[serde(default)]
    pub enabled_features: BTreeSet<String>,
    /// True when feature state is authoritative (complete YANG Library
    /// module entry). False → every feature atom evaluates Unknown.
    #[serde(default)]
    pub features_state_known: bool,
    /// Deviation module names the device reports for this module.
    #[serde(default)]
    pub deviations: Vec<String>,
    /// True when contradictory identity evidence (e.g. same module name
    /// with different revisions) was merged — preserved, never resolved
    /// silently.
    #[serde(default)]
    pub identity_conflict: bool,
    /// Platform/NOS tag match against catalogue `platforms` — supporting
    /// metadata only, never positive applicability by itself.
    #[serde(default)]
    pub platform_match: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleEvidenceSource {
    #[default]
    Capabilities,
    YangLibrary,
    Both,
}

/// Trusted device/collector identity context for one evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceBinding {
    /// Registry address key (may include port).
    pub address: String,
    /// Canonical bare-IP device key used for graph identity.
    #[serde(default)]
    pub canonical_address: String,
    /// Stable external identity (e.g. netbox id) when the device was
    /// imported; `None` for locally registered devices.
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub collector_id: Option<String>,
    /// Set when identity evidence is ambiguous — preserved, never merged.
    #[serde(default)]
    pub identity_ambiguous: bool,
}

impl DeviceBinding {
    /// Stable identity string persisted with the snapshot: external id
    /// when present (strongest), else canonical address.
    pub fn identity_key(&self) -> String {
        self.external_id
            .clone()
            .unwrap_or_else(|| self.canonical_address.clone())
    }
}

/// Everything the resolver needs. All fields are evidence — the resolver
/// performs no I/O and is deterministic for identical inputs.
pub struct ApplicabilityInput<'a> {
    /// Verified active compiled catalogue. `None` = no usable catalogue
    /// (missing, corrupt, integrity failure) — explicit, never assumed.
    pub catalogue: Option<&'a CompiledCatalogue>,
    /// Diagnostics for catalogue load failures (integrity etc.).
    pub catalogue_diagnostics: Vec<String>,
    pub capabilities: Option<&'a CapabilityEvidence>,
    pub yang_library: Option<&'a YangLibraryOutcome>,
    /// Datastore identity selected for this evaluation (e.g.
    /// "ietf-datastores:running"). `None` → resolver picks the preferred
    /// datastore and records the choice; ambiguity is explicit.
    pub requested_datastore: Option<&'a str>,
    /// Explicit platform/NOS metadata (inventory label). Supporting
    /// evidence only.
    pub platform: Option<&'a str>,
    pub device: &'a DeviceBinding,
    pub now_ns: i64,
}

// ---------------------------------------------------------------------------
// Result model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleClassification {
    Implemented,
    ImportOnly,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModuleResult {
    pub name: String,
    /// Revision of the compiled catalogue definition used.
    #[serde(default)]
    pub catalogue_revision: String,
    /// Device-advertised YANG revision when independently known.
    #[serde(default)]
    pub device_revision: Option<String>,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub namespace: String,
    pub classification: ModuleClassification,
    /// Feature names proven enabled on the device (bounded).
    #[serde(default)]
    pub enabled_features: Vec<String>,
    /// Whether feature state is authoritative for this module.
    #[serde(default)]
    pub features_state_known: bool,
    #[serde(default)]
    pub deviations: Vec<String>,
    pub status: ApplicabilityStatus,
    #[serde(default)]
    pub reasons: Vec<ReasonCode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PathResult {
    /// Canonical schema path — a TEMPLATE with `[key]` placeholders,
    /// never an instance path.
    pub path: String,
    pub module: String,
    pub module_revision: String,
    pub namespace: String,
    /// container|list|leaf|leaf-list|anyxml|anydata
    pub kind: String,
    /// Effective config flag: true = configuration data, false = state.
    pub config: bool,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub resolved_type: Option<String>,
    #[serde(default)]
    pub units: Option<String>,
    #[serde(default)]
    pub required_features: Vec<String>,
    pub status: ApplicabilityStatus,
    #[serde(default)]
    pub reasons: Vec<ReasonCode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplicabilityResult {
    pub result_version: u32,
    pub schema_id: Option<String>,
    /// Device module-set `content-id`/`module-set-id` when known.
    #[serde(default)]
    pub content_id: Option<String>,
    /// Datastore whose schema was evaluated.
    #[serde(default)]
    pub datastore: Option<String>,
    /// Overall resolution status for this evaluation.
    pub overall_status: ApplicabilityStatus,
    #[serde(default)]
    pub reasons: Vec<ReasonCode>,
    pub modules: Vec<ModuleResult>,
    pub paths: Vec<PathResult>,
    /// Total path results before truncation.
    pub paths_total: usize,
    pub paths_truncated: bool,
    #[serde(default)]
    pub diagnostics: Vec<String>,
    /// sha256 fingerprint over the canonical evidence inputs — stable
    /// for identical catalogue + device evidence.
    pub evidence_fingerprint: String,
    pub evaluated_at_ns: i64,
    /// When the device evidence was observed (capabilities/library).
    #[serde(default)]
    pub evidence_observed_at_ns: Option<i64>,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Feature state per module name for atom resolution.
struct ModuleFeatureState {
    /// Declaring module's YANG prefix (atoms are canonicalized to
    /// `declaring_prefix:local`).
    prefix: String,
    /// Local feature names proven enabled.
    enabled: BTreeSet<String>,
    /// Local feature names declared by the compiled module.
    declared: BTreeSet<String>,
    /// Whether enabled/disabled is authoritative. When false every atom
    /// resolves Unknown.
    known: bool,
}

/// The device module evidence applicable to the selected datastore,
/// after merging capabilities and YANG Library sources.
struct EffectiveSet {
    /// module name -> merged evidence
    modules: BTreeMap<String, DeviceModuleEvidence>,
    /// Names of module-sets combined into the selected schema.
    module_set_names: Vec<String>,
    /// True when module presence/identity is device-authoritative
    /// (complete NMDA library or legacy modules-state).
    authoritative: bool,
    /// True when the datastore→schema→module-set link is established
    /// (NMDA only). Without it no path may be `applicable`.
    datastore_linked: bool,
    /// Selected datastore identity, when resolved.
    datastore: Option<String>,
    content_id: Option<String>,
    /// Set when datastore/schema resolution failed — every module is
    /// then `unknown`, never `not_applicable` (absence of evidence is
    /// not evidence of absence).
    blocked: Option<ReasonCode>,
}

pub fn resolve_applicability(input: &ApplicabilityInput<'_>) -> ApplicabilityResult {
    let mut diagnostics = input.catalogue_diagnostics.clone();
    let mut reasons: BTreeSet<ReasonCode> = BTreeSet::new();

    // ── A. Catalogue integrity ────────────────────────────────────────
    let Some(catalogue) = input.catalogue else {
        reasons.insert(ReasonCode::CatalogueUnavailable);
        reasons.insert(ReasonCode::CatalogueIntegrityFailure);
        return ApplicabilityResult {
            result_version: APPLICABILITY_RESULT_VERSION,
            schema_id: None,
            content_id: None,
            datastore: input.requested_datastore.map(str::to_string),
            overall_status: ApplicabilityStatus::Unknown,
            reasons: reasons.into_iter().collect(),
            modules: Vec::new(),
            paths: Vec::new(),
            paths_total: 0,
            paths_truncated: false,
            diagnostics,
            evidence_fingerprint: fingerprint(input, None, None),
            evaluated_at_ns: input.now_ns,
            evidence_observed_at_ns: evidence_observed_at(input),
        };
    };

    let catalogue_reason = match catalogue.status.as_str() {
        "complete" => None,
        "partial" => Some(ReasonCode::CataloguePartial),
        "failed" => Some(ReasonCode::CatalogueFailed),
        _ => Some(ReasonCode::CatalogueUnknown),
    };
    if let Some(rc) = catalogue_reason {
        reasons.insert(rc);
    }
    let catalogue_authoritative = catalogue.status == "complete";

    // ── B. Device evidence merge ──────────────────────────────────────
    let effective = build_effective_set(input, &mut diagnostics);
    if let Some(blocked) = effective.blocked {
        reasons.insert(blocked);
    }
    if let Some(outcome) = input.yang_library {
        let rc = outcome.reason_code();
        // Complete carries no reason; everything else is recorded.
        if !matches!(outcome, YangLibraryOutcome::Complete { .. }) {
            reasons.insert(rc);
        }
    }
    if input.capabilities.is_none() {
        reasons.insert(ReasonCode::DeviceLibraryUnavailable);
    }

    // ── C/D. Module + path evaluation ─────────────────────────────────
    let feature_states = build_feature_states(catalogue, &effective.modules);
    let (ds_config, ds_state) =
        datastore_capabilities(effective.datastore.as_deref(), &mut diagnostics);

    let mut module_results: Vec<ModuleResult> = Vec::new();
    let mut path_results: Vec<PathResult> = Vec::new();
    // Total schema paths in scope — counted independently of result
    // truncation so `paths_truncated` stays truthful.
    let paths_total = catalogue.paths.len();
    let mut truncated = false;

    for cm in &catalogue.modules {
        if module_results.len() >= MAX_MODULE_RESULTS {
            reasons.insert(ReasonCode::EvidenceTruncated);
            truncated = true;
            break;
        }
        let ev = effective.modules.get(&cm.name);
        let platform_match = platform_match(cm, input.platform);
        let (module_status, module_reasons, classification) = evaluate_module(
            cm,
            ev,
            &effective,
            input,
            catalogue_authoritative,
            catalogue_reason,
            platform_match,
        );

        module_results.push(ModuleResult {
            name: cm.name.clone(),
            catalogue_revision: cm.revision.clone(),
            device_revision: ev.and_then(|e| e.revision.clone()),
            organization: cm.organization.clone(),
            namespace: cm.namespace.clone(),
            classification,
            enabled_features: ev
                .map(|e| {
                    e.enabled_features
                        .iter()
                        .take(MAX_FEATURES_RECORDED)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            features_state_known: ev.map(|e| e.features_state_known).unwrap_or(false),
            deviations: ev.map(|e| e.deviations.clone()).unwrap_or_default(),
            status: module_status,
            reasons: module_reasons.iter().map(|r| *r).collect(),
        });

        for entry in catalogue.paths.iter().filter(|p| p.module == cm.name) {
            if path_results.len() < MAX_PATH_RESULTS {
                path_results.push(evaluate_path(
                    entry,
                    module_status,
                    &module_reasons,
                    &feature_states,
                    &effective,
                    ds_config,
                    ds_state,
                ));
            } else {
                truncated = true;
            }
        }
    }
    let truncated = truncated || path_results.len() < paths_total;

    path_results.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    module_results.sort_by(|a, b| a.name.cmp(&b.name));

    // ── Overall status ────────────────────────────────────────────────
    let overall = overall_status(
        catalogue_authoritative,
        &effective,
        input.yang_library,
        &module_results,
        input,
        &mut reasons,
        &mut diagnostics,
    );

    let mut all_reasons: Vec<ReasonCode> = reasons.into_iter().collect();
    for m in &module_results {
        for r in &m.reasons {
            if !all_reasons.contains(r) {
                all_reasons.push(*r);
            }
        }
    }
    for p in path_results.iter().take(512) {
        for r in &p.reasons {
            if !all_reasons.contains(r) {
                all_reasons.push(*r);
            }
        }
    }
    all_reasons.sort();
    all_reasons.dedup();

    ApplicabilityResult {
        result_version: APPLICABILITY_RESULT_VERSION,
        schema_id: Some(catalogue.schema_id.clone()),
        content_id: effective.content_id.clone(),
        datastore: effective.datastore.clone(),
        overall_status: overall,
        reasons: all_reasons,
        modules: module_results,
        paths: path_results,
        paths_total,
        paths_truncated: truncated || paths_total > MAX_PATH_RESULTS,
        diagnostics,
        evidence_fingerprint: fingerprint(input, Some(catalogue), Some(&effective)),
        evaluated_at_ns: input.now_ns,
        evidence_observed_at_ns: evidence_observed_at(input),
    }
}

fn evidence_observed_at(input: &ApplicabilityInput<'_>) -> Option<i64> {
    input.capabilities.map(|c| c.observed_at_ns).or_else(|| {
        input
            .yang_library
            .as_ref()
            .and_then(|o| o.evidence())
            .map(|e| e.fetched_at_ns)
    })
}

// ── effective set construction ─────────────────────────────────────────

fn local_name(identity: &str) -> &str {
    identity.rsplit(':').next().unwrap_or(identity)
}

fn build_effective_set(
    input: &ApplicabilityInput<'_>,
    diagnostics: &mut Vec<String>,
) -> EffectiveSet {
    let mut set = EffectiveSet {
        modules: BTreeMap::new(),
        module_set_names: Vec::new(),
        authoritative: false,
        datastore_linked: false,
        datastore: None,
        content_id: None,
        blocked: None,
    };

    // Seed from gNMI Capabilities — candidate-level evidence only.
    if let Some(caps) = input.capabilities {
        for m in &caps.models {
            let vacant = !set.modules.contains_key(&m.name);
            let entry = set
                .modules
                .entry(m.name.clone())
                .or_insert_with(|| DeviceModuleEvidence {
                    name: m.name.clone(),
                    organization: None,
                    catalog_version: None,
                    revision: None,
                    namespace: None,
                    source: ModuleEvidenceSource::Capabilities,
                    import_only: false,
                    enabled_features: BTreeSet::new(),
                    features_state_known: false,
                    deviations: Vec::new(),
                    identity_conflict: false,
                    platform_match: None,
                });
            // Duplicate advertisements: same name with different
            // organizations is ambiguous identity evidence — preserved
            // as a conflict, never merged silently.
            if let Some(prev) = &entry.organization
                && !prev.is_empty()
                && !m.organization.is_empty()
                && prev != &m.organization
            {
                entry.identity_conflict = true;
                diagnostics.push(format!(
                    "capabilities advertise conflicting organizations for model '{}'",
                    m.name
                ));
            }
            if entry.organization.is_none() && !m.organization.is_empty() {
                entry.organization = Some(m.organization.clone());
            }
            if entry.catalog_version.is_none() && !m.version.is_empty() {
                entry.catalog_version = Some(m.version.clone());
            }
            if vacant {
                entry.source = ModuleEvidenceSource::Capabilities;
            }
        }
    }

    // Merge YANG Library evidence for the selected datastore.
    if let Some(outcome) = input.yang_library
        && let Some(evidence) = outcome.evidence()
    {
        set.content_id = evidence.content_id.clone();
        match evidence.tree {
            YangLibraryTree::Nmda => {
                match resolve_nmda_datastore(evidence, input.requested_datastore, diagnostics) {
                    Ok((datastore_name, schema)) => {
                        set.datastore = Some(datastore_name);
                        set.module_set_names = schema.module_sets.clone();
                        set.datastore_linked = true;
                        set.authoritative = true;
                        merge_module_sets(&mut set, evidence, &schema.module_sets, diagnostics);
                    }
                    Err(rc) => {
                        set.blocked = Some(rc);
                        diagnostics.push(format!(
                            "datastore/schema resolution failed: {}",
                            rc.as_str()
                        ));
                    }
                }
            }
            YangLibraryTree::Legacy => {
                // Deprecated modules-state: module implementation is
                // device-reported, but there is NO datastore→schema link.
                set.authoritative = true;
                set.datastore_linked = false;
                let names: Vec<String> = evidence
                    .module_sets
                    .iter()
                    .map(|s| s.name.clone())
                    .collect();
                set.module_set_names = names.clone();
                merge_module_sets(&mut set, evidence, &names, diagnostics);
            }
        }
    }

    set
}

/// Resolve datastore → schema → module-set names for NMDA evidence.
fn resolve_nmda_datastore(
    evidence: &YangLibraryEvidence,
    requested: Option<&str>,
    diagnostics: &mut Vec<String>,
) -> Result<(String, YangLibrarySchema), ReasonCode> {
    if evidence.datastores.is_empty() {
        return Err(ReasonCode::DatastoreSchemaMissing);
    }
    let datastore = if let Some(want) = requested {
        let want = want.trim();
        evidence
            .datastores
            .iter()
            .find(|d| d.name == want || local_name(&d.name) == want)
            .ok_or(ReasonCode::DatastoreSchemaMissing)?
            .clone()
    } else {
        select_preferred_datastore(evidence, diagnostics)?
    };
    let schema = evidence
        .schemas
        .iter()
        .find(|s| s.name == datastore.schema)
        .cloned()
        .ok_or(ReasonCode::DatastoreSchemaMissing)?;
    if schema.module_sets.is_empty() {
        return Err(ReasonCode::DatastoreSchemaAmbiguous);
    }
    // Every referenced module-set must resolve — a dangling reference
    // means the effective set cannot be established completely.
    for ms in &schema.module_sets {
        if !evidence.module_sets.iter().any(|m| &m.name == ms) {
            return Err(ReasonCode::DatastoreSchemaMissing);
        }
    }
    Ok((datastore.name, schema))
}

/// Pick the datastore whose schema is being evaluated. Preference:
/// running (gNMI Get/Subscribe ALL semantics) > operational > any single
/// remaining datastore. Multiple unmatched datastores are ambiguous —
/// never silently combined.
fn select_preferred_datastore(
    evidence: &YangLibraryEvidence,
    diagnostics: &mut Vec<String>,
) -> Result<YangLibraryDatastore, ReasonCode> {
    if evidence.datastores.len() == 1 {
        return Ok(evidence.datastores[0].clone());
    }
    for preferred in ["running", "operational", "candidate", "startup", "intended"] {
        let matches: Vec<&YangLibraryDatastore> = evidence
            .datastores
            .iter()
            .filter(|d| local_name(&d.name) == preferred)
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
        if matches.len() > 1 {
            diagnostics.push(format!(
                "multiple datastores named '{}' — ambiguous",
                preferred
            ));
            return Err(ReasonCode::DatastoreSchemaAmbiguous);
        }
    }
    diagnostics.push(format!(
        "no preferred datastore among {:?}",
        evidence
            .datastores
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
    ));
    Err(ReasonCode::DatastoreSchemaAmbiguous)
}

/// Merge module entries from the referenced module-sets into the
/// effective set. Duplicate module names across sets must agree on
/// revision/namespace/features/deviations — disagreement is preserved
/// as `identity_conflict`, never resolved silently.
fn merge_module_sets(
    set: &mut EffectiveSet,
    evidence: &YangLibraryEvidence,
    module_set_names: &[String],
    diagnostics: &mut Vec<String>,
) {
    for ms_name in module_set_names {
        let Some(ms) = evidence.module_sets.iter().find(|m| &m.name == ms_name) else {
            continue;
        };
        for m in &ms.modules {
            merge_library_module(set, m, false, diagnostics);
        }
        for m in &ms.import_only_modules {
            merge_library_module(set, m, true, diagnostics);
        }
    }
}

fn merge_library_module(
    set: &mut EffectiveSet,
    m: &YangLibraryModule,
    import_only: bool,
    diagnostics: &mut Vec<String>,
) {
    match set.modules.get_mut(&m.name) {
        None => {
            set.modules.insert(
                m.name.clone(),
                DeviceModuleEvidence {
                    name: m.name.clone(),
                    organization: None,
                    catalog_version: None,
                    revision: m.revision.clone().filter(|r| !r.is_empty()),
                    namespace: m.namespace.clone(),
                    source: ModuleEvidenceSource::YangLibrary,
                    import_only,
                    enabled_features: if import_only {
                        BTreeSet::new()
                    } else {
                        m.features.clone()
                    },
                    features_state_known: !import_only,
                    deviations: m.deviations.clone(),
                    identity_conflict: false,
                    platform_match: None,
                },
            );
        }
        Some(existing) => {
            // Merge library identity onto possibly capability-seeded
            // entry. A conflict requires BOTH sides to independently
            // report a value that disagrees — absent fields on the
            // capabilities side (revision, namespace, feature state)
            // are filled in, not treated as contradictions.
            let same_identity = (existing.revision.is_none() || existing.revision == m.revision)
                && (existing.namespace.is_none() || existing.namespace == m.namespace)
                && (!existing.features_state_known
                    || import_only
                    || existing.enabled_features == m.features);
            if !same_identity {
                existing.identity_conflict = true;
                diagnostics.push(format!(
                    "module '{}' has conflicting definitions across evidence sources",
                    m.name
                ));
            }
            if existing.revision.is_none() {
                existing.revision = m.revision.clone().filter(|r| !r.is_empty());
            }
            if existing.namespace.is_none() {
                existing.namespace = m.namespace.clone();
            }
            if !import_only {
                existing.enabled_features = m.features.clone();
                existing.features_state_known = true;
            }
            existing.deviations = m.deviations.clone();
            existing.import_only = existing.import_only || import_only;
            existing.source = ModuleEvidenceSource::Both;
        }
    }
}

// ── module evaluation ──────────────────────────────────────────────────

fn platform_match(cm: &CompiledModule, platform: Option<&str>) -> Option<bool> {
    let p = platform?.trim();
    if p.is_empty() || cm.platforms.is_empty() {
        return None;
    }
    Some(
        cm.platforms
            .iter()
            .any(|t| t == "*" || t.eq_ignore_ascii_case(p)),
    )
}

fn evaluate_module(
    cm: &CompiledModule,
    ev: Option<&DeviceModuleEvidence>,
    effective: &EffectiveSet,
    input: &ApplicabilityInput<'_>,
    catalogue_authoritative: bool,
    catalogue_reason: Option<ReasonCode>,
    platform_match: Option<bool>,
) -> (
    ApplicabilityStatus,
    BTreeSet<ReasonCode>,
    ModuleClassification,
) {
    let mut reasons = BTreeSet::new();
    let _ = platform_match; // recorded via evidence only; never decisive

    let classification = match ev {
        Some(e) if e.import_only => ModuleClassification::ImportOnly,
        Some(e) if matches!(e.source, ModuleEvidenceSource::Capabilities) => {
            ModuleClassification::Unknown
        }
        Some(_) => ModuleClassification::Implemented,
        None => ModuleClassification::Unknown,
    };

    if input.device.identity_ambiguous {
        reasons.insert(ReasonCode::IdentityAmbiguity);
        return (
            ApplicabilityStatus::Conflicted,
            reasons,
            ModuleClassification::Unknown,
        );
    }

    if !catalogue_authoritative {
        reasons.insert(catalogue_reason.unwrap_or(ReasonCode::CataloguePartial));
        return (ApplicabilityStatus::Unknown, reasons, classification);
    }

    let Some(ev) = ev else {
        if effective.authoritative {
            reasons.insert(ReasonCode::ModuleMissingFromEffectiveSet);
            return (ApplicabilityStatus::NotApplicable, reasons, classification);
        }
        reasons.insert(ReasonCode::DeviceLibraryUnavailable);
        if input.capabilities.is_some() {
            reasons.insert(ReasonCode::CapabilitiesOnlyEvidence);
        }
        return (ApplicabilityStatus::Unknown, reasons, classification);
    };

    if ev.import_only {
        reasons.insert(ReasonCode::ImportOnlyModule);
        return (
            ApplicabilityStatus::NotApplicable,
            reasons,
            ModuleClassification::ImportOnly,
        );
    }

    if ev.identity_conflict {
        reasons.insert(ReasonCode::ModuleRevisionUnresolved);
        reasons.insert(ReasonCode::YangLibraryConflictingModules);
        return (ApplicabilityStatus::Conflicted, reasons, classification);
    }

    // Namespace identity: an independently reported namespace that
    // disagrees with the compiled module is a different module.
    if let (Some(ns), true) = (&ev.namespace, !cm.namespace.is_empty())
        && ns != &cm.namespace
    {
        reasons.insert(ReasonCode::ModuleNamespaceConflict);
        return (ApplicabilityStatus::Conflicted, reasons, classification);
    }

    // Organization identity (capabilities-sourced): both present and
    // different is ambiguous when capabilities are the ONLY identity
    // evidence. When RFC 8525 YANG Library membership already proves the
    // module's name+namespace+revision, the capabilities organization is
    // descriptive provenance (gNMI ModelData has no independent revision
    // authority), so the mismatch is recorded as a warning reason rather
    // than a conflict. Namespace/revision/feature/deviation checks below
    // remain authoritative and unchanged.
    if let Some(org) = &ev.organization
        && !org.is_empty()
        && !cm.organization.is_empty()
        && org != &cm.organization
    {
        if matches!(
            ev.source,
            ModuleEvidenceSource::YangLibrary | ModuleEvidenceSource::Both
        ) {
            reasons.insert(ReasonCode::ModuleOrganizationMismatch);
        } else {
            reasons.insert(ReasonCode::ModuleOrganizationConflict);
            return (ApplicabilityStatus::Conflicted, reasons, classification);
        }
    }

    // Revision resolution. Catalogue revision "unknown"/"" means the
    // compiler found no revision statement.
    let cat_rev = normalized_revision(&cm.revision);
    let dev_rev = ev.revision.as_deref().and_then(normalized_revision);
    let revision_state = match (cat_rev, dev_rev) {
        (Some(c), Some(d)) if c == d => RevisionState::Exact,
        (Some(_), Some(_)) => RevisionState::Mismatch,
        (None, None) => RevisionState::Exact, // neither declares a revision
        _ => RevisionState::Unresolved,
    };
    match revision_state {
        RevisionState::Exact => {}
        RevisionState::Mismatch => {
            reasons.insert(ReasonCode::ModuleRevisionUnresolved);
            return (ApplicabilityStatus::Unknown, reasons, classification);
        }
        RevisionState::Unresolved => {
            reasons.insert(ReasonCode::ModuleRevisionUnresolved);
        }
    }

    // Evidence strength → module status.
    let library_backed = matches!(
        ev.source,
        ModuleEvidenceSource::YangLibrary | ModuleEvidenceSource::Both
    );
    let status = if effective.blocked.is_some() {
        reasons.insert(ReasonCode::DeviceLibraryUnavailable);
        ApplicabilityStatus::Unknown
    } else if !effective.datastore_linked {
        // Legacy modules-state or caps-only: module presence may be
        // established but the datastore schema link is not.
        reasons.insert(ReasonCode::DatastoreSchemaUnverified);
        if library_backed {
            ApplicabilityStatus::Candidate
        } else {
            reasons.insert(ReasonCode::CapabilitiesOnlyEvidence);
            ApplicabilityStatus::Candidate
        }
    } else if !effective.authoritative {
        reasons.insert(ReasonCode::CapabilitiesOnlyEvidence);
        ApplicabilityStatus::Candidate
    } else if library_backed && revision_state == RevisionState::Exact {
        ApplicabilityStatus::Applicable
    } else {
        // Authoritative set but revision unresolved → cannot claim
        // the compiled schema is the device's effective schema.
        ApplicabilityStatus::Candidate
    };

    if !ev.deviations.is_empty() {
        reasons.insert(ReasonCode::DeviationSemanticsUnsupported);
    }
    if !ev.features_state_known {
        reasons.insert(ReasonCode::FeatureStateUnknown);
    }

    (status, reasons, classification)
}

#[derive(PartialEq, Eq)]
enum RevisionState {
    Exact,
    Mismatch,
    Unresolved,
}

fn normalized_revision(rev: &str) -> Option<&str> {
    let r = rev.trim();
    if r.is_empty() || r == "unknown" {
        None
    } else {
        Some(r)
    }
}

// ── path evaluation ────────────────────────────────────────────────────

fn evaluate_path(
    entry: &SchemaPathEntry,
    module_status: ApplicabilityStatus,
    module_reasons: &BTreeSet<ReasonCode>,
    feature_states: &BTreeMap<String, ModuleFeatureState>,
    effective: &EffectiveSet,
    ds_config: bool,
    ds_state: bool,
) -> PathResult {
    let mut status = module_status;
    let mut reasons: BTreeSet<ReasonCode> = module_reasons.clone();

    // Datastore applicability: config data needs a config-bearing
    // datastore; state data needs an operational-bearing one.
    if entry.config && !ds_config {
        reasons.insert(ReasonCode::DatastoreMismatch);
        status = not_applicable_if_positive(status);
    } else if !entry.config && !ds_state {
        reasons.insert(ReasonCode::DatastoreMismatch);
        status = not_applicable_if_positive(status);
    }

    // Feature gates: evaluate each required expression with tri-state
    // logic. A proven-false gate removes the subtree; an unproven gate
    // caps positive applicability.
    let mut feature_disabled = false;
    let mut feature_unknown = false;
    for expr in &entry.required_features {
        let truth =
            eval_feature_expr_tristate(expr, &|atom| feature_atom_truth(atom, feature_states));
        match truth {
            FeatureTruth::False => feature_disabled = true,
            FeatureTruth::Unknown => feature_unknown = true,
            FeatureTruth::True => {}
        }
    }
    if feature_disabled {
        reasons.insert(ReasonCode::FeatureDisabled);
        status = ApplicabilityStatus::NotApplicable;
    } else if feature_unknown {
        reasons.insert(ReasonCode::FeatureStateUnknown);
        if status == ApplicabilityStatus::Applicable {
            status = ApplicabilityStatus::Unknown;
        }
    }

    // Device-advertised deviations whose per-node semantics M6B cannot
    // evaluate: no path of that module may be presented as applicable.
    if module_reasons.contains(&ReasonCode::DeviationSemanticsUnsupported)
        && status == ApplicabilityStatus::Applicable
    {
        status = ApplicabilityStatus::Unknown;
    }

    // Legacy tree: no datastore→schema link — nothing is `applicable`.
    if status == ApplicabilityStatus::Applicable && !effective.datastore_linked {
        status = ApplicabilityStatus::Candidate;
        reasons.insert(ReasonCode::DatastoreSchemaUnverified);
    }

    // Blocked evidence resolution → unknown, never not_applicable.
    if let Some(blocked) = effective.blocked {
        reasons.insert(blocked);
        if status != ApplicabilityStatus::NotApplicable {
            status = ApplicabilityStatus::Unknown;
        }
    }

    PathResult {
        path: entry.path.clone(),
        module: entry.module.clone(),
        module_revision: entry.module_revision.clone(),
        namespace: entry.namespace.clone(),
        kind: entry.kind.clone(),
        config: entry.config,
        keys: entry.keys.clone(),
        type_name: entry.type_name.clone(),
        resolved_type: entry.resolved_type.clone(),
        units: entry.units.clone(),
        required_features: entry.required_features.clone(),
        status,
        reasons: reasons.into_iter().collect(),
    }
}

fn not_applicable_if_positive(status: ApplicabilityStatus) -> ApplicabilityStatus {
    match status {
        ApplicabilityStatus::Applicable | ApplicabilityStatus::Candidate => {
            ApplicabilityStatus::NotApplicable
        }
        other => other,
    }
}

// ── feature state ──────────────────────────────────────────────────────

fn build_feature_states(
    catalogue: &CompiledCatalogue,
    device_modules: &BTreeMap<String, DeviceModuleEvidence>,
) -> BTreeMap<String, ModuleFeatureState> {
    let mut states = BTreeMap::new();
    for cm in &catalogue.modules {
        let declared: BTreeSet<String> = cm.features.iter().map(|f| f.name.clone()).collect();
        let ev = device_modules.get(&cm.name);
        let (enabled, known) = match ev {
            Some(e) if e.features_state_known => (e.enabled_features.clone(), true),
            _ => (BTreeSet::new(), false),
        };
        states.insert(
            cm.name.clone(),
            ModuleFeatureState {
                prefix: cm.prefix.clone(),
                enabled,
                declared,
                known,
            },
        );
    }
    states
}

/// Resolve one feature atom (`prefix:local` or bare `local`) to a
/// tri-state truth. `prefix` is the declaring module's prefix (the
/// compiler canonicalizes if-feature atoms to that form).
fn module_feature_truth(st: &ModuleFeatureState, local: &str) -> FeatureTruth {
    if !st.known {
        FeatureTruth::Unknown
    } else if st.enabled.contains(local) {
        FeatureTruth::True
    } else if st.declared.contains(local) {
        // Declared but not reported enabled → provably disabled.
        FeatureTruth::False
    } else {
        FeatureTruth::Unknown
    }
}

fn feature_atom_truth(atom: &str, states: &BTreeMap<String, ModuleFeatureState>) -> FeatureTruth {
    if let Some((prefix, local)) = atom.split_once(':') {
        for st in states.values() {
            if st.prefix == prefix && st.declared.contains(local) {
                return module_feature_truth(st, local);
            }
        }
        return FeatureTruth::Unknown;
    }
    // Bare local name: every module declaring it must agree, and
    // unknown state anywhere makes the atom unproven.
    let mut acc: Option<FeatureTruth> = None;
    for st in states.values() {
        if !st.declared.contains(atom) {
            continue;
        }
        let t = module_feature_truth(st, atom);
        acc = Some(match acc {
            None => t,
            Some(a) if a == t => a,
            Some(_) => return FeatureTruth::Unknown,
        });
    }
    acc.unwrap_or(FeatureTruth::Unknown)
}

// ── datastore capabilities ─────────────────────────────────────────────

/// (supports_config, supports_state) for a datastore identity.
fn datastore_capabilities(name: Option<&str>, diagnostics: &mut Vec<String>) -> (bool, bool) {
    let Some(name) = name else {
        return (true, true);
    };
    match local_name(name) {
        "running" => (true, true),
        "operational" => (false, true),
        "candidate" | "startup" | "intended" => (true, false),
        other => {
            diagnostics.push(format!(
                "unrecognized datastore identity '{other}' — assuming combined config+state scope"
            ));
            (true, true)
        }
    }
}

// ── overall status ─────────────────────────────────────────────────────

fn overall_status(
    catalogue_authoritative: bool,
    effective: &EffectiveSet,
    yang_library: Option<&YangLibraryOutcome>,
    module_results: &[ModuleResult],
    input: &ApplicabilityInput<'_>,
    reasons: &mut BTreeSet<ReasonCode>,
    diagnostics: &mut Vec<String>,
) -> ApplicabilityStatus {
    let _ = diagnostics;
    if input.device.identity_ambiguous {
        reasons.insert(ReasonCode::IdentityAmbiguity);
        return ApplicabilityStatus::Conflicted;
    }
    if let Some(outcome) = yang_library
        && matches!(
            outcome,
            YangLibraryOutcome::ConflictingModuleDefinitions { .. }
        )
    {
        reasons.insert(ReasonCode::YangLibraryConflictingModules);
        return ApplicabilityStatus::Conflicted;
    }
    if let Some(blocked) = effective.blocked {
        reasons.insert(blocked);
        return ApplicabilityStatus::Unknown;
    }
    if !catalogue_authoritative {
        return ApplicabilityStatus::Unknown;
    }
    if module_results
        .iter()
        .any(|m| m.status == ApplicabilityStatus::Conflicted)
    {
        return ApplicabilityStatus::Conflicted;
    }
    // Authoritative complete resolution: verified catalogue + complete
    // NMDA library + established datastore link.
    if effective.authoritative && effective.datastore_linked {
        return ApplicabilityStatus::Applicable;
    }
    // Weaker evidence tiers still produce candidate-level results.
    if module_results
        .iter()
        .any(|m| m.status != ApplicabilityStatus::Unknown)
        || input.capabilities.is_some()
    {
        return ApplicabilityStatus::Candidate;
    }
    ApplicabilityStatus::Unknown
}

// ── fingerprint ────────────────────────────────────────────────────────

/// Deterministic evidence fingerprint: identical catalogue + device
/// evidence always yields the same value. Covers schema identity,
/// module-set identity, selected datastore, device identity binding and
/// the advertised model set — the inputs that determine the result.
fn fingerprint(
    input: &ApplicabilityInput<'_>,
    catalogue: Option<&CompiledCatalogue>,
    effective: Option<&EffectiveSet>,
) -> String {
    #[derive(Serialize)]
    struct FingerprintInput<'a> {
        schema_id: Option<&'a str>,
        content_id: Option<&'a str>,
        datastore: Option<&'a str>,
        device_identity: &'a str,
        collector_id: Option<&'a str>,
        requested_datastore: Option<&'a str>,
        platform: Option<&'a str>,
        models: Vec<AdvertisedModel>,
        library_modules: Vec<(String, Option<String>, bool)>,
    }
    let mut models = input
        .capabilities
        .map(|c| c.models.clone())
        .unwrap_or_default();
    models.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.organization.cmp(&b.organization))
            .then_with(|| a.version.cmp(&b.version))
    });
    models.dedup();
    let mut library_modules: Vec<(String, Option<String>, bool)> = effective
        .map(|e| {
            e.modules
                .values()
                .map(|m| (m.name.clone(), m.revision.clone(), m.import_only))
                .collect()
        })
        .unwrap_or_default();
    library_modules.sort();
    library_modules.dedup();
    let device_identity = input.device.identity_key();
    let fp = FingerprintInput {
        schema_id: catalogue.map(|c| c.schema_id.as_str()),
        content_id: effective.and_then(|e| e.content_id.as_deref()),
        datastore: effective.and_then(|e| e.datastore.as_deref()),
        device_identity: &device_identity,
        collector_id: input.device.collector_id.as_deref(),
        requested_datastore: input.requested_datastore,
        platform: input.platform,
        models,
        library_modules,
    };
    let raw = serde_json::to_vec(&fp).unwrap_or_default();
    hex::encode(Sha256::digest(&raw))
}

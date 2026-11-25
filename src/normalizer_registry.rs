//! BN-024-03/03A — offline normalizer registry: the dispatch seam
//! between a *receipt-bound* `NosFeatureProfile` selection and a concrete
//! native normalizer.
//!
//! Scope: OFFLINE/shadow dispatch only. The registry verifies feature
//! applicability, exact implementation/mapping revisions, acquisition
//! contract shape, namespace scope, resource budgets and the binding
//! between selection metadata and the actual observation. Qualified
//! dispatch additionally requires an explicitly release-qualified profile
//! and exact observation release/hardware evidence. A separate explicit
//! experimental path exists for historical fixtures and regression
//! development; its output is machine-labelled unqualified and remains
//! shadow-only. No path in this module performs collection, I/O, graph
//! access, authorization or authoritative projection.

use crate::frr_interface_json::{
    FRR_JSON_MAPPING_REVISION, FRR_JSON_PARSER_REVISION, FrrInterfaceJsonObservation,
    normalize_frr_interface_json,
};
use crate::interface_normalizer::{
    INTERFACE_CLI_MAPPING_REVISION, INTERFACE_CLI_PARSER_REVISION, InterfaceObservation,
    NormalizationOutput, NormalizerLimits, NormalizerNos, ObservationContext,
    normalize_with_limits,
};
use crate::nos_features::{
    AcquisitionKind, FeatureStatus, HardwareScope, ProfileRegistry, PromotionState,
    ReleaseConstraint, Selection, SelectionReceipt, SoftwareRelease,
};

/// Normalizer implementations known to this registry. Ids match the
/// `NormalizerRef.id` values profiles declare — profiles never name
/// code paths or executables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalizerKind {
    /// `interface_normalizer` — the FRR `show int vrf all` +
    /// `ip -d address show` CLI observation pair.
    InterfaceFrrCli,
    /// `interface_frr_json_normalizer` — standalone FRR
    /// `show interface json` structured output.
    InterfaceFrrJson,
}

/// Smallest useful versioned input extension for the interface feature:
/// exact input variants preserve source format identity instead of an
/// unrestricted JSON blob.
pub enum InterfaceObservationInput<'a, 'src> {
    CliPair(&'a InterfaceObservation<'src>),
    FrrJson(&'a FrrInterfaceJsonObservation<'src>),
}

/// One registered normalizer binding.
#[derive(Clone, Debug)]
struct NormalizerEntry {
    /// `NormalizerRef.id` this entry implements.
    id: &'static str,
    kind: NormalizerKind,
    /// Canonical platform families this normalizer implementation is
    /// allowed to serve. Platform qualification is still feature- and
    /// release-specific; this is only the hard implementation boundary.
    qualified_families: &'static [&'static str],
    /// Exact implementation revisions this binary executes.
    normalizer_revision: &'static str,
    mapping_revision: &'static str,
    /// Acquisition plane and logical source-format contracts required
    /// by the input type. These are identifiers, never runnable commands.
    acquisition_kind: AcquisitionKind,
    acquisition_contracts: &'static [&'static str],
}

/// Registry of offline normalizers. Immutable after construction;
/// adding a normalizer is a code change reviewed like any parser.
pub struct NormalizerRegistry {
    entries: Vec<NormalizerEntry>,
}

/// Why dispatch refused — always explicit, never a silent fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchError {
    /// Selection was not `Selected` (Candidate/Unsupported/Ambiguous/
    /// OutOfScope) — dispatch requires an executable selection.
    NotExecutableSelection,
    /// The feature is not enabled/applicable on the selected profile.
    FeatureNotEnabled {
        feature: String,
        status: FeatureStatus,
    },
    /// The profile data is internally inconsistent or incomplete.
    InvalidProfile { errors: Vec<String> },
    /// Dependency/conflict validation failed for the executable closure.
    FeatureCompositionInvalid { errors: Vec<String> },
    /// The feature declares no normalizer binding.
    NoNormalizerBound { feature: String },
    /// The declared normalizer id is not registered.
    UnknownNormalizer { id: String },
    /// The profile refers to a revision this binary does not implement.
    NormalizerRevisionMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    /// The profile refers to a mapping revision this binary does not emit.
    MappingRevisionMismatch {
        id: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    /// The normalizer is not qualified for this platform family.
    PlatformNotQualified { platform: String },
    /// The observation's declared NOS disagrees with the profile's
    /// platform — a cumulus normalizer path cannot be invoked under a
    /// sonic claim (or vice versa).
    ObservationPlatformMismatch {
        expected: NormalizerNos,
        got: NormalizerNos,
    },
    /// The concrete input variant does not match the input type implemented
    /// by the selected normalizer. This is the source-format boundary: an
    /// exact typed variant is required, never a generic JSON blob.
    ObservationInputMismatch {
        normalizer_id: String,
        feature: String,
        expected_input: String,
        actual_input: String,
    },
    /// The feature's declared acquisition contracts do not contain the
    /// source-format contract required by the registered input type.
    AcquisitionContractMismatch {
        feature: String,
        required_kind: AcquisitionKind,
        required_contracts: Vec<String>,
    },
    /// The profile is recognized but is not release-qualified evidence
    /// for executable dispatch. Use explicit experimental dispatch for
    /// fixture/regression development and label the result accordingly.
    ProfileNotReleaseQualified { profile: String, reason: String },
    /// Runtime metadata needed to validate the selection/observation
    /// boundary is absent.
    ObservationMetadataMissing { field: String },
    /// Canonical platform evidence on the observation differs from the
    /// canonical platform that justified selection.
    ObservationPlatformMetadataMismatch { expected: String, actual: String },
    /// The observation's explicit acquisition contract differs from the
    /// contract required by the selected feature/normalizer.
    ObservationAcquisitionContractMismatch {
        expected: Vec<String>,
        actual: String,
    },
    /// The observation namespace is absent, unknown or not verified for
    /// this feature/profile.
    NamespaceNotQualified {
        namespace: Option<String>,
        status: FeatureStatus,
    },
    /// The actual observation release evidence differs from the release
    /// that justified selection or fails the profile constraint.
    ObservationReleaseMismatch { reason: String },
    /// The actual observation hardware/scope evidence differs from the
    /// hardware metadata that justified selection.
    ObservationHardwareMismatch { field: String, reason: String },
}

/// Whether a run satisfied the qualified dispatch boundary or was an
/// explicitly unqualified experiment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionQualification {
    /// Exact profile/release/namespace/acquisition/revision validation
    /// succeeded. This is still shadow evidence, not production authority.
    Qualified,
    /// Explicit fixture/regression execution that bypassed release and/or
    /// namespace qualification. Never eligible for operational promotion.
    ExperimentalUnqualified,
}

/// Result of a successful dispatch: normalization output plus the exact
/// applicability/implementation contract used. `shadow_only` is always
/// true at this development stage, irrespective of declarative
/// `PromotionState::Active` on profile data.
pub struct NormalizerRun {
    pub output: NormalizationOutput,
    /// Always true: promotion beyond shadow requires the separate
    /// qualification, authorization and projection gates.
    pub shadow_only: bool,
    pub qualification: ExecutionQualification,
    /// Machine-readable reasons an experimental run is unqualified.
    pub qualification_limitations: Vec<String>,
    pub profile_id: String,
    pub profile_fingerprint: Option<String>,
    pub normalizer_id: String,
    pub normalizer_revision: String,
    pub mapping_revision: Option<String>,
    pub acquisition_contracts: Vec<String>,
    /// Declarative promotion state on the profile. It never relaxes
    /// `shadow_only` in this integration stage.
    pub declared_promotion: PromotionState,
}

impl std::fmt::Debug for NormalizerRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NormalizerRun")
            .field("profile_id", &self.profile_id)
            .field("qualification", &self.qualification)
            .field("shadow_only", &self.shadow_only)
            .field("normalizer_id", &self.normalizer_id)
            .field("normalizer_revision", &self.normalizer_revision)
            .field("mapping_revision", &self.mapping_revision)
            .field("facts_emitted", &self.output.report.facts_emitted)
            .field("facts_len", &self.output.facts.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchMode {
    Qualified,
    Experimental,
}

impl NormalizerRegistry {
    /// The built-in registry: currently only the PR #86 interface
    /// normalizer, bound to its exact emitted parser/mapping revisions.
    pub fn builtin() -> Self {
        Self {
            entries: vec![
                NormalizerEntry {
                    id: "interface_normalizer",
                    kind: NormalizerKind::InterfaceFrrCli,
                    qualified_families: &["sonic", "nvidia.cumulus"],
                    normalizer_revision: INTERFACE_CLI_PARSER_REVISION,
                    mapping_revision: INTERFACE_CLI_MAPPING_REVISION,
                    acquisition_kind: AcquisitionKind::CliObservation,
                    acquisition_contracts: &["vtysh_show_int_vrf_all", "ip_addr_show"],
                },
                NormalizerEntry {
                    id: "interface_frr_json_normalizer",
                    kind: NormalizerKind::InterfaceFrrJson,
                    qualified_families: &["frr"],
                    normalizer_revision: FRR_JSON_PARSER_REVISION,
                    mapping_revision: FRR_JSON_MAPPING_REVISION,
                    acquisition_kind: AcquisitionKind::CliObservation,
                    acquisition_contracts: &["frr_vtysh_show_interface_json"],
                },
            ],
        }
    }

    /// Qualified offline dispatch. This path fails closed unless the
    /// profile is explicitly release-qualified, the actual observation
    /// carries matching release/hardware/namespace evidence and every
    /// revision/acquisition/dependency check passes.
    pub fn dispatch(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        obs: &InterfaceObservation,
    ) -> Result<NormalizerRun, DispatchError> {
        self.dispatch_inner(
            receipt,
            feature,
            &obs.context,
            Some(obs.nos),
            NormalizerKind::InterfaceFrrCli,
            None,
            DispatchMode::Qualified,
            |limits| normalize_with_limits(obs, limits),
        )
    }

    /// Explicit experimental offline dispatch for historical fixtures,
    /// parser development and regression tests. This bypasses release and
    /// namespace *qualification* only; it still validates implementation
    /// identity, revisions, acquisition contract shape, platform/NOS
    /// consistency, declarative disable/unsupported states and resource
    /// budgets. The result is permanently labelled unqualified and
    /// shadow-only.
    pub fn dispatch_experimental(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        obs: &InterfaceObservation,
    ) -> Result<NormalizerRun, DispatchError> {
        self.dispatch_inner(
            receipt,
            feature,
            &obs.context,
            Some(obs.nos),
            NormalizerKind::InterfaceFrrCli,
            None,
            DispatchMode::Experimental,
            |limits| normalize_with_limits(obs, limits),
        )
    }

    /// Dispatch the smallest versioned input envelope. Exact variants
    /// preserve source-format identity; no generic JSON blob is accepted.
    pub fn dispatch_input(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        input: InterfaceObservationInput<'_, '_>,
    ) -> Result<NormalizerRun, DispatchError> {
        match input {
            InterfaceObservationInput::CliPair(obs) => self.dispatch(receipt, feature, obs),
            InterfaceObservationInput::FrrJson(obs) => {
                self.dispatch_frr_json(receipt, feature, obs)
            }
        }
    }

    /// Experimental counterpart of [`Self::dispatch_input`].
    pub fn dispatch_input_experimental(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        input: InterfaceObservationInput<'_, '_>,
    ) -> Result<NormalizerRun, DispatchError> {
        match input {
            InterfaceObservationInput::CliPair(obs) => {
                self.dispatch_experimental(receipt, feature, obs)
            }
            InterfaceObservationInput::FrrJson(obs) => {
                self.dispatch_frr_json_experimental(receipt, feature, obs)
            }
        }
    }

    /// Qualified dispatch of standalone-FRR structured interface JSON.
    pub fn dispatch_frr_json(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        obs: &FrrInterfaceJsonObservation,
    ) -> Result<NormalizerRun, DispatchError> {
        self.dispatch_inner(
            receipt,
            feature,
            &obs.context,
            None,
            NormalizerKind::InterfaceFrrJson,
            Some(obs.acquisition_contract),
            DispatchMode::Qualified,
            |limits| normalize_frr_interface_json(obs, limits),
        )
    }

    /// Explicit experimental dispatch of standalone-FRR structured JSON.
    pub fn dispatch_frr_json_experimental(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        obs: &FrrInterfaceJsonObservation,
    ) -> Result<NormalizerRun, DispatchError> {
        self.dispatch_inner(
            receipt,
            feature,
            &obs.context,
            None,
            NormalizerKind::InterfaceFrrJson,
            Some(obs.acquisition_contract),
            DispatchMode::Experimental,
            |limits| normalize_frr_interface_json(obs, limits),
        )
    }

    fn dispatch_inner<F>(
        &self,
        receipt: &SelectionReceipt,
        feature: &str,
        context: &ObservationContext<'_>,
        actual_nos: Option<NormalizerNos>,
        input_kind: NormalizerKind,
        actual_acquisition_contract: Option<&str>,
        mode: DispatchMode,
        invoke: F,
    ) -> Result<NormalizerRun, DispatchError>
    where
        F: FnOnce(NormalizerLimits) -> NormalizationOutput,
    {
        let Selection::Selected(profile) = receipt.selection() else {
            return Err(DispatchError::NotExecutableSelection);
        };
        let profile_errors = ProfileRegistry::validate_profile(profile);
        if !profile_errors.is_empty() {
            return Err(DispatchError::InvalidProfile {
                errors: profile_errors,
            });
        }
        let flag =
            profile
                .features
                .get(feature)
                .ok_or_else(|| DispatchError::FeatureNotEnabled {
                    feature: feature.to_string(),
                    status: FeatureStatus::Absent,
                })?;
        if !flag.enabled {
            return Err(DispatchError::FeatureNotEnabled {
                feature: feature.to_string(),
                status: FeatureStatus::Disabled,
            });
        }

        let nref = flag
            .normalizer
            .as_ref()
            .ok_or_else(|| DispatchError::NoNormalizerBound {
                feature: feature.to_string(),
            })?;
        let entry = self
            .entries
            .iter()
            .find(|e| e.id == nref.id)
            .ok_or_else(|| DispatchError::UnknownNormalizer {
                id: nref.id.clone(),
            })?;
        if entry.kind != input_kind {
            return Err(DispatchError::ObservationInputMismatch {
                normalizer_id: entry.id.to_string(),
                feature: feature.to_string(),
                expected_input: input_kind_label(entry.kind).to_string(),
                actual_input: input_kind_label(input_kind).to_string(),
            });
        }
        if nref.revision != entry.normalizer_revision {
            return Err(DispatchError::NormalizerRevisionMismatch {
                id: nref.id.clone(),
                expected: nref.revision.clone(),
                actual: entry.normalizer_revision.to_string(),
            });
        }
        let expected_mapping = nref.mapping_revision.clone();
        let actual_mapping = Some(entry.mapping_revision.to_string());
        if expected_mapping != actual_mapping {
            return Err(DispatchError::MappingRevisionMismatch {
                id: nref.id.clone(),
                expected: expected_mapping,
                actual: actual_mapping,
            });
        }

        let family = platform_family(&profile.platform_family);
        if !entry
            .qualified_families
            .iter()
            .any(|f| *f == profile.platform_family || *f == family)
        {
            return Err(DispatchError::PlatformNotQualified {
                platform: profile.platform_family.clone(),
            });
        }
        if let Some(expected) = nos_for_platform(&profile.platform_family) {
            if actual_nos != Some(expected) {
                return Err(DispatchError::ObservationPlatformMismatch {
                    expected,
                    got: actual_nos.unwrap_or(expected),
                });
            }
        } else if entry.kind != NormalizerKind::InterfaceFrrJson {
            return Err(DispatchError::PlatformNotQualified {
                platform: profile.platform_family.clone(),
            });
        }

        let acquisition_contracts =
            validate_acquisition_contract(flag, entry).ok_or_else(|| {
                DispatchError::AcquisitionContractMismatch {
                    feature: feature.to_string(),
                    required_kind: entry.acquisition_kind,
                    required_contracts: entry
                        .acquisition_contracts
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                }
            })?;

        if let Some(actual) = actual_acquisition_contract
            && !acquisition_contracts
                .iter()
                .any(|expected| expected == actual)
        {
            return Err(DispatchError::ObservationAcquisitionContractMismatch {
                expected: acquisition_contracts.clone(),
                actual: actual.to_string(),
            });
        }

        let namespace = context.network_namespace.filter(|ns| !ns.trim().is_empty());
        let ns_status =
            namespace.map(|ns| ProfileRegistry::feature_status_in_namespace(profile, feature, ns));
        let device_status = ProfileRegistry::feature_status(profile, feature);
        let mut limitations = Vec::new();
        match mode {
            DispatchMode::Qualified => {
                let Some(ns) = namespace else {
                    return Err(DispatchError::ObservationMetadataMissing {
                        field: "network_namespace".to_string(),
                    });
                };
                let status = ns_status.unwrap_or(FeatureStatus::Unknown);
                if status != FeatureStatus::Enabled {
                    return Err(DispatchError::NamespaceNotQualified {
                        namespace: Some(ns.to_string()),
                        status,
                    });
                }
                let closure = ProfileRegistry::dependency_closure(profile, feature);
                if let Err(errors) =
                    ProfileRegistry::validate_feature_set_in_scope(profile, &closure, ns)
                {
                    return Err(DispatchError::FeatureCompositionInvalid { errors });
                }
            }
            DispatchMode::Experimental => {
                let status = ns_status.unwrap_or(device_status);
                match status {
                    FeatureStatus::Enabled => {}
                    FeatureStatus::Unknown | FeatureStatus::OutOfScope => {
                        limitations.push(format!(
                            "feature/namespace applicability is {status:?}; experimental execution only"
                        ));
                    }
                    other => {
                        return Err(DispatchError::FeatureNotEnabled {
                            feature: feature.to_string(),
                            status: other,
                        });
                    }
                }
                if namespace.is_none() {
                    limitations.push(
                        "observation network namespace missing; exact scope unqualified"
                            .to_string(),
                    );
                }
            }
        }
        if mode == DispatchMode::Qualified {
            if let Some(err) = platform_binding_error(receipt, context) {
                return Err(err);
            }
            validate_release_qualification(profile, receipt, context)?;
            validate_hardware_binding(receipt.hardware(), context)?;
        } else {
            if let Some(err) = platform_binding_error(receipt, context) {
                match err {
                    DispatchError::ObservationMetadataMissing { .. } => limitations.push(
                        "observation canonical platform missing; experimental execution only"
                            .to_string(),
                    ),
                    other => return Err(other),
                }
            }
            if !is_release_qualified_profile(profile) {
                limitations.push(
                    "profile has no exact release-qualified constraint and verification evidence"
                        .to_string(),
                );
            }
            if validate_observation_release(profile, receipt, context).is_err() {
                limitations.push(
                    "observation release missing or different from selection metadata".to_string(),
                );
            }
        }

        let mut limits = NormalizerLimits::default();
        if let Some(v) = profile.resource_budget.max_input_bytes {
            limits.max_input_bytes = limits
                .max_input_bytes
                .min(usize::try_from(v).unwrap_or(usize::MAX));
        }
        if let Some(v) = profile.resource_budget.max_entities {
            limits.max_entities = limits
                .max_entities
                .min(usize::try_from(v).unwrap_or(usize::MAX));
        }
        if let Some(v) = profile.resource_budget.max_output_facts {
            limits.max_output_facts = limits
                .max_output_facts
                .min(usize::try_from(v).unwrap_or(usize::MAX));
        }

        let output = invoke(limits);
        if flag.promotion == PromotionState::Active {
            limitations.push(
                "declared PromotionState::Active ignored at this stage; output remains shadow-only"
                    .to_string(),
            );
        }
        Ok(NormalizerRun {
            output,
            shadow_only: true,
            qualification: match mode {
                DispatchMode::Qualified => ExecutionQualification::Qualified,
                DispatchMode::Experimental => ExecutionQualification::ExperimentalUnqualified,
            },
            qualification_limitations: limitations,
            profile_id: profile.profile_id.clone(),
            profile_fingerprint: receipt.profile_fingerprint().map(str::to_string),
            normalizer_id: entry.id.to_string(),
            normalizer_revision: entry.normalizer_revision.to_string(),
            mapping_revision: Some(entry.mapping_revision.to_string()),
            acquisition_contracts,
            declared_promotion: flag.promotion,
        })
    }
}

fn canonical_observation_platform(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_lowercase();
    let canonical = crate::platform_registry::normalize_platform_id(&trimmed);
    if canonical != "unknown" || trimmed == "unknown" {
        canonical.to_string()
    } else {
        trimmed
    }
}

fn platform_binding_error(
    receipt: &SelectionReceipt,
    context: &ObservationContext<'_>,
) -> Option<DispatchError> {
    let Some(actual_raw) = context.platform.filter(|p| !p.trim().is_empty()) else {
        return Some(DispatchError::ObservationMetadataMissing {
            field: "platform".to_string(),
        });
    };
    let actual = canonical_observation_platform(actual_raw);
    let expected = receipt.canonical_platform().to_string();
    (actual != expected)
        .then(|| DispatchError::ObservationPlatformMetadataMismatch { expected, actual })
}

fn input_kind_label(kind: NormalizerKind) -> &'static str {
    match kind {
        NormalizerKind::InterfaceFrrCli => "frr_cli_command_pair",
        NormalizerKind::InterfaceFrrJson => "frr_show_interface_json",
    }
}

fn validate_acquisition_contract(
    flag: &crate::nos_features::FeatureFlag,
    entry: &NormalizerEntry,
) -> Option<Vec<String>> {
    flag.acquisition.iter().find_map(|a| {
        if a.kind != entry.acquisition_kind {
            return None;
        }
        let missing = entry
            .acquisition_contracts
            .iter()
            .any(|required| !a.contracts.iter().any(|got| got == required));
        (!missing).then(|| {
            entry
                .acquisition_contracts
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
    })
}

fn is_release_qualified_profile(profile: &crate::nos_features::NosFeatureProfile) -> bool {
    profile.verification == "release_qualified"
        && profile
            .release_constraint
            .as_ref()
            .is_some_and(|rc| !rc.is_empty())
}

fn validate_release_qualification(
    profile: &crate::nos_features::NosFeatureProfile,
    receipt: &SelectionReceipt,
    context: &ObservationContext<'_>,
) -> Result<(), DispatchError> {
    if !is_release_qualified_profile(profile) {
        return Err(DispatchError::ProfileNotReleaseQualified {
            profile: profile.profile_id.clone(),
            reason: "profile verification is not 'release_qualified' with a non-empty exact release constraint".to_string(),
        });
    }
    validate_observation_release(profile, receipt, context)
}

fn observation_release(context: &ObservationContext<'_>) -> Option<SoftwareRelease> {
    let has_any = context.release_raw.is_some()
        || context.release_train.is_some()
        || context.release_build.is_some()
        || !context.release_components.is_empty();
    if !has_any {
        return None;
    }
    Some(SoftwareRelease {
        raw: context.release_raw.unwrap_or_default().to_string(),
        train: context.release_train.map(str::to_string),
        build: context.release_build.map(str::to_string),
        components: context.release_components.clone(),
    })
}

fn validate_observation_release(
    profile: &crate::nos_features::NosFeatureProfile,
    receipt: &SelectionReceipt,
    context: &ObservationContext<'_>,
) -> Result<(), DispatchError> {
    let Some(expected) = receipt.effective_release() else {
        return Err(DispatchError::ObservationReleaseMismatch {
            reason: "selection receipt contains no known release metadata".to_string(),
        });
    };
    if !expected.has_known_raw() {
        return Err(DispatchError::ObservationReleaseMismatch {
            reason: "selection release raw value is unknown".to_string(),
        });
    }
    let Some(actual) = observation_release(context) else {
        return Err(DispatchError::ObservationMetadataMissing {
            field: "release_raw".to_string(),
        });
    };
    if !actual.has_known_raw() {
        return Err(DispatchError::ObservationMetadataMissing {
            field: "release_raw".to_string(),
        });
    }
    if actual.raw != expected.raw {
        return Err(DispatchError::ObservationReleaseMismatch {
            reason: format!(
                "selected release '{}' differs from observation release '{}'",
                expected.raw, actual.raw
            ),
        });
    }
    for (axis, expected_v, actual_v) in [
        (
            "release_train",
            expected.train.as_deref(),
            context.release_train,
        ),
        (
            "release_build",
            expected.build.as_deref(),
            context.release_build,
        ),
    ] {
        if let Some(exp) = expected_v
            && actual_v != Some(exp)
        {
            return Err(DispatchError::ObservationReleaseMismatch {
                reason: format!("{axis} expected '{exp}', got {actual_v:?}"),
            });
        }
    }
    for (component, expected_version) in &expected.components {
        match context.release_components.get(component) {
            Some(got) if got == expected_version => {}
            got => {
                return Err(DispatchError::ObservationReleaseMismatch {
                    reason: format!(
                        "component '{component}' expected '{expected_version}', got {got:?}"
                    ),
                });
            }
        }
    }
    if let Some(rc) = &profile.release_constraint
        && !rc.matches(Some(&actual))
    {
        return Err(DispatchError::ObservationReleaseMismatch {
            reason: format!(
                "observation release does not satisfy exact profile constraint {:?}",
                constraint_summary(rc)
            ),
        });
    }
    Ok(())
}

fn constraint_summary(rc: &ReleaseConstraint) -> String {
    format!(
        "raw={:?} train={:?} build={:?} components={:?}",
        rc.raw, rc.train, rc.build, rc.components
    )
}

fn validate_hardware_binding(
    query_hardware: Option<&HardwareScope>,
    context: &ObservationContext<'_>,
) -> Result<(), DispatchError> {
    let Some(expected) = query_hardware else {
        return Ok(());
    };
    let check = |field: &str, expected_v: &Option<String>, actual_v: Option<&str>| {
        if let Some(exp) = expected_v
            && actual_v != Some(exp.as_str())
        {
            return Err(DispatchError::ObservationHardwareMismatch {
                field: field.to_string(),
                reason: format!("expected '{exp}', got {actual_v:?}"),
            });
        }
        Ok(())
    };
    check("platform_sku", &expected.platform_sku, context.platform_sku)?;
    check("asic", &expected.asic, context.asic)?;
    check("namespace", &expected.namespace, context.network_namespace)?;
    for (key, expected_value) in &expected.attributes {
        let actual = context.hardware_attributes.get(key);
        if actual != Some(expected_value) {
            return Err(DispatchError::ObservationHardwareMismatch {
                field: format!("hardware_attributes.{key}"),
                reason: format!("expected '{expected_value}', got {actual:?}"),
            });
        }
    }
    Ok(())
}

/// Family portion of a canonical platform id — same rule as
/// `nos_features::platform_family` (kept local to avoid widening its
/// visibility contract).
fn platform_family(platform: &str) -> &str {
    platform.split(['.', '-']).next().unwrap_or(platform)
}

/// The `NormalizerNos` a platform's FRR CLI observation path uses.
/// Only the two qualified platforms map; anything else is refused.
fn nos_for_platform(platform_family_id: &str) -> Option<NormalizerNos> {
    match platform_family_id {
        "sonic" => Some(NormalizerNos::Sonic),
        "nvidia.cumulus" | "cumulus" => Some(NormalizerNos::Cumulus),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    const FRR_LAB_ARTIFACT: &str = "quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b";

    use crate::interface_normalizer::{CaptureOutcome, CommandCapture, ObservationContext};
    use crate::nos_features::{ProfileQuery, builtin_profiles, builtin_registry};

    fn obs(nos: NormalizerNos) -> InterfaceObservation<'static> {
        let mut context =
            ObservationContext::minimal("dev-1", "col-1", "cap-1", "2026-11-25T00:00:00Z");
        context.session_epoch = 1;
        context.platform = Some(match nos {
            NormalizerNos::Sonic => "sonic",
            NormalizerNos::Cumulus => "nvidia.cumulus",
        });
        InterfaceObservation {
            nos,
            vtysh: CommandCapture::new("", CaptureOutcome::Complete),
            ip_addr: CommandCapture::new("", CaptureOutcome::Complete),
            context,
        }
    }

    fn frr_json_obs<'a>(
        json: &'a str,
        namespace: Option<&'a str>,
        release: Option<&'a str>,
    ) -> crate::frr_interface_json::FrrInterfaceJsonObservation<'a> {
        let mut context =
            ObservationContext::minimal("frr2", "col-1", "cap-frr-json", "2026-11-25T00:00:00Z");
        context.session_epoch = 1;
        context.network_namespace = namespace;
        context.platform = Some("frr");
        context.release_raw = release;
        context.release_build = release.map(|_| FRR_LAB_ARTIFACT);
        crate::frr_interface_json::FrrInterfaceJsonObservation {
            capture: CommandCapture::new(json, CaptureOutcome::Complete),
            context,
            acquisition_contract: crate::frr_interface_json::FRR_JSON_ACQUISITION_CONTRACT,
        }
    }

    fn frr_receipt(reg: &crate::nos_features::ProfileRegistry) -> SelectionReceipt {
        reg.select_receipt(&ProfileQuery {
            platform: "frr".into(),
            version: "10.6.1_git".into(),
            release: Some(SoftwareRelease {
                raw: "10.6.1_git".into(),
                train: None,
                build: Some(FRR_LAB_ARTIFACT.into()),
                components: BTreeMap::new(),
            }),
            hardware: None,
        })
    }

    #[test]
    fn standalone_frr_json_exact_release_dispatch_is_qualified_and_shadow_only() {
        let json = include_str!(
            "../tests/fixtures/bn024/interface-frr-json/show-interface-json-frr-10.6.1.json"
        );
        let profiles = builtin_registry();
        let receipt = frr_receipt(&profiles);
        assert!(matches!(receipt.selection(), Selection::Selected(_)));
        let nr = NormalizerRegistry::builtin();
        let obs = frr_json_obs(json, Some("default"), Some("10.6.1_git"));
        let run = nr
            .dispatch_frr_json(&receipt, "interfaces.frr_json", &obs)
            .expect("genuine exact-release structured dispatch");
        assert_eq!(run.qualification, ExecutionQualification::Qualified);
        assert!(run.shadow_only);
        assert_eq!(run.profile_id, "frr-10.6.1-structured-interfaces");
        assert_eq!(
            run.normalizer_revision,
            crate::frr_interface_json::FRR_JSON_PARSER_REVISION
        );
        assert_eq!(
            run.acquisition_contracts,
            vec!["frr_vtysh_show_interface_json"]
        );
        assert_eq!(run.output.facts.len(), 4);
        assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
        assert!(run.output.facts.iter().all(|f| f.authority.is_none()));

        let via_envelope = nr
            .dispatch_input(
                &receipt,
                "interfaces.frr_json",
                InterfaceObservationInput::FrrJson(&obs),
            )
            .expect("versioned input envelope dispatch");
        assert_eq!(via_envelope.output.facts.len(), 4);
    }

    #[test]
    fn standalone_frr_json_rejects_wrong_release_and_namespace() {
        let json = include_str!(
            "../tests/fixtures/bn024/interface-frr-json/show-interface-json-frr-10.6.1.json"
        );
        let profiles = builtin_registry();
        let receipt = frr_receipt(&profiles);
        let nr = NormalizerRegistry::builtin();
        for obs in [
            frr_json_obs(json, Some("default"), Some("10.7.0")),
            frr_json_obs(json, Some("asic-0"), Some("10.6.1_git")),
            frr_json_obs(json, None, Some("10.6.1_git")),
        ] {
            assert!(
                nr.dispatch_frr_json(&receipt, "interfaces.frr_json", &obs)
                    .is_err()
            );
        }
    }

    #[test]
    fn same_git_version_with_different_build_artifact_is_not_qualified() {
        let json = include_str!(
            "../tests/fixtures/bn024/interface-frr-json/show-interface-json-frr-10.6.1.json"
        );
        let profiles = builtin_registry();
        let receipt = frr_receipt(&profiles);
        assert!(matches!(receipt.selection(), Selection::Selected(_)));
        let nr = NormalizerRegistry::builtin();
        let mut obs = frr_json_obs(json, Some("default"), Some("10.6.1_git"));
        obs.context.release_build = Some(
            "quay.io/frrouting/frr@sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(matches!(
            nr.dispatch_frr_json(&receipt, "interfaces.frr_json", &obs),
            Err(DispatchError::ObservationReleaseMismatch { .. })
        ));

        // A query with only the mutable `_git` version string and no
        // immutable artifact identity does not even select the constrained
        // profile for executable use.
        let weak_receipt = profiles.select_receipt(&ProfileQuery {
            platform: "frr".into(),
            version: "10.6.1_git".into(),
            release: None,
            hardware: None,
        });
        assert!(!matches!(weak_receipt.selection(), Selection::Selected(_)));
    }

    #[test]
    fn builtin_cli_profile_is_not_release_qualified() {
        let reg = builtin_registry();
        let receipt = reg.select_receipt(&ProfileQuery {
            platform: "nvidia.cumulus".into(),
            version: "5.13.0".into(),
            release: None,
            hardware: None,
        });
        let nr = NormalizerRegistry::builtin();
        assert!(matches!(
            nr.dispatch(&receipt, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus)),
            Err(DispatchError::ObservationMetadataMissing { .. })
                | Err(DispatchError::ProfileNotReleaseQualified { .. })
                | Err(DispatchError::NamespaceNotQualified { .. })
        ));
        let run = nr
            .dispatch_experimental(&receipt, "interfaces.frr_cli", &obs(NormalizerNos::Cumulus))
            .expect("explicit experimental fixture execution");
        assert_eq!(
            run.qualification,
            ExecutionQualification::ExperimentalUnqualified
        );
        assert!(run.shadow_only);
        assert!(!run.qualification_limitations.is_empty());
        assert_eq!(run.output.facts.len(), 1);
        assert!(run.output.facts.iter().all(|f| f.validate().is_ok()));
    }

    #[test]
    fn wrong_observation_input_shape_fails_closed() {
        let json = include_str!(
            "../tests/fixtures/bn024/interface-frr-json/show-interface-json-frr-10.6.1.json"
        );
        let profiles = builtin_registry();
        let frr_receipt = frr_receipt(&profiles);
        let nr = NormalizerRegistry::builtin();

        // The FRR JSON feature cannot consume the CLI command pair.
        let cli = obs(NormalizerNos::Cumulus);
        assert!(matches!(
            nr.dispatch(&frr_receipt, "interfaces.frr_json", &cli),
            Err(DispatchError::ObservationInputMismatch { .. })
        ));

        // Conversely, the CLI feature cannot consume structured JSON even
        // when both are interface observations.
        let mut reg = ProfileRegistry::new();
        let mut p = builtin_profiles()
            .into_iter()
            .find(|p| p.profile_id == "cumulus-1")
            .expect("builtin cumulus");
        p.profile_id = "shape-contract-test".into();
        p.version_match = "=shape-test-release".into();
        p.release_constraint = Some(ReleaseConstraint {
            raw: Some("shape-test-release".into()),
            train: None,
            build: None,
            components: Default::default(),
        });
        p.verified_namespaces = vec!["default".into()];
        p.verification = "release_qualified".into();
        reg.register(p);
        let receipt = reg.select_receipt(&ProfileQuery {
            platform: "nvidia.cumulus".into(),
            version: "shape-test-release".into(),
            release: None,
            hardware: None,
        });
        let structured = frr_json_obs(json, Some("default"), Some("shape-test-release"));
        assert!(matches!(
            nr.dispatch_frr_json(&receipt, "interfaces.frr_cli", &structured),
            Err(DispatchError::ObservationInputMismatch { .. })
        ));
    }
}

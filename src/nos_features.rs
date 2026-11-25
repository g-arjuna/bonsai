//! BN-024-03 — bounded extension seam: NOS feature-profile contract.
//!
//! Defines the *contract* a future-NOS or feature extension must satisfy:
//! a declarative `NosFeatureProfile` selected by precedence rules against
//! the existing `platform_registry`, with explicit unsupported/disabled
//! outcomes. This module performs **no** collection, parsing, graph
//! mutation, or authority projection — it is the registration/selection
//! seam BN-024-06+ normalizers will plug into.
//!
//! Stability note: built on `typed_facts` v1 + `platform_registry`; both
//! are versioned contracts. Changes here require the same review bar.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Advisory support claim for a feature — distinct from `enabled`
/// (profile wiring) and from `verification` (evidence status). `Unknown`
/// is the default: absence of evidence never becomes support.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    /// Documented/observed support within the declared scope.
    Supported,
    /// Explicitly unsupported on this platform/release/hardware.
    Unsupported,
    /// Support cannot be established — never treated as absent/down and
    /// never silently selects a parser.
    Unknown,
}

fn unknown_support() -> SupportState {
    SupportState::Unknown
}

/// Provenance for a support claim. Advisory evidence only: a document or
/// device-advertised claim never grants collection or graph authority and
/// never alters executable parser rules (BN-028-13 boundary).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicabilityEvidence {
    /// Evidence kind: "vendor_document" | "device_advertised" |
    /// "observed_capture" | "standard" | "internal_test".
    pub kind: String,
    /// Human-readable source reference (doc title, RFC, fixture id).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// What the evidence claims about support.
    pub claim: SupportState,
}

/// How a feature's input is acquired. Declarative provenance only —
/// variants name the acquisition *plane*, never an executable command,
/// shell string, eval expression or plugin path. `contracts` holds
/// logical contract identifiers (e.g. observation-source names or
/// schema paths), not runnable text.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionKind {
    /// CLI observation output (e.g. vtysh/iproute2 text captures).
    CliObservation,
    /// Vendor structured state database (e.g. SONiC STATE_DB).
    StructuredStateDb,
    /// Management API (e.g. NVUE REST).
    ApiRest,
    /// gNMI subscription/GET.
    Gnmi,
    /// Kernel structured state (netlink/sysfs).
    KernelNetlink,
    /// Acquisition plane not established — never treated as supported.
    Unknown,
}

/// One typed acquisition contract for a feature. `contracts` are
/// logical identifiers a future collector resolves through its own
/// authorized acquisition path; they are data, not commands.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcquisitionContract {
    pub kind: AcquisitionKind,
    #[serde(default)]
    pub contracts: Vec<String>,
}

/// Reference to the normalizer/mapping implementation bound to a
/// feature. `id` is resolved by the offline normalizer registry —
/// profiles never name code paths, shared libraries or executables.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NormalizerRef {
    pub id: String,
    /// Normalizer implementation revision.
    pub revision: String,
    /// Optional mapping/semantics revision (typed-fact `mapping_revision`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_revision: Option<String>,
}

/// Promotion/activation state of a feature binding. `Shadow` is the
/// default and the only state this contract can express for
/// fixture-qualified normalizers: output is evidence for comparison,
/// never an authoritative input. `Active` is reserved for profiles
/// whose qualification evidence exists; flipping it is a profile-data
/// change, fully reversible by re-selecting the profile.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    #[default]
    Shadow,
    Active,
}

/// Per-profile resource budgets. `None` = unbounded is NOT implied —
/// consumers must apply their own defaults; these bounds only tighten
/// what a profile declares safe.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entities: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_facts: Option<u64>,
}

/// A single feature a profile claims support for, with compositional
/// semantics (BN-024-03A): prerequisites, conflicts and evidence are
/// first-class so feature sets like "IS-IS + SR-MPLS + Flex-Algo" or
/// "SR policy via PCE" are validated as combinations, not single flags.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeatureFlag {
    /// Whether the feature is enabled for this profile.
    pub enabled: bool,
    /// Parser backend ids permitted for this feature (e.g. "genie",
    /// "textfsm", "native_line_split"). Empty = no parser bound yet.
    #[serde(default)]
    pub parsers: Vec<String>,
    /// Free-form scope notes (ASIC namespace, transport, limits).
    #[serde(default)]
    pub scope_notes: Vec<String>,
    /// Feature keys that must also be enabled for this feature to apply
    /// (e.g. "isis.sr_mpls" requires "isis").
    #[serde(default)]
    pub requires: Vec<String>,
    /// Feature keys that cannot coexist with this feature.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Declared support state; `Unknown` unless evidence upgrades it.
    #[serde(default = "unknown_support")]
    pub support: SupportState,
    /// Advisory applicability evidence (BN-028-13 seam).
    #[serde(default)]
    pub evidence: Vec<ApplicabilityEvidence>,
    /// Typed acquisition contracts — the planes this feature may draw
    /// observations from. Empty = no acquisition path declared.
    #[serde(default)]
    pub acquisition: Vec<AcquisitionContract>,
    /// Bound normalizer/mapping implementation, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalizer: Option<NormalizerRef>,
    /// Shadow-only by default; see [`PromotionState`].
    #[serde(default)]
    pub promotion: PromotionState,
}

/// Device software release as reported by the NOS: opaque raw string plus
/// optional structured components (train/build/component-daemon versions
/// such as FRR or NVUE). Ordering is intentionally absent — NOS release
/// formats differ and are not semver.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SoftwareRelease {
    /// Exact vendor-reported version string (e.g. "7.0.3", "5.13.0",
    /// "2025.1"). Empty/"*" means unknown.
    pub raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Component/daemon versions keyed by component name
    /// (e.g. "frr" → "10.6.1", "nvue" → "...", "kernel" → "...").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, String>,
}

/// Equality-only release constraint: every specified field must equal the
/// device's reported value. No ranges, ordering or semver — a device whose
/// release is unknown or partially reported fails closed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseConstraint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub train: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Component versions that must match exactly, e.g. {"frr": "10.6.1"}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, String>,
}

impl ReleaseConstraint {
    /// True iff the constraint specifies nothing (matches all) or every
    /// specified dimension equals the release's reported value. A `None`
    /// argument (unknown release) never matches a non-empty constraint.
    pub fn matches(&self, release: Option<&SoftwareRelease>) -> bool {
        if self.raw.is_none()
            && self.train.is_none()
            && self.build.is_none()
            && self.components.is_empty()
        {
            return true;
        }
        let Some(rel) = release else {
            return false;
        };
        if let Some(raw) = &self.raw
            && rel.raw != *raw
        {
            return false;
        }
        if let Some(train) = &self.train
            && rel.train.as_deref() != Some(train.as_str())
        {
            return false;
        }
        if let Some(build) = &self.build
            && rel.build.as_deref() != Some(build.as_str())
        {
            return false;
        }
        self.components
            .iter()
            .all(|(k, v)| rel.components.get(k).is_some_and(|got| got == v))
    }
}

/// Hardware/ASIC/namespace scope — used both as device description (on a
/// query) and as an applicability constraint (on a profile). Every `Some`
/// field on a constraint must equal the device's reported value; `None`
/// fields are unconstrained. Extra vendor dimensions go in `attributes`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_sku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asic: Option<String>,
    /// Namespace/instance scope (e.g. SONiC "asic-0", FRR "default").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Vendor-specific dimensions (hwsku, linecard, optics, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl HardwareScope {
    /// Constraint match: all specified fields must equal `device`'s.
    /// `None` device satisfies only an empty constraint.
    pub fn constraint_matches(&self, device: Option<&HardwareScope>) -> bool {
        let specifies = self.platform_sku.is_some()
            || self.asic.is_some()
            || self.namespace.is_some()
            || !self.attributes.is_empty();
        if !specifies {
            return true;
        }
        let Some(dev) = device else {
            return false;
        };
        if let Some(sku) = &self.platform_sku
            && dev.platform_sku.as_deref() != Some(sku.as_str())
        {
            return false;
        }
        if let Some(asic) = &self.asic
            && dev.asic.as_deref() != Some(asic.as_str())
        {
            return false;
        }
        if let Some(ns) = &self.namespace
            && dev.namespace.as_deref() != Some(ns.as_str())
        {
            return false;
        }
        self.attributes
            .iter()
            .all(|(k, v)| dev.attributes.get(k).is_some_and(|got| got == v))
    }
}

/// Declarative per-NOS feature profile. `platform_family` matches the
/// canonical platform registry family (e.g. "cisco", "arista");
/// `version_match` is a legacy exact/prefix constraint — new profiles
/// should prefer `release_constraint`, which is equality-based and
/// NOS-format-agnostic.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NosFeatureProfile {
    pub profile_id: String,
    /// Canonical platform family or exact platform id this profile covers.
    pub platform_family: String,
    /// Optional version constraint: "*" (any), "=X.Y", or prefix "X.Y".
    /// Lexical only — retained for existing profiles; prefer
    /// `release_constraint` for NOS-specific release semantics.
    #[serde(default = "any_version")]
    pub version_match: String,
    /// Structured release constraint (BN-024-03A). When present it must
    /// match the query's `release`; profiles without one match any known
    /// release but still fail closed on unknown hardware constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_constraint: Option<ReleaseConstraint>,
    /// Hardware/ASIC constraints: the query's `hardware` must satisfy
    /// every entry. Empty = no hardware constraint declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hardware_constraints: Vec<HardwareScope>,
    /// Named ASIC/namespace scopes the profile is verified for. Empty means
    /// "unspecified" — consumers must treat multi-namespace devices as
    /// unverified rather than assuming coverage.
    #[serde(default)]
    pub verified_namespaces: Vec<String>,
    /// Device roles this profile describes (e.g. "leaf", "spine",
    /// "border"). Informational scope only — role never selects a
    /// profile by itself.
    #[serde(default)]
    pub device_roles: Vec<String>,
    /// Per-profile resource budgets for bounded execution.
    #[serde(default)]
    pub resource_budget: ResourceBudget,
    #[serde(default)]
    pub features: BTreeMap<String, FeatureFlag>,
    /// Test/verification status — profiles are `untested` until evidence is
    /// attached; selection never upgrades this implicitly.
    #[serde(default = "untested_status")]
    pub verification: String,
}

fn any_version() -> String {
    "*".to_string()
}
fn untested_status() -> String {
    "untested".to_string()
}

/// Outcome of profile selection — always explicit, never a silent default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Best-matching profile for (platform, version) with a KNOWN
    /// release — may drive executable native parsing.
    Selected(Box<NosFeatureProfile>),
    /// Metadata/discovery candidate only: a wildcard profile matched a
    /// platform whose release is UNKNOWN. May identify a candidate for
    /// inventory/coverage display but must never authorize executable
    /// native parsing.
    Candidate(Box<NosFeatureProfile>),
    /// Platform family known but no profile covers this version.
    UnsupportedVersion { platform: String, version: String },
    /// No profile registered for the platform family at all.
    UnsupportedPlatform { platform: String },
    /// A profile matched platform/version but its release or hardware
    /// constraint rejected the query scope (e.g. unknown ASIC, missing
    /// component version). Distinct from version unsupported: the release
    /// may be known while the hardware/namespace scope is not covered.
    OutOfScope { platform: String, reason: String },
    /// Two or more profiles are equally applicable — explicit conflict,
    /// never arbitrary first-match selection.
    Ambiguous {
        platform: String,
        candidates: Vec<String>,
    },
}

/// Query for profile selection.
#[derive(Clone, Debug)]
pub struct ProfileQuery {
    /// Canonical platform id or family (e.g. "cisco.iosxe", "cisco").
    /// Case-insensitive; matched against canonical lowercase ids.
    pub platform: String,
    /// NOS version string; "" or "*" matches any-version profiles only.
    /// Legacy convenience — when `release` is absent this becomes the
    /// release's `raw` component for `release_constraint` matching.
    pub version: String,
    /// Structured device release. `None` = unknown — profiles with a
    /// `release_constraint` will not select.
    pub release: Option<SoftwareRelease>,
    /// Device hardware/ASIC/namespace scope. `None` = unspecified.
    pub hardware: Option<HardwareScope>,
}

/// Effective support assessment for a feature after consulting the
/// declared `support` state AND advisory evidence. Evidence can
/// contradict or negatively qualify a declaration but can never upgrade
/// `Unknown` to `Supported` — a vendor document is a scoped reference
/// claim, not proof of runtime applicability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportAssessment {
    Supported,
    Unsupported,
    Unknown,
    /// Declared support and evidence claims contradict each other.
    Conflicting,
}

/// Compute the effective support state of a feature flag.
fn assess_support(flag: &FeatureFlag) -> SupportAssessment {
    let has_unsupported_evidence = flag
        .evidence
        .iter()
        .any(|e| e.claim == SupportState::Unsupported);
    let has_supported_evidence = flag
        .evidence
        .iter()
        .any(|e| e.claim == SupportState::Supported);
    let has_unknown_evidence = flag
        .evidence
        .iter()
        .any(|e| e.claim == SupportState::Unknown);
    let _ = has_unknown_evidence;
    match flag.support {
        SupportState::Unsupported => SupportAssessment::Unsupported,
        SupportState::Supported => {
            // Explicit unsupported evidence against a supported
            // declaration is contradictory — never silently resolved.
            if has_unsupported_evidence {
                SupportAssessment::Conflicting
            } else {
                SupportAssessment::Supported
            }
        }
        SupportState::Unknown => {
            if has_unsupported_evidence && has_supported_evidence {
                SupportAssessment::Conflicting
            } else if has_unsupported_evidence {
                // Negative evidence qualifies an unknown declaration.
                SupportAssessment::Unsupported
            } else {
                // Advisory supported/unknown evidence never upgrades an
                // unknown declaration to proven support.
                SupportAssessment::Unknown
            }
        }
    }
}

/// Applicability of an observation namespace on a selected profile.
/// Distinct from device-level profile selection: a profile may be
/// recognized for a device while specific namespaces remain unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceStatus {
    /// Namespace is explicitly verified by the profile.
    Applicable,
    /// Namespace is known but NOT verified by this profile.
    Unsupported,
    /// Namespace coverage is unspecified — must not be assumed.
    Unknown,
}

/// Whether `feature` applies on `profile` at observation `namespace`.
/// `None` namespace = device/feature-level applicability only (no
/// observation scope claimed).
pub fn namespace_status(profile: &NosFeatureProfile, namespace: &str) -> NamespaceStatus {
    if namespace.is_empty() || profile.verified_namespaces.is_empty() {
        return NamespaceStatus::Unknown;
    }
    if profile.verified_namespaces.iter().any(|n| n == namespace) {
        NamespaceStatus::Applicable
    } else {
        NamespaceStatus::Unsupported
    }
}

/// Registry of feature profiles. Profiles register declaratively; core
/// collectors/graph code never change when a new NOS is added.
pub struct ProfileRegistry {
    profiles: Vec<NosFeatureProfile>,
}

impl ProfileRegistry {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
        }
    }

    pub fn register(&mut self, profile: NosFeatureProfile) {
        self.profiles.push(profile);
    }

    /// Select the best profile for a query. Precedence:
    /// 1. exact platform id + exact version match
    /// 2. exact platform id + any-version
    /// 3. family match + exact version
    /// 4. family match + any-version
    /// Ties broken by registration order (first wins) — deterministic.
    ///
    /// A profile additionally qualifies only when its `release_constraint`
    /// (if any) matches the query release and every `hardware_constraints`
    /// entry is satisfied by the query hardware scope. Unknown release or
    /// unspecified hardware fails closed against constrained profiles.
    /// This registry describes *feature applicability* only — canonical
    /// device identity remains `platform_registry`'s responsibility.
    pub fn select(&self, query: &ProfileQuery) -> Selection {
        let platform = query.platform.trim().to_ascii_lowercase();
        let platform = platform.as_str();
        let family = platform_family(platform);
        let version = query.version.as_str();
        // Effective release: explicit structured release wins; otherwise
        // the legacy version string becomes the raw component so existing
        // `release_constraint { raw }` profiles still work.
        let effective_release;
        let release = match &query.release {
            Some(r) => Some(r),
            None if !version.is_empty() && version != "*" => {
                effective_release = SoftwareRelease {
                    raw: version.to_string(),
                    train: None,
                    build: None,
                    components: BTreeMap::new(),
                };
                Some(&effective_release)
            }
            None => None,
        };

        // Structured release and the legacy version string must not
        // silently contradict each other when both are supplied.
        let metadata_conflict = match (&query.release, version) {
            (Some(r), v) if !v.is_empty() && v != "*" && r.raw != v => true,
            _ => false,
        };

        let platform_exact =
            |p: &NosFeatureProfile| p.platform_family == platform || p.platform_family == family;
        // A profile is scope-applicable only when its release and hardware
        // constraints accept the query AND its own release metadata is
        // internally consistent. Constraint failures are tracked so a
        // known-but-out-of-scope platform reports OutOfScope rather than
        // UnsupportedVersion/Platform.
        let scope_ok = |p: &NosFeatureProfile| -> Result<(), String> {
            if metadata_conflict {
                return Err(
                    "conflicting release metadata: structured release and version disagree"
                        .to_string(),
                );
            }
            // Self-consistency: a pinned legacy version constraint that
            // contradicts the structured release constraint's raw value
            // is a broken profile, not a match.
            if let (Some(rc), true) = (&p.release_constraint, p.version_match != "*")
                && let Some(raw) = &rc.raw
                && !version_matches(&p.version_match, raw)
            {
                return Err(format!(
                    "profile release metadata contradicts itself (version_match '{}' vs release raw '{raw}')",
                    p.version_match
                ));
            }
            if let Some(rc) = &p.release_constraint
                && !rc.matches(release)
            {
                return Err(format!(
                    "release constraint not satisfied (profile requires {:?})",
                    rc.raw.as_deref().unwrap_or("<structured>")
                ));
            }
            for hc in &p.hardware_constraints {
                if !hc.constraint_matches(query.hardware.as_ref()) {
                    return Err(format!("hardware constraint not satisfied ({hc:?})"));
                }
            }
            Ok(())
        };
        let version_pinned = |p: &NosFeatureProfile| {
            p.version_match != "*" && version_matches(&p.version_match, version)
        };

        let mut best_tier = 0u8;
        let mut best: Vec<&NosFeatureProfile> = Vec::new();
        let mut scope_failure: Option<String> = None;
        for p in &self.profiles {
            if !platform_exact(p) {
                continue;
            }
            if let Err(reason) = scope_ok(p) {
                scope_failure.get_or_insert(reason);
                continue;
            }
            let tier = if p.platform_family == platform && version_pinned(p) {
                4
            } else if p.platform_family == platform && p.version_match == "*" {
                3
            } else if p.platform_family == family && version_pinned(p) {
                2
            } else if p.platform_family == family && p.version_match == "*" {
                1
            } else {
                continue;
            };
            if tier > best_tier {
                best_tier = tier;
                best.clear();
            }
            if tier == best_tier {
                best.push(p);
            }
        }
        // Ambiguity is explicit: equally applicable profiles — including
        // duplicate registrations — conflict rather than silently picking
        // registration order.
        if best.len() > 1 {
            return Selection::Ambiguous {
                platform: platform.to_string(),
                candidates: best.iter().map(|p| p.profile_id.clone()).collect(),
            };
        }
        if let Some(p) = best.first() {
            // A wildcard match against an UNKNOWN release is a discovery
            // candidate only — never executable applicability.
            if release.is_none() && p.version_match == "*" {
                return Selection::Candidate(Box::new((*p).clone()));
            }
            return Selection::Selected(Box::new((*p).clone()));
        }
        if self.profiles.iter().any(|p| platform_exact(p)) {
            if let Some(reason) = scope_failure {
                return Selection::OutOfScope {
                    platform: platform.to_string(),
                    reason,
                };
            }
            Selection::UnsupportedVersion {
                platform: platform.to_string(),
                version: version.to_string(),
            }
        } else {
            Selection::UnsupportedPlatform {
                platform: platform.to_string(),
            }
        }
    }

    /// Registry self-check: duplicate profile ids and internally
    /// contradictory release metadata are rejected explicitly. Selection
    /// skips self-inconsistent profiles, but callers registering profiles
    /// from untrusted config should run this first.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut seen = std::collections::BTreeMap::new();
        for p in &self.profiles {
            if seen.insert(p.profile_id.as_str(), ()).is_some() {
                errors.push(format!("duplicate profile_id '{}'", p.profile_id));
            }
            if p.version_match != "*"
                && let Some(rc) = &p.release_constraint
                && let Some(raw) = &rc.raw
                && !version_matches(&p.version_match, raw)
            {
                errors.push(format!(
                    "profile '{}' release metadata contradicts itself",
                    p.profile_id
                ));
            }
        }
        errors
    }

    /// Feature lookup on a selected profile — disabled, unsupported,
    /// unknown and absent features are distinct outcomes. This is the
    /// device/feature-level view: it does NOT establish applicability for
    /// a specific observation namespace — use
    /// [`Self::feature_status_in_namespace`] for that.
    pub fn feature_status(profile: &NosFeatureProfile, feature: &str) -> FeatureStatus {
        Self::feature_status_inner(profile, feature, None)
    }

    /// Feature lookup scoped to an observation namespace (e.g. SONiC
    /// "asic-0"). A selected profile does not authorize normalization of
    /// an unverified namespace: `asic-2`/`host`/unknown scopes return
    /// explicit non-applicable statuses, never silent `Enabled`.
    pub fn feature_status_in_namespace(
        profile: &NosFeatureProfile,
        feature: &str,
        namespace: &str,
    ) -> FeatureStatus {
        Self::feature_status_inner(profile, feature, Some(namespace))
    }

    fn feature_status_inner(
        profile: &NosFeatureProfile,
        feature: &str,
        namespace: Option<&str>,
    ) -> FeatureStatus {
        let Some(flag) = profile.features.get(feature) else {
            return FeatureStatus::Absent;
        };
        if !flag.enabled {
            return FeatureStatus::Disabled;
        }
        match assess_support(flag) {
            SupportAssessment::Unsupported => return FeatureStatus::Unsupported,
            SupportAssessment::Conflicting => return FeatureStatus::Conflicting,
            SupportAssessment::Unknown => return FeatureStatus::Unknown,
            SupportAssessment::Supported => {}
        }
        if let Some(ns) = namespace {
            match namespace_status(profile, ns) {
                NamespaceStatus::Applicable => {}
                NamespaceStatus::Unsupported => return FeatureStatus::OutOfScope,
                NamespaceStatus::Unknown => return FeatureStatus::Unknown,
            }
        }
        FeatureStatus::Enabled
    }

    /// Validate a requested feature *set* as a composition (BN-024-03A):
    /// every feature must exist and be applicable, all `requires`
    /// prerequisites must be declared, applicable and present in the set,
    /// no `conflicts` pair may co-occur, and the `requires` graph must be
    /// acyclic. Returns every violation found — callers get the full
    /// conflict list, not just the first.
    ///
    /// Device/feature-level check: pass `None` for namespace-insensitive
    /// validation; use [`Self::validate_feature_set_in_scope`] when an
    /// observation namespace must be verified.
    ///
    /// This checks declarative applicability only; it does not implement
    /// IS-IS/SR/PCE semantics and grants no collection or graph authority.
    pub fn validate_feature_set(
        profile: &NosFeatureProfile,
        requested: &[String],
    ) -> Result<(), Vec<String>> {
        Self::validate_feature_set_inner(profile, requested, None)
    }

    /// Composition check scoped to an observation namespace: every
    /// requested feature must additionally be verified for `namespace`.
    pub fn validate_feature_set_in_scope(
        profile: &NosFeatureProfile,
        requested: &[String],
        namespace: &str,
    ) -> Result<(), Vec<String>> {
        Self::validate_feature_set_inner(profile, requested, Some(namespace))
    }

    fn validate_feature_set_inner(
        profile: &NosFeatureProfile,
        requested: &[String],
        namespace: Option<&str>,
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let status_of = |name: &str| -> FeatureStatus {
            match namespace {
                Some(ns) => Self::feature_status_in_namespace(profile, name, ns),
                None => Self::feature_status(profile, name),
            }
        };
        let set: std::collections::BTreeSet<&str> = requested.iter().map(String::as_str).collect();
        for feature in requested {
            match status_of(feature) {
                FeatureStatus::Absent => {
                    errors.push(format!("feature '{feature}' is not declared"))
                }
                FeatureStatus::Disabled => errors.push(format!("feature '{feature}' is disabled")),
                FeatureStatus::Unsupported => {
                    errors.push(format!("feature '{feature}' is unsupported"))
                }
                FeatureStatus::Unknown => {
                    errors.push(format!("feature '{feature}' support is unknown"))
                }
                FeatureStatus::Conflicting => errors.push(format!(
                    "feature '{feature}' has conflicting support evidence"
                )),
                FeatureStatus::OutOfScope => errors.push(format!(
                    "feature '{feature}' is not verified for the observation namespace"
                )),
                FeatureStatus::Enabled => {}
            }
            if let Some(flag) = profile.features.get(feature) {
                for req in &flag.requires {
                    if !profile.features.contains_key(req) {
                        errors.push(format!(
                            "feature '{feature}' requires undeclared feature '{req}'"
                        ));
                        continue;
                    }
                    if !set.contains(req.as_str()) {
                        errors.push(format!(
                            "feature '{feature}' requires '{req}' which is not in the set"
                        ));
                        continue;
                    }
                    // A prerequisite that is itself disabled, unsupported,
                    // unknown or out of scope cannot ground applicability.
                    match status_of(req) {
                        FeatureStatus::Enabled => {}
                        other => errors.push(format!(
                            "prerequisite '{req}' of '{feature}' is not applicable ({other:?})"
                        )),
                    }
                }
            }
        }
        for feature in requested {
            if let Some(flag) = profile.features.get(feature) {
                for conflict in &flag.conflicts {
                    if set.contains(conflict.as_str()) {
                        errors.push(format!("features '{feature}' and '{conflict}' conflict"));
                    }
                }
            }
        }
        // Cycle detection over the declared requires graph (bounded:
        // only edges between declared features are followed).
        fn visit(
            node: &str,
            profile: &NosFeatureProfile,
            state: &mut std::collections::BTreeMap<String, u8>,
            errors: &mut Vec<String>,
        ) {
            match state.get(node) {
                Some(1) => {
                    errors.push(format!("circular feature dependency at '{node}'"));
                    return;
                }
                Some(2) => return,
                _ => {}
            }
            state.insert(node.to_string(), 1);
            if let Some(flag) = profile.features.get(node) {
                for req in &flag.requires {
                    if profile.features.contains_key(req) {
                        visit(req, profile, state, errors);
                    }
                }
            }
            state.insert(node.to_string(), 2);
        }
        let mut state = std::collections::BTreeMap::new();
        for feature in requested {
            visit(feature, profile, &mut state, &mut errors);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureStatus {
    /// Enabled AND evidence-backed supported AND (when a namespace is
    /// given) verified for that observation scope — applicable.
    Enabled,
    Disabled,
    /// Feature not declared by the profile — consumers must treat as
    /// unsupported, never as implicitly enabled.
    Absent,
    /// Explicitly unsupported on this platform/release/hardware — a
    /// boolean `enabled` flag cannot override this.
    Unsupported,
    /// Support cannot be established — never treated as absent/down and
    /// never silently selects a parser.
    Unknown,
    /// Declared support and applicability evidence contradict each other.
    Conflicting,
    /// Feature is enabled but the observation namespace/scope is not
    /// verified by this profile (e.g. SONiC "asic-2"/"host").
    OutOfScope,
}

/// Extract the family portion of a canonical platform id
/// ("cisco.iosxe" → "cisco", "sonic" → "sonic").
fn platform_family(platform: &str) -> &str {
    platform.split(['.', '-']).next().unwrap_or(platform)
}

/// Version constraint match. Two DISTINCT declared policies:
/// - `"*"` matches any *known* version string (callers must separately
///   reject unknown/empty versions for executable use — see `select`).
/// - `"=X"` is EXACT equality only — `"=17.9"` never matches `"17.9.1"`.
/// - `"X"`/`"X.Y"` without `=` is the declared prefix policy:
///   `"17.9"` matches `"17.9"` and `"17.9.1"` but not `"17.10"`.
/// Empty or `"*"` device versions never match a constraining pattern.
fn version_matches(pattern: &str, version: &str) -> bool {
    if pattern == "*" {
        return !version.is_empty() && version != "*";
    }
    if version.is_empty() || version == "*" {
        return false;
    }
    if let Some(pat) = pattern.strip_prefix('=') {
        return version == pat;
    }
    version == pattern || version.starts_with(&format!("{pattern}."))
}

/// Canonical platform id for profile queries — delegates to
/// `platform_registry` so every registered alias resolves to one
/// identity. Profile selection never creates a second platform
/// namespace.
pub fn canonical_platform_id(raw: &str) -> &'static str {
    crate::platform_registry::normalize_platform_id(raw)
}

/// Built-in feature profiles for the platforms Bonsai currently has
/// *some* qualified native path for. These are declarative data —
/// registering them changes no collector, parser or graph code.
///
/// Qualification honesty:
/// - `version_match: "*"` records that upstream selectors declare
///   `version: all`; it is NOT tested-release support. Unknown device
///   releases select only as `Candidate` (discovery), never `Selected`.
/// - SONiC ASIC/host namespace separation applies to the structured
///   STATE_DB/COUNTERS_DB/APPL_DB/CONFIG_DB slices — NOT yet
///   implemented; `verified_namespaces` stays empty (unspecified) so
///   namespace-scoped checks fail closed as `Unknown`.
/// - Cumulus NVUE/kernel/FRR structured acquisition is likewise a
///   later slice; only the FRR CLI observation pair is bound.
/// - Standalone FRR is deliberately NOT given the cumulus/sonic
///   qualified interface normalizer: the observation path is
///   qualified for those two NOS profiles only.
/// Feature flag for a declared-but-not-yet-qualified builtin feature:
/// disabled, unknown support, no acquisition/normalizer binding,
/// shadow promotion. Used by [`builtin_profiles`]; the test module has
/// its own `flag` helper.
fn builtin_disabled_flag(scope_notes: Vec<String>) -> FeatureFlag {
    FeatureFlag {
        enabled: false,
        parsers: vec![],
        scope_notes,
        requires: vec![],
        conflicts: vec![],
        support: SupportState::Unknown,
        evidence: vec![],
        acquisition: vec![],
        normalizer: None,
        promotion: PromotionState::Shadow,
    }
}

pub fn builtin_profiles() -> Vec<NosFeatureProfile> {
    let cli_observation = || {
        vec![AcquisitionContract {
            kind: AcquisitionKind::CliObservation,
            // Logical contract identifiers — the observation-source
            // names the normalizer reports — never runnable commands.
            contracts: vec!["vtysh_show_int_vrf_all".into(), "ip_addr_show".into()],
        }]
    };
    let interface_flag = |support_evidence: Vec<ApplicabilityEvidence>| FeatureFlag {
        enabled: true,
        parsers: vec!["native_rust".into()],
        scope_notes: vec![
            "FRR 'show int vrf all' + 'ip -d address show' observation pair".into(),
            "offline normalization only — no live collector, no graph projection".into(),
        ],
        requires: vec![],
        conflicts: vec![],
        support: SupportState::Supported,
        evidence: support_evidence,
        acquisition: cli_observation(),
        normalizer: Some(NormalizerRef {
            id: "interface_normalizer".into(),
            revision: "bn024-iface-1".into(),
            mapping_revision: Some("bn024-map-1".into()),
        }),
        promotion: PromotionState::Shadow,
    };
    let fixture_evidence = || {
        vec![ApplicabilityEvidence {
            kind: "observed_capture".into(),
            source:
                "suzieq cl_interfaces.txt @75198b5 (genuine Cumulus capture, Apache-2.0 fixture)"
                    .into(),
            revision: Some("75198b5585ebef91f0659b6b1ec3c964cc2cd1ad".into()),
            scope: Some("interfaces.frr_cli only".into()),
            claim: SupportState::Supported,
        }]
    };
    vec![
        NosFeatureProfile {
            profile_id: "sonic-1".into(),
            platform_family: "sonic".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget {
                max_input_bytes: Some(8 * 1024 * 1024),
                max_entities: Some(4096),
                max_output_facts: Some(8192),
            },
            features: BTreeMap::from([
                (
                    "interfaces.frr_cli".into(),
                    interface_flag(fixture_evidence()),
                ),
                // Structured planes are declared but NOT enabled —
                // their acquisition slices are future work.
                (
                    "state_db".into(),
                    builtin_disabled_flag(vec![
                        "SONiC STATE_DB per-ASIC namespaces — acquisition slice pending".into(),
                    ]),
                ),
                (
                    "counters_db".into(),
                    builtin_disabled_flag(vec![
                        "SONiC COUNTERS_DB per-ASIC namespaces — acquisition slice pending".into(),
                    ]),
                ),
            ]),
            verification: "fixture_only".into(),
        },
        NosFeatureProfile {
            profile_id: "cumulus-1".into(),
            platform_family: "nvidia.cumulus".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec!["default".into()],
            device_roles: vec![],
            resource_budget: ResourceBudget {
                max_input_bytes: Some(8 * 1024 * 1024),
                max_entities: Some(4096),
                max_output_facts: Some(8192),
            },
            features: BTreeMap::from([
                (
                    "interfaces.frr_cli".into(),
                    interface_flag(fixture_evidence()),
                ),
                // NVUE applied/startup planes — declared but NOT
                // enabled; acquisition slice pending.
                (
                    "nvue".into(),
                    builtin_disabled_flag(vec![
                        "NVUE applied/startup planes — acquisition slice pending".into(),
                    ]),
                ),
            ]),
            verification: "fixture_only".into(),
        },
        NosFeatureProfile {
            profile_id: "frr-1".into(),
            platform_family: "frr".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::from([(
                "interfaces.frr_cli".into(),
                // Same observation syntax exists on standalone FRR,
                // but the normalizer is qualified for the cumulus/sonic
                // profiles only — disabled here, never silently reused.
                builtin_disabled_flag(vec![
                    "standalone FRR interface normalization not yet qualified".into(),
                ]),
            )]),
            verification: "untested".into(),
        },
    ]
}

/// A `ProfileRegistry` pre-loaded with [`builtin_profiles`]. Callers may
/// register additional profiles; selection precedence and ambiguity
/// rules are unchanged.
pub fn builtin_registry() -> ProfileRegistry {
    let mut r = ProfileRegistry::new();
    for p in builtin_profiles() {
        r.register(p);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(enabled: bool) -> FeatureFlag {
        FeatureFlag {
            enabled,
            parsers: vec![],
            scope_notes: vec![],
            requires: vec![],
            conflicts: vec![],
            support: SupportState::Supported,
            evidence: vec![],
            acquisition: vec![],
            normalizer: None,
            promotion: PromotionState::Shadow,
        }
    }

    fn sonic_profile() -> NosFeatureProfile {
        NosFeatureProfile {
            profile_id: "sonic-1".into(),
            platform_family: "sonic".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec!["asic-0".into(), "asic-1".into()],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::from([
                (
                    "interfaces".into(),
                    FeatureFlag {
                        enabled: true,
                        parsers: vec!["native_db".into()],
                        scope_notes: vec!["CONFIG_DB+STATE_DB per namespace".into()],
                        requires: vec![],
                        conflicts: vec![],
                        support: SupportState::Supported,
                        evidence: vec![],
                        acquisition: vec![],
                        normalizer: None,
                        promotion: PromotionState::Shadow,
                    },
                ),
                ("bgp".into(), flag(false)),
            ]),
            verification: "fixture_only".into(),
        }
    }

    /// Illustrative Cumulus Linux profile: NVUE/FRR/kernel planes are
    /// modeled as components + hardware scope, not as standalone FRR.
    fn cumulus_profile() -> NosFeatureProfile {
        NosFeatureProfile {
            profile_id: "cumulus-1".into(),
            platform_family: "cumulus".into(),
            version_match: "*".into(),
            release_constraint: Some(ReleaseConstraint {
                raw: Some("5.13.0".into()),
                train: None,
                build: None,
                components: BTreeMap::from([("frr".to_string(), "10.6.1".to_string())]),
            }),
            hardware_constraints: vec![HardwareScope {
                platform_sku: None,
                asic: Some("marvell".into()),
                namespace: None,
                attributes: BTreeMap::new(),
            }],
            verified_namespaces: vec!["default".into()],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::from([
                (
                    "interfaces".into(),
                    FeatureFlag {
                        enabled: true,
                        parsers: vec!["nvye_rest".into(), "frr_json".into()],
                        scope_notes: vec![
                            "NVUE applied vs startup config are separate planes".into(),
                            "kernel FIB/VRF via netlink".into(),
                        ],
                        requires: vec![],
                        conflicts: vec![],
                        support: SupportState::Supported,
                        evidence: vec![ApplicabilityEvidence {
                            kind: "vendor_document".into(),
                            source: "NVIDIA Cumulus Linux 5.13 NVUE API guide".into(),
                            revision: Some("5.13.0".into()),
                            scope: None,
                            claim: SupportState::Supported,
                        }],
                        acquisition: vec![],
                        normalizer: None,
                        promotion: PromotionState::Shadow,
                    },
                ),
                ("nvue".into(), flag(true)),
                ("frr_bgp".into(), flag(true)),
            ]),
            verification: "fixture_only".into(),
        }
    }

    /// Illustrative IS-IS/SR composition profile exercising requires/
    /// conflicts without implementing the protocols.
    fn isis_sr_profile() -> NosFeatureProfile {
        let mut features = BTreeMap::new();
        features.insert("isis".into(), flag(true));
        for (name, req) in [
            ("isis.sr_mpls", "isis"),
            ("isis.sr_srv6", "isis"),
            ("isis.flex_algo", "isis"),
        ] {
            let mut f = flag(true);
            f.requires = vec![req.to_string()];
            features.insert(name.to_string(), f);
        }
        let mut explicit = flag(true);
        explicit.requires = vec!["isis.sr_mpls".to_string()];
        features.insert("sr.policy.explicit".into(), explicit);
        let mut pce = flag(true);
        pce.requires = vec!["sr.policy.explicit".to_string()];
        features.insert("sr.policy.pce".into(), pce);
        // Illustrative conflict: SRv6 TE mode vs MPLS TE on this profile.
        let mut srv6_te = features.get("isis.sr_srv6").unwrap().clone();
        srv6_te.conflicts = vec!["isis.sr_mpls".to_string()];
        features.insert("isis.sr_srv6".into(), srv6_te);
        NosFeatureProfile {
            profile_id: "srnos-1".into(),
            // Synthetic NOS family: the requires/conflicts below exercise
            // the composition contract only — they are NOT claims about
            // real Arista EOS SR-MPLS/SRv6 mutual exclusivity.
            platform_family: "example.srnos".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features,
            verification: "untested".into(),
        }
    }

    fn future_nos_profile() -> NosFeatureProfile {
        NosFeatureProfile {
            profile_id: "futureos-1".into(),
            platform_family: "futureos".into(),
            version_match: "=2026.1".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::from([("interfaces".into(), flag(true))]),
            verification: "untested".into(),
        }
    }

    fn registry() -> ProfileRegistry {
        let mut r = ProfileRegistry::new();
        r.register(sonic_profile());
        r.register(cumulus_profile());
        r.register(isis_sr_profile());
        r.register(future_nos_profile());
        r
    }

    #[test]
    fn dummy_future_nos_registers_without_core_changes() {
        let r = registry();
        match r.select(&ProfileQuery {
            platform: "futureos.router".into(),
            version: "2026.1".into(),
            release: None,
            hardware: None,
        }) {
            Selection::Selected(p) => assert_eq!(p.profile_id, "futureos-1"),
            other => panic!("expected selection, got {other:?}"),
        }
    }

    #[test]
    fn wrong_version_is_explicitly_unsupported() {
        let r = registry();
        match r.select(&ProfileQuery {
            platform: "futureos.router".into(),
            version: "2025.9".into(),
            release: None,
            hardware: None,
        }) {
            Selection::UnsupportedVersion { .. } => {}
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn unknown_platform_is_explicitly_unsupported() {
        let r = registry();
        match r.select(&ProfileQuery {
            platform: "acme.unknown9k".into(),
            version: "1.0".into(),
            release: None,
            hardware: None,
        }) {
            Selection::UnsupportedPlatform { .. } => {}
            other => panic!("expected UnsupportedPlatform, got {other:?}"),
        }
    }

    #[test]
    fn sonic_namespace_fixture_selects_and_scopes() {
        let r = registry();
        let sel = r.select(&ProfileQuery {
            platform: "sonic".into(),
            version: "2025.1".into(),
            release: None,
            hardware: None,
        });
        let Selection::Selected(p) = sel else {
            panic!("no sonic profile")
        };
        assert_eq!(p.verified_namespaces, vec!["asic-0", "asic-1"]);
        // Feature disablement is explicit.
        assert_eq!(
            ProfileRegistry::feature_status(&p, "bgp"),
            FeatureStatus::Disabled
        );
        assert_eq!(
            ProfileRegistry::feature_status(&p, "evpn"),
            FeatureStatus::Absent
        );
        // Device-level feature status does not claim a namespace.
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Enabled
        );
    }

    #[test]
    fn namespace_scope_gates_observation_applicability() {
        let r = registry();
        let Selection::Selected(p) = r.select(&ProfileQuery {
            platform: "sonic".into(),
            version: "2025.1".into(),
            release: None,
            hardware: None,
        }) else {
            panic!("no sonic profile")
        };
        // Verified ASIC namespaces are applicable.
        for ns in ["asic-0", "asic-1"] {
            assert_eq!(
                ProfileRegistry::feature_status_in_namespace(&p, "interfaces", ns),
                FeatureStatus::Enabled,
                "{ns} must be applicable"
            );
        }
        // Unverified ASIC namespace and host scope are explicit
        // non-applicable — never silently Enabled.
        for ns in ["asic-2", "host"] {
            assert_eq!(
                ProfileRegistry::feature_status_in_namespace(&p, "interfaces", ns),
                FeatureStatus::OutOfScope,
                "{ns} must be out of scope"
            );
        }
        // Unknown/empty scope is Unknown, not applicable.
        assert_eq!(
            ProfileRegistry::feature_status_in_namespace(&p, "interfaces", ""),
            FeatureStatus::Unknown
        );
        assert_eq!(namespace_status(&p, "asic-2"), NamespaceStatus::Unsupported);
        // A profile with unspecified namespaces reports Unknown coverage.
        let Selection::Selected(sr) = r.select(&query("example.srnos", "1.0")) else {
            panic!("srnos profile must select")
        };
        assert_eq!(namespace_status(&sr, "default"), NamespaceStatus::Unknown);
        assert_eq!(
            ProfileRegistry::feature_status_in_namespace(&sr, "isis", "default"),
            FeatureStatus::Unknown
        );
        // Scoped composition validation enforces the same boundary.
        let errs =
            ProfileRegistry::validate_feature_set_in_scope(&p, &["interfaces".into()], "asic-2")
                .expect_err("asic-2 must fail scoped validation");
        assert!(errs.iter().any(|e| e.contains("namespace")));
        assert!(
            ProfileRegistry::validate_feature_set_in_scope(&p, &["interfaces".into()], "asic-0")
                .is_ok()
        );
    }

    #[test]
    fn support_state_gates_enabled_flag() {
        let mut p = sonic_profile();
        // enabled=true but support=Unsupported must not report Enabled.
        p.features.get_mut("interfaces").unwrap().support = SupportState::Unsupported;
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Unsupported
        );
        // enabled=true but support=Unknown reports Unknown.
        p.features.get_mut("interfaces").unwrap().support = SupportState::Unknown;
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Unknown
        );
        // Contradictory evidence against a Supported declaration.
        p.features.get_mut("interfaces").unwrap().support = SupportState::Supported;
        p.features.get_mut("interfaces").unwrap().evidence = vec![
            ApplicabilityEvidence {
                kind: "vendor_document".into(),
                source: "doc".into(),
                revision: None,
                scope: None,
                claim: SupportState::Supported,
            },
            ApplicabilityEvidence {
                kind: "observed_capture".into(),
                source: "fixture".into(),
                revision: None,
                scope: None,
                claim: SupportState::Unsupported,
            },
        ];
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Conflicting
        );
        // Advisory supported evidence alone never upgrades Unknown.
        p.features.get_mut("interfaces").unwrap().support = SupportState::Unknown;
        p.features.get_mut("interfaces").unwrap().evidence = vec![ApplicabilityEvidence {
            kind: "vendor_document".into(),
            source: "doc".into(),
            revision: None,
            scope: None,
            claim: SupportState::Supported,
        }];
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Unknown
        );
    }

    #[test]
    fn exact_version_is_exact_and_prefix_is_declared_policy() {
        let mut r = ProfileRegistry::new();
        r.register(NosFeatureProfile {
            profile_id: "fam-any".into(),
            platform_family: "cisco".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::new(),
            verification: "untested".into(),
        });
        r.register(NosFeatureProfile {
            profile_id: "cisco-17".into(),
            platform_family: "cisco.iosxe".into(),
            version_match: "=17.9".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::new(),
            verification: "fixture_only".into(),
        });
        r.register(NosFeatureProfile {
            profile_id: "cisco-17-prefix".into(),
            platform_family: "cisco.iosxe".into(),
            version_match: "17.9".into(), // declared prefix policy
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::new(),
            verification: "fixture_only".into(),
        });
        // "=17.9" is exact: it must NOT match "17.9.4". Only the
        // declared prefix profile "17.9" applies -> Selected.
        match r.select(&ProfileQuery {
            platform: "cisco.iosxe".into(),
            version: "17.9.4".into(),
            release: None,
            hardware: None,
        }) {
            Selection::Selected(p) => assert_eq!(p.profile_id, "cisco-17-prefix"),
            other => panic!("expected cisco-17-prefix, got {other:?}"),
        }
        // On the exact version "17.9" BOTH pinned profiles apply at the
        // same tier -> explicit Ambiguous, never first-match.
        match r.select(&ProfileQuery {
            platform: "cisco.iosxe".into(),
            version: "17.9".into(),
            release: None,
            hardware: None,
        }) {
            Selection::Ambiguous { candidates, .. } => {
                assert!(candidates.contains(&"cisco-17".to_string()));
                assert!(candidates.contains(&"cisco-17-prefix".to_string()));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn wildcard_on_unknown_release_is_discovery_only() {
        let r = registry();
        // Unknown release ("*"/empty version) cannot select executable.
        for v in ["*", ""] {
            match r.select(&ProfileQuery {
                platform: "sonic".into(),
                version: v.into(),
                release: None,
                hardware: None,
            }) {
                Selection::Candidate(p) => assert_eq!(p.profile_id, "sonic-1"),
                other => panic!("version '{v}' must be Candidate, got {other:?}"),
            }
        }
        // Conflicting structured/legacy release metadata is explicit.
        match r.select(&ProfileQuery {
            platform: "sonic".into(),
            version: "2025.1".into(),
            release: Some(SoftwareRelease {
                raw: "2024.2".into(),
                train: None,
                build: None,
                components: BTreeMap::new(),
            }),
            hardware: None,
        }) {
            Selection::OutOfScope { reason, .. } => {
                assert!(reason.contains("conflicting release metadata"), "{reason}")
            }
            other => panic!("expected OutOfScope, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_profiles_are_explicit_conflict() {
        let mut r = ProfileRegistry::new();
        for id in ["dup-a", "dup-b"] {
            r.register(NosFeatureProfile {
                profile_id: id.into(),
                platform_family: "acme".into(),
                version_match: "*".into(),
                release_constraint: None,
                hardware_constraints: vec![],
                verified_namespaces: vec![],
                device_roles: vec![],
                resource_budget: ResourceBudget::default(),
                features: BTreeMap::new(),
                verification: "untested".into(),
            });
        }
        match r.select(&ProfileQuery {
            platform: "acme.box".into(),
            version: "1.0".into(),
            release: None,
            hardware: None,
        }) {
            Selection::Ambiguous { candidates, .. } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // Registry validation flags duplicate ids.
        let mut r2 = ProfileRegistry::new();
        let p = NosFeatureProfile {
            profile_id: "same".into(),
            platform_family: "acme".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
            device_roles: vec![],
            resource_budget: ResourceBudget::default(),
            features: BTreeMap::new(),
            verification: "untested".into(),
        };
        r2.register(p.clone());
        r2.register(p);
        assert!(r2.validate().iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn feature_composition_rejects_undeclared_prereq_and_cycles() {
        let mut p = isis_sr_profile();
        // Undeclared prerequisite.
        p.features.get_mut("isis.sr_mpls").unwrap().requires = vec!["isis.nonexistent".into()];
        let errs =
            ProfileRegistry::validate_feature_set(&p, &["isis".into(), "isis.sr_mpls".into()])
                .expect_err("undeclared prerequisite");
        assert!(errs.iter().any(|e| e.contains("undeclared")));
        // Disabled prerequisite.
        let mut p = isis_sr_profile();
        p.features.get_mut("isis").unwrap().enabled = false;
        let errs =
            ProfileRegistry::validate_feature_set(&p, &["isis".into(), "isis.sr_mpls".into()])
                .expect_err("disabled prerequisite");
        assert!(errs.iter().any(|e| e.contains("not applicable")));
        // Cycle: a requires b, b requires a.
        let mut p = isis_sr_profile();
        p.features.get_mut("isis").unwrap().requires = vec!["isis.sr_mpls".into()];
        let errs =
            ProfileRegistry::validate_feature_set(&p, &["isis".into(), "isis.sr_mpls".into()])
                .expect_err("cycle");
        assert!(errs.iter().any(|e| e.contains("circular")));
    }

    #[test]
    fn sonic_namespace_fixture_declares_distinct_scopes() {
        use std::path::Path;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bn024/sonic-namespaces.fixture.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let nss = v["namespaces"].as_array().unwrap();
        assert!(nss.len() >= 2, "fixture must model multi-namespace device");
        let names: std::collections::BTreeSet<&str> = nss
            .iter()
            .map(|n| n["namespace"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), nss.len(), "namespaces must be distinct");
        assert!(names.contains("asic-0") && names.contains("asic-1"));
        // Profile fixture expectation must match the registered profile.
        let r = registry();
        let sel = r.select(&ProfileQuery {
            platform: v["platform"].as_str().unwrap().into(),
            version: v["version"].as_str().unwrap().into(),
            release: None,
            hardware: None,
        });
        let Selection::Selected(p) = sel else {
            panic!("sonic fixture must select")
        };
        for ns in p.verified_namespaces.iter() {
            assert!(names.contains(ns.as_str()), "verified ns {ns} in fixture");
        }
    }

    #[test]
    fn profiles_roundtrip_through_json() {
        let p = sonic_profile();
        let bytes = serde_json::to_vec(&p).unwrap();
        let back: NosFeatureProfile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.profile_id, p.profile_id);
        assert_eq!(back.verified_namespaces, p.verified_namespaces);
    }

    // ── BN-024-03A: release/hardware scope and composition ──────────────

    fn query(platform: &str, version: &str) -> ProfileQuery {
        ProfileQuery {
            platform: platform.into(),
            version: version.into(),
            release: None,
            hardware: None,
        }
    }

    #[test]
    fn cumulus_release_and_asic_constraints_gate_selection() {
        let r = registry();
        // Matching release (raw + frr component) and marvell ASIC select.
        let sel = r.select(&ProfileQuery {
            release: Some(SoftwareRelease {
                raw: "5.13.0".into(),
                train: None,
                build: None,
                components: BTreeMap::from([("frr".into(), "10.6.1".into())]),
            }),
            hardware: Some(HardwareScope {
                asic: Some("marvell".into()),
                ..Default::default()
            }),
            ..query("cumulus", "5.13.0")
        });
        let Selection::Selected(p) = sel else {
            panic!("cumulus fixture must select, got {sel:?}")
        };
        assert_eq!(p.profile_id, "cumulus-1");
        // NVUE applied/startup planes are modeled as distinct scope notes.
        let ifaces = &p.features["interfaces"];
        assert!(
            ifaces
                .scope_notes
                .iter()
                .any(|n| n.contains("applied vs startup"))
        );

        // Wrong FRR component → out of scope, not silently selected.
        let sel = r.select(&ProfileQuery {
            release: Some(SoftwareRelease {
                raw: "5.13.0".into(),
                train: None,
                build: None,
                components: BTreeMap::from([("frr".into(), "9.1".into())]),
            }),
            hardware: Some(HardwareScope {
                asic: Some("marvell".into()),
                ..Default::default()
            }),
            ..query("cumulus", "5.13.0")
        });
        assert!(matches!(sel, Selection::OutOfScope { .. }), "got {sel:?}");

        // Unknown release (None) cannot satisfy the constraint.
        let sel = r.select(&query("cumulus", "*"));
        assert!(matches!(sel, Selection::OutOfScope { .. }), "got {sel:?}");

        // Wrong ASIC → out of scope.
        let sel = r.select(&ProfileQuery {
            release: Some(SoftwareRelease {
                raw: "5.13.0".into(),
                train: None,
                build: None,
                components: BTreeMap::from([("frr".into(), "10.6.1".into())]),
            }),
            hardware: Some(HardwareScope {
                asic: Some("broadcom".into()),
                ..Default::default()
            }),
            ..query("cumulus", "5.13.0")
        });
        assert!(matches!(sel, Selection::OutOfScope { .. }), "got {sel:?}");
    }

    #[test]
    fn cumulus_metadata_fixture_matches_profile_contract() {
        use std::path::Path;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bn024/cumulus-metadata.fixture.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["platform"].as_str().unwrap(), "cumulus");
        // The fixture must carry the architectural planes the contract
        // models: release, NVUE, FRR, kernel and applied/startup config.
        for key in ["release", "nvue", "frr", "kernel", "asic"] {
            assert!(v[key].is_string() || v[key].is_object(), "missing {key}");
        }
        assert!(v["config_planes"]["applied"].is_object());
        assert!(v["config_planes"]["startup"].is_object());
        // And the registered profile must accept this device's scope.
        let r = registry();
        let sel = r.select(&ProfileQuery {
            platform: "cumulus".into(),
            version: v["release"]["raw"].as_str().unwrap().into(),
            release: Some(SoftwareRelease {
                raw: v["release"]["raw"].as_str().unwrap().into(),
                train: None,
                build: None,
                components: BTreeMap::from([(
                    "frr".into(),
                    v["frr"]["version"].as_str().unwrap().into(),
                )]),
            }),
            hardware: Some(HardwareScope {
                asic: Some(v["asic"].as_str().unwrap().into()),
                ..Default::default()
            }),
        });
        assert!(matches!(sel, Selection::Selected(_)), "got {sel:?}");
    }

    #[test]
    fn feature_composition_validates_requires_and_conflicts() {
        let r = registry();
        let Selection::Selected(p) = r.select(&query("example.srnos", "1.0")) else {
            panic!("srnos profile must select")
        };
        // IS-IS base alone is valid.
        assert!(ProfileRegistry::validate_feature_set(&p, &["isis".into()]).is_ok());
        // IS-IS + SR-MPLS + Flex-Algo + explicit SR policy + PCE-delegated
        // policy is a valid composition.
        assert!(
            ProfileRegistry::validate_feature_set(
                &p,
                &[
                    "isis".into(),
                    "isis.sr_mpls".into(),
                    "isis.flex_algo".into(),
                    "sr.policy.explicit".into(),
                    "sr.policy.pce".into(),
                ]
            )
            .is_ok()
        );
        // Subfeature without base fails on the missing prerequisite.
        let errs = ProfileRegistry::validate_feature_set(&p, &["isis.sr_mpls".into()])
            .expect_err("missing isis prerequisite");
        assert!(errs.iter().any(|e| e.contains("requires 'isis'")));
        // PCE policy without explicit segment-list policy fails.
        assert!(
            ProfileRegistry::validate_feature_set(
                &p,
                &["isis".into(), "isis.sr_mpls".into(), "sr.policy.pce".into()]
            )
            .is_err()
        );
        // Undeclared feature is explicit, not implicitly enabled.
        assert!(ProfileRegistry::validate_feature_set(&p, &["isis.bfd".into()]).is_err());
        // Conflicting pair reports both directions once.
        let errs = ProfileRegistry::validate_feature_set(
            &p,
            &["isis".into(), "isis.sr_mpls".into(), "isis.sr_srv6".into()],
        )
        .expect_err("srv6 conflicts with sr_mpls on this profile");
        assert!(errs.iter().any(|e| e.contains("conflict")));
    }

    #[test]
    fn platform_query_is_case_normalized() {
        let r = registry();
        match r.select(&query("SONIC", "2025.1")) {
            Selection::Selected(p) => assert_eq!(p.profile_id, "sonic-1"),
            other => panic!("expected sonic selection, got {other:?}"),
        }
    }
}

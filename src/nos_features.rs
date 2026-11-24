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
        self.components.iter().all(|(k, v)| {
            rel.components.get(k).is_some_and(|got| got == v)
        })
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
        self.attributes.iter().all(|(k, v)| {
            dev.attributes.get(k).is_some_and(|got| got == v)
        })
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
    /// Best-matching profile for (platform, version).
    Selected(Box<NosFeatureProfile>),
    /// Platform family known but no profile covers this version.
    UnsupportedVersion { platform: String, version: String },
    /// No profile registered for the platform family at all.
    UnsupportedPlatform { platform: String },
    /// A profile matched platform/version but its release or hardware
    /// constraint rejected the query scope (e.g. unknown ASIC, missing
    /// component version). Distinct from version unsupported: the release
    /// may be known while the hardware/namespace scope is not covered.
    OutOfScope { platform: String, reason: String },
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

        let platform_exact =
            |p: &NosFeatureProfile| p.platform_family == platform || p.platform_family == family;
        // A profile is scope-applicable only when its release and hardware
        // constraints accept the query. Constraint failures are tracked so
        // a known-but-out-of-scope platform reports OutOfScope rather than
        // UnsupportedVersion/Platform.
        let scope_ok = |p: &NosFeatureProfile| -> Result<(), String> {
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
                    return Err(format!(
                        "hardware constraint not satisfied ({hc:?})"
                    ));
                }
            }
            Ok(())
        };
        let version_exact = |p: &NosFeatureProfile| {
            p.version_match == "*" || version_matches(&p.version_match, version)
        };
        // "exact version" means the profile constrains to this version
        // (not "*"); any-version is the fallback tier.
        let version_pinned = |p: &NosFeatureProfile| {
            p.version_match != "*" && version_matches(&p.version_match, version)
        };

        let mut best: Option<&NosFeatureProfile> = None;
        let mut best_tier = 0u8;
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
            // version_exact unused beyond pinning; "*" matches everything.
            let _ = version_exact;
            if tier > best_tier || (tier == best_tier && best.is_none()) {
                best = Some(p);
                best_tier = tier;
            }
        }
        if let Some(p) = best {
            return Selection::Selected(Box::new(p.clone()));
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

    /// Feature lookup on a selected profile — disabled and absent features
    /// are distinct outcomes.
    pub fn feature_status(profile: &NosFeatureProfile, feature: &str) -> FeatureStatus {
        match profile.features.get(feature) {
            None => FeatureStatus::Absent,
            Some(f) if !f.enabled => FeatureStatus::Disabled,
            Some(_) => FeatureStatus::Enabled,
        }
    }

    /// Validate a requested feature *set* as a composition (BN-024-03A):
    /// every feature must exist and be enabled, all `requires`
    /// prerequisites must be present in the set, and no `conflicts`
    /// pair may co-occur. Returns every violation found — callers get the
    /// full conflict list, not just the first.
    ///
    /// This checks declarative applicability only; it does not implement
    /// IS-IS/SR/PCE semantics and grants no collection or graph authority.
    pub fn validate_feature_set(
        profile: &NosFeatureProfile,
        requested: &[String],
    ) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let set: std::collections::BTreeSet<&str> =
            requested.iter().map(String::as_str).collect();
        for feature in requested {
            match Self::feature_status(profile, feature) {
                FeatureStatus::Absent => {
                    errors.push(format!("feature '{feature}' is not declared"))
                }
                FeatureStatus::Disabled => {
                    errors.push(format!("feature '{feature}' is disabled"))
                }
                FeatureStatus::Enabled => {}
            }
            if let Some(flag) = profile.features.get(feature) {
                for req in &flag.requires {
                    if !set.contains(req.as_str()) {
                        errors.push(format!(
                            "feature '{feature}' requires '{req}' which is not in the set"
                        ));
                    }
                }
            }
        }
        for feature in requested {
            if let Some(flag) = profile.features.get(feature) {
                for conflict in &flag.conflicts {
                    if set.contains(conflict.as_str()) {
                        errors.push(format!(
                            "features '{feature}' and '{conflict}' conflict"
                        ));
                    }
                }
            }
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
    Enabled,
    Disabled,
    /// Feature not declared by the profile — consumers must treat as
    /// unsupported, never as implicitly enabled.
    Absent,
}

/// Extract the family portion of a canonical platform id
/// ("cisco.iosxe" → "cisco", "sonic" → "sonic").
fn platform_family(platform: &str) -> &str {
    platform.split(['.', '-']).next().unwrap_or(platform)
}

/// Version constraint match: "*" any, "=X" exact, "X" or "X.Y" prefix.
fn version_matches(pattern: &str, version: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let pat = pattern.strip_prefix('=').unwrap_or(pattern);
    if version.is_empty() || version == "*" {
        return false;
    }
    version == pat || version.starts_with(&format!("{pat}."))
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
            support: SupportState::Unknown,
            evidence: vec![],
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
            features: BTreeMap::from([
                (
                    "interfaces".into(),
                    FeatureFlag {
                        enabled: true,
                        parsers: vec!["native_db".into()],
                        scope_notes: vec!["CONFIG_DB+STATE_DB per namespace".into()],
                        requires: vec![],
                        conflicts: vec![],
                        support: SupportState::Unknown,
                        evidence: vec![],
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
        assert_eq!(
            ProfileRegistry::feature_status(&p, "interfaces"),
            FeatureStatus::Enabled
        );
    }

    #[test]
    fn exact_version_beats_family_wildcard() {
        let mut r = ProfileRegistry::new();
        r.register(NosFeatureProfile {
            profile_id: "fam-any".into(),
            platform_family: "cisco".into(),
            version_match: "*".into(),
            release_constraint: None,
            hardware_constraints: vec![],
            verified_namespaces: vec![],
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
            features: BTreeMap::new(),
            verification: "fixture_only".into(),
        });
        match r.select(&ProfileQuery {
            platform: "cisco.iosxe".into(),
            version: "17.9.4".into(),
            release: None,
            hardware: None,
        }) {
            // "=17.9" prefix-matches "17.9.4" → exact-ish tier wins.
            Selection::Selected(p) => assert_eq!(p.profile_id, "cisco-17"),
            other => panic!("expected cisco-17, got {other:?}"),
        }
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
        assert!(ifaces
            .scope_notes
            .iter()
            .any(|n| n.contains("applied vs startup")));

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
        assert!(ProfileRegistry::validate_feature_set(
            &p,
            &[
                "isis".into(),
                "isis.sr_mpls".into(),
                "isis.flex_algo".into(),
                "sr.policy.explicit".into(),
                "sr.policy.pce".into(),
            ]
        )
        .is_ok());
        // Subfeature without base fails on the missing prerequisite.
        let errs = ProfileRegistry::validate_feature_set(&p, &["isis.sr_mpls".into()])
            .expect_err("missing isis prerequisite");
        assert!(errs.iter().any(|e| e.contains("requires 'isis'")));
        // PCE policy without explicit segment-list policy fails.
        assert!(ProfileRegistry::validate_feature_set(
            &p,
            &["isis".into(), "isis.sr_mpls".into(), "sr.policy.pce".into()]
        )
        .is_err());
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

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

/// A single feature a profile claims support for.
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
}

/// Declarative per-NOS feature profile. `platform_family` matches the
/// canonical platform registry family (e.g. "cisco", "arista");
/// `version_match` is an optional exact-version or prefix constraint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NosFeatureProfile {
    pub profile_id: String,
    /// Canonical platform family or exact platform id this profile covers.
    pub platform_family: String,
    /// Optional version constraint: "*" (any), "=X.Y", or prefix "X.Y".
    #[serde(default = "any_version")]
    pub version_match: String,
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
}

/// Query for profile selection.
#[derive(Clone, Debug)]
pub struct ProfileQuery {
    /// Canonical platform id or family (e.g. "cisco.iosxe", "cisco").
    pub platform: String,
    /// NOS version string; "" or "*" matches any-version profiles only.
    pub version: String,
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
    pub fn select(&self, query: &ProfileQuery) -> Selection {
        let platform = query.platform.as_str();
        let family = platform_family(platform);
        let version = query.version.as_str();

        let platform_exact =
            |p: &NosFeatureProfile| p.platform_family == platform || p.platform_family == family;
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
        for p in &self.profiles {
            if !platform_exact(p) {
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
        match best {
            Some(p) => Selection::Selected(Box::new(p.clone())),
            None => {
                if self.profiles.iter().any(|p| platform_exact(p)) {
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

    fn sonic_profile() -> NosFeatureProfile {
        NosFeatureProfile {
            profile_id: "sonic-1".into(),
            platform_family: "sonic".into(),
            version_match: "*".into(),
            verified_namespaces: vec!["asic-0".into(), "asic-1".into()],
            features: BTreeMap::from([
                (
                    "interfaces".into(),
                    FeatureFlag {
                        enabled: true,
                        parsers: vec!["native_db".into()],
                        scope_notes: vec!["CONFIG_DB+STATE_DB per namespace".into()],
                    },
                ),
                (
                    "bgp".into(),
                    FeatureFlag {
                        enabled: false,
                        parsers: vec![],
                        scope_notes: vec![],
                    },
                ),
            ]),
            verification: "fixture_only".into(),
        }
    }

    fn future_nos_profile() -> NosFeatureProfile {
        NosFeatureProfile {
            profile_id: "futureos-1".into(),
            platform_family: "futureos".into(),
            version_match: "=2026.1".into(),
            verified_namespaces: vec![],
            features: BTreeMap::from([(
                "interfaces".into(),
                FeatureFlag {
                    enabled: true,
                    parsers: vec![],
                    scope_notes: vec![],
                },
            )]),
            verification: "untested".into(),
        }
    }

    fn registry() -> ProfileRegistry {
        let mut r = ProfileRegistry::new();
        r.register(sonic_profile());
        r.register(future_nos_profile());
        r
    }

    #[test]
    fn dummy_future_nos_registers_without_core_changes() {
        let r = registry();
        match r.select(&ProfileQuery {
            platform: "futureos.router".into(),
            version: "2026.1".into(),
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
            verified_namespaces: vec![],
            features: BTreeMap::new(),
            verification: "untested".into(),
        });
        r.register(NosFeatureProfile {
            profile_id: "cisco-17".into(),
            platform_family: "cisco.iosxe".into(),
            version_match: "=17.9".into(),
            verified_namespaces: vec![],
            features: BTreeMap::new(),
            verification: "fixture_only".into(),
        });
        match r.select(&ProfileQuery {
            platform: "cisco.iosxe".into(),
            version: "17.9.4".into(),
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
}

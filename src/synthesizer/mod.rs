use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::catalogue::{
    CataloguePath, CatalogueProfile, canonical_role, is_sp_role, load_catalogue,
};
use crate::config::{SelectedSubscriptionPath, TargetConfig};
use crate::discovery::{DiscoveryReport, GnmiReadinessReport, PathProfileMatch, SubscriptionPath};
use crate::registry::{OverrideAction, OverrideScope, PathOverride};
use crate::yang::{YangLibraryState, evaluate_profile_requirements};

const PATH_PROFILE_DIR: &str = "config/path_profiles";
const SYNTHESIZER_RULE_DIR: &str = "config/synthesizer_rules";

#[derive(Clone, Debug, Deserialize)]
pub struct SynthesizerRule {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub role_hints: Vec<String>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub vendor_scope: Vec<String>,
    #[serde(default)]
    pub profile_names: Vec<String>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub gaps: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SynthesizedProfile {
    pub profile_name: String,
    pub rule_name: String,
    pub path_count: usize,
    pub confidence: f32,
    pub rationale: String,
    pub available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SynthesizedPath {
    pub profile_name: String,
    pub path: String,
    pub origin: String,
    pub mode: String,
    pub sample_interval_ns: u64,
    pub rationale: String,
    pub optional: bool,
    pub confidence: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct SynthesizerReport {
    pub status: String,
    pub role: String,
    pub environment: String,
    pub vendor: String,
    pub matched_rules: Vec<String>,
    pub recommended_profiles: Vec<SynthesizedProfile>,
    pub recommended_paths: Vec<SynthesizedPath>,
    pub blockers: Vec<String>,
    pub gaps: Vec<String>,
    pub warnings: Vec<String>,
    pub override_audit: Vec<String>,
}

pub fn synthesize_for_target(
    target: &TargetConfig,
    discovery: Option<&DiscoveryReport>,
    readiness: Option<&GnmiReadinessReport>,
    warnings: Vec<String>,
    overrides: &[PathOverride],
    yang_library: Option<&YangLibraryState>,
) -> SynthesizerReport {
    let role = canonical_role(target.role.as_deref());
    let environment = infer_environment(&role);
    let vendor = discovery
        .map(|report| report.vendor_detected.clone())
        .or_else(|| target.vendor.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let rules = load_rules(Path::new(SYNTHESIZER_RULE_DIR));
    let catalogue_profiles = load_catalogue(Path::new(PATH_PROFILE_DIR))
        .all_profiles()
        .cloned()
        .map(|profile| (profile.name.clone(), profile))
        .collect::<BTreeMap<_, _>>();
    let matched_rules = match_rules(&rules, &role, &environment, &vendor);

    let mut report = SynthesizerReport {
        status: "empty".to_string(),
        role,
        environment,
        vendor,
        matched_rules: matched_rules.iter().map(|rule| rule.name.clone()).collect(),
        recommended_profiles: Vec::new(),
        recommended_paths: Vec::new(),
        blockers: readiness.map(|r| r.blockers.clone()).unwrap_or_default(),
        gaps: Vec::new(),
        warnings,
        override_audit: Vec::new(),
    };

    if let Some(readiness) = readiness {
        report
            .blockers
            .extend(readiness.known_issues.iter().cloned());
    }
    if let Some(discovery) = discovery {
        report.warnings.extend(discovery.warnings.iter().cloned());
    }

    let live_profiles = discovery
        .map(|report| {
            report
                .recommended_profiles
                .iter()
                .map(|profile| (profile.profile_name.clone(), profile.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut requested_profiles = BTreeSet::new();
    for rule in &matched_rules {
        report.gaps.extend(rule.gaps.iter().cloned());
        for profile_name in &rule.profile_names {
            requested_profiles.insert(profile_name.clone());
        }
    }

    if requested_profiles.is_empty()
        && let Some(discovery) = discovery
    {
        for profile in &discovery.recommended_profiles {
            requested_profiles.insert(profile.profile_name.clone());
        }
        if !discovery.recommended_profiles.is_empty() {
            report.warnings.push(
                "No synthesizer rule matched exactly; falling back to live discovery recommendations."
                    .to_string(),
            );
        }
    }

    for profile_name in requested_profiles {
        if let Some(profile_match) = live_profiles.get(&profile_name) {
            let rule_name = matched_rules
                .iter()
                .find(|rule| rule.profile_names.iter().any(|name| name == &profile_name))
                .map(|rule| rule.name.clone())
                .unwrap_or_else(|| "live-discovery".to_string());
            let rationale = matched_rules
                .iter()
                .find(|rule| rule.profile_names.iter().any(|name| name == &profile_name))
                .and_then(|rule| {
                    if rule.rationale.trim().is_empty() {
                        None
                    } else {
                        Some(format!(
                            "{} {}",
                            rule.rationale.trim(),
                            profile_match.rationale.trim()
                        ))
                    }
                })
                .unwrap_or_else(|| profile_match.rationale.clone());
            push_profile_match(
                &mut report,
                &rule_name,
                profile_match,
                profile_match.confidence,
                rationale,
            );
            continue;
        }

        if discovery.is_some() {
            report.gaps.push(format!(
                "Profile '{profile_name}' matched the device role, but live capabilities did not support it."
            ));
            continue;
        }

        if let Some(profile) = catalogue_profiles.get(&profile_name) {
            let rule_name = matched_rules
                .iter()
                .find(|rule| rule.profile_names.iter().any(|name| name == &profile_name))
                .map(|rule| rule.name.clone())
                .unwrap_or_else(|| "catalogue-fallback".to_string());
            let profile_match = catalogue_profile_to_match(profile);
            push_profile_match(
                &mut report,
                &rule_name,
                &profile_match,
                0.55,
                format!(
                    "Catalogue-only recommendation while live capabilities are unavailable. {}",
                    profile_match.rationale
                ),
            );
        }
    }

    if report.recommended_profiles.is_empty() && !target.selected_paths.is_empty() {
        report.status = "selected-paths-only".to_string();
        report.warnings.push(
            "Falling back to the device's currently selected paths because no synthesizer recommendation could be built."
                .to_string(),
        );
        for selected in &target.selected_paths {
            report.recommended_paths.push(SynthesizedPath {
                profile_name: "current-selection".to_string(),
                path: selected.path.clone(),
                origin: selected.origin.clone(),
                mode: selected.mode.clone(),
                sample_interval_ns: selected.sample_interval_ns,
                rationale: selected.rationale.clone(),
                optional: selected.optional,
                confidence: 0.3,
            });
        }
    } else if !report.recommended_profiles.is_empty() {
        report.status = if discovery.is_some() {
            "live".to_string()
        } else {
            "catalogue-only".to_string()
        };
    } else {
        report.warnings.push(
            "No recommendations could be produced from the current role, environment, and catalogue state."
                .to_string(),
        );
    }

    apply_overrides(&mut report, target, overrides);
    apply_yang_awareness(&mut report, &catalogue_profiles, yang_library);
    dedupe_report(&mut report);
    report
}

fn push_profile_match(
    report: &mut SynthesizerReport,
    rule_name: &str,
    profile_match: &PathProfileMatch,
    confidence: f32,
    rationale: String,
) {
    report.recommended_profiles.push(SynthesizedProfile {
        profile_name: profile_match.profile_name.clone(),
        rule_name: rule_name.to_string(),
        path_count: profile_match.paths.len(),
        confidence,
        rationale,
        available: true,
    });
    for path in &profile_match.paths {
        report.recommended_paths.push(SynthesizedPath {
            profile_name: profile_match.profile_name.clone(),
            path: path.path.clone(),
            origin: path.origin.clone(),
            mode: path.mode.clone(),
            sample_interval_ns: path.sample_interval_ns,
            rationale: path.rationale.clone(),
            optional: path.optional,
            confidence,
        });
    }
}

fn dedupe_report(report: &mut SynthesizerReport) {
    report.blockers.sort();
    report.blockers.dedup();
    report.gaps.sort();
    report.gaps.dedup();
    report.warnings.sort();
    report.warnings.dedup();
    report.override_audit.sort();
    report.override_audit.dedup();

    let mut seen_profiles = BTreeSet::new();
    report.recommended_profiles.retain(|profile| {
        seen_profiles.insert((profile.profile_name.clone(), profile.rule_name.clone()))
    });

    let mut seen_paths = BTreeSet::new();
    report.recommended_paths.retain(|path| {
        seen_paths.insert((
            path.profile_name.clone(),
            path.path.clone(),
            path.mode.clone(),
        ))
    });
}

fn apply_yang_awareness(
    report: &mut SynthesizerReport,
    catalogue_profiles: &BTreeMap<String, CatalogueProfile>,
    yang_library: Option<&YangLibraryState>,
) {
    let Some(yang_library) = yang_library else {
        report.warnings.push(
            "Local YANG library is unavailable; synthesizer could not verify installed module coverage."
                .to_string(),
        );
        return;
    };

    let mut per_profile_missing = BTreeMap::<String, BTreeSet<String>>::new();
    for profile in &report.recommended_profiles {
        let Some(catalogue_profile) = catalogue_profiles.get(&profile.profile_name) else {
            continue;
        };
        for path in &catalogue_profile.paths {
            if let Some(reason) = evaluate_profile_requirements(
                yang_library,
                &path.required_models,
                &path.required_any_models,
            ) {
                per_profile_missing
                    .entry(profile.profile_name.clone())
                    .or_default()
                    .insert(reason);
            }
        }
    }

    for (profile_name, reasons) in per_profile_missing {
        for reason in reasons {
            report.gaps.push(format!(
                "Profile '{}' has incomplete local YANG coverage: {}",
                profile_name, reason
            ));
        }
    }
}

fn apply_overrides(
    report: &mut SynthesizerReport,
    target: &TargetConfig,
    overrides: &[PathOverride],
) {
    let role = report.role.clone();
    let environment = report.environment.clone();
    let site = target.site.clone().unwrap_or_default();
    let address = target.address.clone();

    let mut matched = overrides
        .iter()
        .filter(|ovr| override_matches(&ovr.scope, &role, &environment, &site, &address))
        .cloned()
        .collect::<Vec<_>>();
    matched.sort_by_key(|ovr| override_precedence(&ovr.scope));

    for ovr in matched {
        let scope_name = match &ovr.scope {
            OverrideScope::RoleEnv { role, environment } => {
                format!("role-env({role}/{environment})")
            }
            OverrideScope::Site(site) => format!("site({site})"),
            OverrideScope::Device(device) => format!("device({device})"),
        };
        match ovr.action {
            OverrideAction::Add => {
                let key = format!("{}::SAMPLE", ovr.path);
                let exists = report
                    .recommended_paths
                    .iter()
                    .any(|path| format!("{}::{}", path.path, path.mode) == key);
                if !exists {
                    report.recommended_paths.push(SynthesizedPath {
                        profile_name: "override".to_string(),
                        path: ovr.path.clone(),
                        origin: format!("override:{scope_name}"),
                        mode: "SAMPLE".to_string(),
                        sample_interval_ns: ovr.sample_interval_s.unwrap_or(10) * 1_000_000_000,
                        rationale: "Added by operator override.".to_string(),
                        optional: ovr.optional.unwrap_or(false),
                        confidence: 1.0,
                    });
                    report.override_audit.push(format!(
                        "{scope_name} added path '{}' to the recommended set",
                        ovr.path
                    ));
                }
            }
            OverrideAction::Drop => {
                let before = report.recommended_paths.len();
                report
                    .recommended_paths
                    .retain(|path| path.path != ovr.path);
                if report.recommended_paths.len() != before {
                    report.override_audit.push(format!(
                        "{scope_name} removed path '{}' from the recommended set",
                        ovr.path
                    ));
                }
            }
            OverrideAction::Modify => {
                let mut changed = false;
                for path in &mut report.recommended_paths {
                    if path.path == ovr.path {
                        if let Some(sample_interval_s) = ovr.sample_interval_s {
                            path.sample_interval_ns = sample_interval_s * 1_000_000_000;
                        }
                        if let Some(optional) = ovr.optional {
                            path.optional = optional;
                        }
                        path.origin = format!("override:{scope_name}");
                        changed = true;
                    }
                }
                if changed {
                    report.override_audit.push(format!(
                        "{scope_name} modified path '{}' in the recommended set",
                        ovr.path
                    ));
                }
            }
        }
    }

    if !report.recommended_paths.is_empty() {
        report.recommended_profiles =
            recompute_profiles(&report.recommended_paths, &report.recommended_profiles);
    }
}

fn override_matches(
    scope: &OverrideScope,
    role: &str,
    environment: &str,
    site: &str,
    address: &str,
) -> bool {
    match scope {
        OverrideScope::RoleEnv {
            role: r,
            environment: e,
        } => r.eq_ignore_ascii_case(role) && e.eq_ignore_ascii_case(environment),
        OverrideScope::Site(s) => !site.is_empty() && s == site,
        OverrideScope::Device(d) => d == address,
    }
}

fn override_precedence(scope: &OverrideScope) -> u8 {
    match scope {
        OverrideScope::RoleEnv { .. } => 1,
        OverrideScope::Site(_) => 2,
        OverrideScope::Device(_) => 3,
    }
}

fn recompute_profiles(
    paths: &[SynthesizedPath],
    existing_profiles: &[SynthesizedProfile],
) -> Vec<SynthesizedProfile> {
    let mut by_profile = BTreeMap::<String, usize>::new();
    for path in paths {
        *by_profile.entry(path.profile_name.clone()).or_default() += 1;
    }
    existing_profiles
        .iter()
        .filter_map(|profile| {
            by_profile
                .get(&profile.profile_name)
                .copied()
                .map(|path_count| SynthesizedProfile {
                    profile_name: profile.profile_name.clone(),
                    rule_name: profile.rule_name.clone(),
                    path_count,
                    confidence: profile.confidence,
                    rationale: profile.rationale.clone(),
                    available: profile.available,
                })
        })
        .collect()
}

fn infer_environment(role: &str) -> String {
    if matches!(role, "access" | "distribution" | "core" | "edge" | "wlc") {
        "campus_wired".to_string()
    } else if is_sp_role(role) || role == "ce" {
        "service_provider".to_string()
    } else {
        "data_center".to_string()
    }
}

fn load_rules(base_dir: &Path) -> Vec<SynthesizerRule> {
    let mut rules = Vec::new();
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return rules;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let is_yaml = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"));
        if !is_yaml {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(rule) = serde_yaml::from_str::<SynthesizerRule>(&raw) {
            rules.push(rule);
        }
    }
    rules.sort_by(|a, b| a.name.cmp(&b.name));
    rules
}

fn match_rules<'a>(
    rules: &'a [SynthesizerRule],
    role: &str,
    environment: &str,
    vendor: &str,
) -> Vec<&'a SynthesizerRule> {
    rules
        .iter()
        .filter(|rule| {
            matches_value(&rule.role_hints, role)
                && matches_value(&rule.environment, environment)
                && matches_value(&rule.vendor_scope, vendor)
        })
        .collect()
}

fn matches_value(values: &[String], actual: &str) -> bool {
    values.is_empty()
        || values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(actual) || value.eq_ignore_ascii_case("any"))
}

fn catalogue_profile_to_match(profile: &CatalogueProfile) -> PathProfileMatch {
    PathProfileMatch {
        profile_name: profile.name.clone(),
        paths: profile
            .paths
            .iter()
            .map(catalogue_path_to_subscription)
            .collect(),
        rationale: if profile.description.is_empty() {
            profile.rationale.clone()
        } else if profile.rationale.is_empty() {
            profile.description.clone()
        } else {
            format!("{} {}", profile.description, profile.rationale)
        },
        confidence: 0.55,
    }
}

fn catalogue_path_to_subscription(path: &CataloguePath) -> SubscriptionPath {
    SubscriptionPath {
        path: path.path.clone(),
        origin: path.origin.clone(),
        mode: path.mode.clone(),
        sample_interval_ns: path.sample_interval_ns,
        rationale: path.rationale.clone(),
        optional: path.optional,
    }
}

pub fn selected_paths_from_report(report: &SynthesizerReport) -> Vec<SelectedSubscriptionPath> {
    report
        .recommended_paths
        .iter()
        .map(|path| SelectedSubscriptionPath {
            path: path.path.clone(),
            origin: path.origin.clone(),
            mode: path.mode.clone(),
            sample_interval_ns: path.sample_interval_ns,
            rationale: path.rationale.clone(),
            optional: path.optional,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{OverrideAction, OverrideScope, PathOverride};
    use crate::yang::{YangLibraryState, YangModuleRecord};

    fn target(role: &str, vendor: &str) -> TargetConfig {
        TargetConfig {
            address: "10.0.0.1:57400".to_string(),
            enabled: true,
            tls_domain: None,
            ca_cert: None,
            vendor: Some(vendor.to_string()),
            credential_alias: None,
            username_env: None,
            password_env: None,
            username: None,
            password: None,
            hostname: Some("leaf1".to_string()),
            role: Some(role.to_string()),
            site: Some("lab".to_string()),
            collector_id: None,
            selected_paths: Vec::new(),
            created_at_ns: 0,
            updated_at_ns: 0,
            created_by: String::new(),
            updated_by: String::new(),
            last_operator_action: String::new(),
        }
    }

    #[test]
    fn synthesizer_falls_back_to_catalogue_when_discovery_missing() {
        let report = synthesize_for_target(
            &target("leaf", "nokia_srl"),
            None,
            None,
            Vec::new(),
            &[],
            None,
        );
        assert_eq!(report.status, "catalogue-only");
        assert!(
            report
                .recommended_profiles
                .iter()
                .any(|profile| profile.profile_name == "dc_evpn_leaf")
        );
        assert!(!report.recommended_paths.is_empty());
    }

    #[test]
    fn synthesizer_prefers_live_profile_matches() {
        let discovery = DiscoveryReport {
            vendor_detected: "nokia_srl".to_string(),
            models_advertised: vec!["openconfig-interfaces".to_string()],
            gnmi_encoding: "JSON_IETF".to_string(),
            recommended_profiles: vec![PathProfileMatch {
                profile_name: "dc_spine_standard".to_string(),
                paths: vec![SubscriptionPath {
                    path: "interfaces".to_string(),
                    origin: "openconfig".to_string(),
                    mode: "SAMPLE".to_string(),
                    sample_interval_ns: 10,
                    rationale: "OpenConfig interface counters.".to_string(),
                    optional: false,
                }],
                rationale: "Live discovery picked the spine profile.".to_string(),
                confidence: 0.9,
            }],
            warnings: Vec::new(),
        };
        let report = synthesize_for_target(
            &target("spine", "nokia_srl"),
            Some(&discovery),
            None,
            Vec::new(),
            &[],
            None,
        );
        assert_eq!(report.status, "live");
        assert_eq!(report.recommended_profiles.len(), 1);
        assert_eq!(
            report.recommended_profiles[0].profile_name,
            "dc_spine_standard"
        );
        assert_eq!(report.recommended_paths[0].confidence, 0.9);
    }

    #[test]
    fn synthesizer_applies_device_override_precedence() {
        let report = synthesize_for_target(
            &target("leaf", "nokia_srl"),
            None,
            None,
            Vec::new(),
            &[PathOverride {
                scope: OverrideScope::Device("10.0.0.1:57400".to_string()),
                path: "interfaces".to_string(),
                action: OverrideAction::Drop,
                sample_interval_s: None,
                optional: None,
                created_at_ns: 0,
                created_by: String::new(),
            }],
            None,
        );
        assert!(
            report
                .recommended_paths
                .iter()
                .all(|path| path.path != "interfaces"),
            "device override should remove matching recommended paths"
        );
        assert!(
            report
                .override_audit
                .iter()
                .any(|line| line.contains("removed path")),
            "override audit should capture the change"
        );
    }

    #[test]
    fn synthesizer_surfaces_missing_local_yang_coverage() {
        let yang_state = YangLibraryState {
            modules: vec![YangModuleRecord {
                module_name: "openconfig-interfaces".to_string(),
                revision: "2024-01-01".to_string(),
                namespace: String::new(),
                organization: String::new(),
                source_kind: "manual".to_string(),
                source_ref: "test".to_string(),
                vendor_scope: "openconfig".to_string(),
                trust: "trusted".to_string(),
                relative_path: "modules/openconfig-interfaces@2024-01-01.yang".to_string(),
                checksum_sha256: "abc".to_string(),
                imported_at_ns: 1,
            }],
            path_index: Vec::new(),
        };
        let report = synthesize_for_target(
            &target("leaf", "nokia_srl"),
            None,
            None,
            Vec::new(),
            &[],
            Some(&yang_state),
        );
        assert!(
            report
                .gaps
                .iter()
                .any(|gap| gap.contains("local YANG coverage"))
        );
    }
}

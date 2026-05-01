use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

fn semver_gt(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(va), Ok(vb)) => va > vb,
        _ => {
            warn!(a, b, "non-semver plugin versions; falling back to string compare");
            a > b
        }
    }
}

/// v2 path profile — extends the v1 schema with `environment` and `vendor_scope`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CatalogueProfile {
    pub name: String,
    /// Environment archetypes this profile targets. Empty = all archetypes.
    #[serde(default)]
    pub environment: Vec<String>,
    /// Vendor tags this profile is scoped to. Empty = all vendors.
    #[serde(default)]
    pub vendor_scope: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub paths: Vec<CataloguePath>,
}

/// v2 path entry — extends v1 with `vendor_only` and `fallback_for`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CataloguePath {
    pub path: String,
    #[serde(default)]
    pub origin: String,
    pub mode: String,
    #[serde(default)]
    pub sample_interval_ns: u64,
    #[serde(default)]
    pub required_models: Vec<String>,
    #[serde(default)]
    pub required_any_models: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub rationale: String,
    /// Vendors this path is exclusive to. Empty = applies to all vendors.
    #[serde(default)]
    pub vendor_only: Vec<String>,
    /// Name of the preferred path this entry falls back for (documentation only).
    #[serde(default)]
    pub fallback_for: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    /// Profile YAML filenames relative to the plugin directory.
    #[serde(default)]
    pub profiles: Vec<String>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginState {
    pub manifest: PluginManifest,
    pub profiles: Vec<CatalogueProfile>,
    /// Names of profiles that were skipped due to name conflicts.
    pub conflicts: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CatalogueState {
    pub profiles: Vec<CatalogueProfile>,
    pub plugins: Vec<PluginState>,
    pub load_errors: Vec<String>,
}

impl CatalogueState {
    pub fn all_profiles(&self) -> impl Iterator<Item = &CatalogueProfile> {
        self.profiles
            .iter()
            .chain(self.plugins.iter().flat_map(|p| p.profiles.iter()))
    }
}

/// Load built-in profiles from `base_dir/*.yaml` and plugins from
/// `base_dir/plugins/*/MANIFEST.yaml`. Never fails — errors are collected into
/// `CatalogueState::load_errors`.
pub fn load_catalogue(base_dir: &Path) -> CatalogueState {
    let mut state = CatalogueState::default();

    // 1. Load built-ins
    match std::fs::read_dir(base_dir) {
        Err(e) => {
            state
                .load_errors
                .push(format!("cannot read catalogue dir '{}': {e}", base_dir.display()));
        }
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() || !is_yaml(&path) {
                    continue;
                }
                match load_profile_file(&path) {
                    Ok(profile) => {
                        info!(name = %profile.name, "loaded built-in catalogue profile");
                        state.profiles.push(profile);
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "skipping invalid profile");
                        state.load_errors.push(format!("'{}': {e}", path.display()));
                    }
                }
            }
        }
    }
    state.profiles.sort_by(|a, b| a.name.cmp(&b.name));

    // 2. Load plugins and resolve conflicts
    // name -> (PluginState, Profile)
    // winners is the authoritative map; plugin_profiles is unused after refactor
    let _plugin_profiles: HashMap<String, (usize, CatalogueProfile)> = HashMap::new();
    let mut plugins: Vec<PluginState> = Vec::new();

    let plugins_dir = base_dir.join("plugins");
    if plugins_dir.exists() {
        match std::fs::read_dir(&plugins_dir) {
            Err(e) => {
                state
                    .load_errors
                    .push(format!("cannot read plugins dir: {e}"));
            }
            Ok(entries) => {
                for entry in entries.flatten() {
                    let plugin_dir = entry.path();
                    if !plugin_dir.is_dir() {
                        continue;
                    }
                    
                    let manifest_path = plugin_dir.join("MANIFEST.yaml");
                    if !manifest_path.exists() {
                        continue;
                    }

                    match load_plugin_manifest(&manifest_path) {
                        Ok(manifest) => {
                            let mut loaded_profiles = Vec::new();
                            for profile_file in &manifest.profiles {
                                let profile_path = plugin_dir.join(profile_file);
                                match load_profile_file(&profile_path) {
                                    Ok(profile) => loaded_profiles.push(profile),
                                    Err(e) => {
                                        state.load_errors.push(format!("plugin '{}' profile '{}': {e}", manifest.name, profile_file));
                                    }
                                }
                            }
                            
                            plugins.push(PluginState {
                                manifest,
                                profiles: loaded_profiles,
                                conflicts: Vec::new(),
                            });
                        }
                        Err(e) => {
                            state.load_errors.push(format!("plugin '{}' manifest error: {e}", plugin_dir.display()));
                        }
                    }
                }
            }
        }
    }

    // Resolve conflicts
    let built_in_names: HashSet<String> = state.profiles.iter().map(|p| p.name.clone()).collect();
    
    // Sort plugins by version descending, then name alphabetical tie-break
    // Wait, simpler: build a map of winning profiles.
    // Winning logic:
    // 1. Built-in always wins.
    // 2. If plugin conflict: highest version wins.
    // 3. Tie-break: alphabetical plugin name.

    let mut winners: HashMap<String, (usize, String)> = HashMap::new(); // profile_name -> (plugin_index, version)

    for (idx, plugin) in plugins.iter().enumerate() {
        for profile in &plugin.profiles {
            if built_in_names.contains(&profile.name) {
                // Plugin vs Built-in: Built-in wins.
                continue;
            }

            if let Some(&(existing_idx, ref existing_version)) = winners.get(&profile.name) {
                // Plugin vs Plugin
                let current_version = &plugin.manifest.version;
                if semver_gt(current_version, existing_version) {
                    winners.insert(profile.name.clone(), (idx, current_version.clone()));
                } else if current_version == existing_version {
                    // Alphabetical tie-break on plugin name
                    if plugin.manifest.name < plugins[existing_idx].manifest.name {
                        winners.insert(profile.name.clone(), (idx, current_version.clone()));
                    }
                }
            } else {
                winners.insert(profile.name.clone(), (idx, plugin.manifest.version.clone()));
            }
        }
    }

    // Assign winners to state and record conflicts.
    // Pre-compute idx -> plugin name so the mutable loop doesn't borrow the slice immutably.
    let mut final_plugins = plugins;
    for plugin in &mut final_plugins {
        plugin.conflicts.clear();
    }
    let plugin_names: Vec<String> = final_plugins.iter().map(|p| p.manifest.name.clone()).collect();

    for (idx, plugin) in final_plugins.iter_mut().enumerate() {
        let mut actual_profiles = Vec::new();
        let mut seen_in_this_plugin = HashSet::new();

        // Use a copy of profiles to filter
        let temp_profiles = std::mem::take(&mut plugin.profiles);
        for profile in temp_profiles {
            if seen_in_this_plugin.contains(&profile.name) {
                // Internal duplicate in plugin
                continue;
            }
            seen_in_this_plugin.insert(profile.name.clone());

            if built_in_names.contains(&profile.name) {
                plugin.conflicts.push(format!("profile '{}' conflicts with a built-in profile", profile.name));
                continue;
            }

            if let Some(&(winner_idx, _)) = winners.get(&profile.name) {
                if winner_idx == idx {
                    actual_profiles.push(profile);
                } else {
                    let winner_name = plugin_names.get(winner_idx).map(|s| s.as_str()).unwrap_or("unknown");
                    plugin.conflicts.push(format!("profile '{}' superseded by plugin '{}'", profile.name, winner_name));
                }
            }
        }
        plugin.profiles = actual_profiles;
    }

    state.plugins = final_plugins;
    state.plugins.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));

    state
}

fn load_plugin_manifest(path: &Path) -> anyhow::Result<PluginManifest> {
    let raw = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read MANIFEST.yaml: {e}"))?;
    serde_yaml::from_str(&raw).map_err(|e| anyhow::anyhow!("cannot parse MANIFEST.yaml: {e}"))
}

fn load_profile_file(path: &Path) -> anyhow::Result<CatalogueProfile> {
    let raw = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("read error: {e}"))?;
    serde_yaml::from_str(&raw).map_err(|e| anyhow::anyhow!("parse error: {e}"))
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
}

/// Normalize a role hint to lowercase. Returns `"leaf"` for empty/missing hints.
/// The catalogue's `roles` lists are matched against the normalized form so the
/// YAML files drive the role → profile mapping rather than hardcoded code.
pub fn canonical_role(role_hint: Option<&str>) -> String {
    let raw = role_hint.unwrap_or("").trim().to_lowercase();
    if raw.is_empty() {
        "leaf".to_string()
    } else {
        raw
    }
}

/// True for roles that belong to the SP domain and need MPLS/IS-IS/SR paths in
/// the built-in fallback generator.
pub fn is_sp_role(role: &str) -> bool {
    matches!(role, "pe" | "p" | "rr" | "peering")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_gt_handles_double_digit_patch() {
        assert!(semver_gt("0.10.0", "0.2.0"), "0.10.0 must outrank 0.2.0");
        assert!(semver_gt("1.0.0", "0.99.9"), "1.0.0 must outrank 0.99.9");
        assert!(!semver_gt("0.2.0", "0.10.0"), "0.2.0 must not outrank 0.10.0");
        assert!(!semver_gt("0.1.0", "0.1.0"), "equal versions must not outrank each other");
    }
}

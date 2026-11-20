//! Canonical YANG source manifest.
//!
//! `config/yang_sources.toml` is the single authoritative definition of where
//! Bonsai obtains YANG models. Every source pins an exact immutable git ref —
//! moving branches are never promoted automatically. Import provenance
//! (repository, pinned ref, platform/NOS applicability, license) is recorded
//! on every imported module.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DEFAULT_MANIFEST_PATH: &str = "config/yang_sources.toml";

#[derive(Clone, Debug, Deserialize)]
pub struct YangSourceManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub source: Vec<YangSourceEntry>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct YangSourceEntry {
    pub id: String,
    pub vendor: String,
    pub repo: String,
    /// Exact 40-hex commit SHA. Required — HEAD, branches and tags are
    /// moving references and are rejected by manifest validation.
    pub pinned_ref: String,
    /// Platform/NOS applicability tags. `"*"` matches everything.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub subdirs: Vec<String>,
    /// Optional filename prefixes restricting which *.yang files import.
    #[serde(default)]
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub license: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl YangSourceEntry {
    /// True when `pinned_ref` is a full 40-hex commit SHA (preferred) rather
    /// than a tag or branch name.
    pub fn pinned_ref_is_commit(&self) -> bool {
        self.pinned_ref.len() == 40
            && self
                .pinned_ref
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }

    /// True when this source applies to the given platform tag.
    pub fn applies_to_platform(&self, platform: &str) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == "*" || p == platform)
    }
}

pub fn load_manifest(path: &Path) -> Result<YangSourceManifest> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read YANG source manifest '{}'", path.display()))?;
    let manifest: YangSourceManifest =
        toml::from_str(&raw).context("invalid YANG source manifest TOML")?;
    if manifest.schema_version != 1 {
        bail!(
            "unsupported YANG source manifest schema_version {} (expected 1)",
            manifest.schema_version
        );
    }
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Load the manifest from the conventional location, or return an empty
/// manifest when the file does not exist (backward compatibility for
/// deployments that only use `yang import`).
pub fn load_default_manifest() -> Result<YangSourceManifest> {
    let path = Path::new(DEFAULT_MANIFEST_PATH);
    if !path.exists() {
        return Ok(YangSourceManifest {
            schema_version: 1,
            source: Vec::new(),
        });
    }
    load_manifest(path)
}

fn validate_manifest(manifest: &YangSourceManifest) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    for source in &manifest.source {
        if source.id.trim().is_empty() {
            bail!("YANG source manifest entry has empty id");
        }
        if !ids.insert(source.id.as_str()) {
            bail!("duplicate YANG source id '{}'", source.id);
        }
        if source.vendor.trim().is_empty() {
            bail!("YANG source '{}' has empty vendor", source.id);
        }
        // https:// for network sources; file:// is permitted for
        // operator-managed local mirrors and test fixtures (the manifest
        // itself is trusted configuration, equivalent to bonsai.toml).
        if !source.repo.starts_with("https://") && !source.repo.starts_with("file://") {
            bail!(
                "YANG source '{}' repo must be an https:// or file:// URL (got '{}')",
                source.id,
                source.repo
            );
        }
        let reference = source.pinned_ref.trim();
        // pinned_ref MUST be a full 40-hex commit SHA. Branch names and tags
        // are moving references: a tag can be re-pointed and a branch
        // advances, so neither can serve as an immutable pin. (Tags were
        // previously tolerated; production manifests use SHAs exclusively.)
        let is_commit_sha = reference.len() == 40
            && reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !is_commit_sha {
            bail!(
                "YANG source '{}' pinned_ref must be a full 40-hex commit SHA (got '{}'); \
                 branches and tags are moving references and are not accepted",
                source.id,
                reference
            );
        }
        for subdir in &source.subdirs {
            if subdir.starts_with('/') || subdir.split('/').any(|c| c == "..") {
                bail!(
                    "YANG source '{}' subdir '{}' must be a safe relative path",
                    source.id,
                    subdir
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checked_in_manifest() {
        let manifest = load_manifest(Path::new(DEFAULT_MANIFEST_PATH))
            .expect("config/yang_sources.toml must parse");
        assert!(manifest.source.len() >= 8);
        for source in &manifest.source {
            assert!(!source.pinned_ref.is_empty());
            assert!(source.repo.starts_with("https://"));
        }
    }

    #[test]
    fn rejects_moving_refs() {
        let bad = r#"
schema_version = 1
[[source]]
id = "x"
vendor = "v"
repo = "https://example.com/r.git"
pinned_ref = "main"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, bad).unwrap();
        assert!(load_manifest(&path).is_err());
    }

    #[test]
    fn rejects_traversal_subdir() {
        let bad = r#"
schema_version = 1
[[source]]
id = "x"
vendor = "v"
repo = "https://example.com/r.git"
pinned_ref = "0123456789abcdef0123456789abcdef01234567"
subdirs = ["../escape"]
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, bad).unwrap();
        assert!(load_manifest(&path).is_err());
    }

    #[test]
    fn platform_applicability() {
        let entry = YangSourceEntry {
            id: "x".into(),
            vendor: "v".into(),
            repo: "https://example.com/r.git".into(),
            pinned_ref: "0123456789abcdef0123456789abcdef01234567".into(),
            platforms: vec!["cisco_iosxr".into()],
            subdirs: vec![],
            prefixes: vec![],
            license: String::new(),
            enabled: true,
        };
        assert!(entry.applies_to_platform("cisco_iosxr"));
        assert!(!entry.applies_to_platform("juniper_junos"));
    }
}

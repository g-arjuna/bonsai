use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::catalogue::load_catalogue;

type HmacSha256 = Hmac<Sha256>;

const INDEX_FILE: &str = "index.json";
const MODULES_DIR: &str = "modules";
const BUNDLE_MANIFEST_PATH: &str = "manifest.json";
const BUNDLE_SIGNATURE_PATH: &str = "signature.txt";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct YangModuleRecord {
    pub module_name: String,
    pub revision: String,
    pub namespace: String,
    pub organization: String,
    pub source_kind: String,
    pub source_ref: String,
    pub vendor_scope: String,
    pub trust: String,
    pub relative_path: String,
    pub checksum_sha256: String,
    pub imported_at_ns: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct YangPathIndexEntry {
    pub module_name: String,
    pub profile_name: String,
    pub path: String,
    pub origin: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct YangLibraryState {
    #[serde(default)]
    pub modules: Vec<YangModuleRecord>,
    #[serde(default)]
    pub path_index: Vec<YangPathIndexEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct YangImportReport {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub modules: Vec<YangModuleRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct YangSyncReport {
    pub sources: Vec<String>,
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct YangSearchResult {
    pub query: String,
    pub modules: Vec<YangModuleRecord>,
    pub paths: Vec<YangPathIndexEntry>,
}

#[derive(Clone, Debug)]
pub struct YangImportOptions {
    pub source_kind: String,
    pub source_ref: String,
    pub vendor_scope: String,
    pub trust: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YangBundleManifest {
    pub bundle_version: u32,
    pub vendor: String,
    pub version_filter: String,
    pub source: String,
    pub created_at_ns: i64,
    pub modules: Vec<YangBundleModule>,
    pub signature_alg: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YangBundleModule {
    pub module_name: String,
    pub revision: String,
    pub checksum_sha256: String,
    pub archive_path: String,
}

#[derive(Clone, Debug)]
pub struct YangSource {
    pub vendor: &'static str,
    pub repo_url: &'static str,
    pub clone_name: &'static str,
    pub subdirs: &'static [&'static str],
}

const DEFAULT_SOURCES: &[YangSource] = &[
    YangSource {
        vendor: "openconfig",
        repo_url: "https://github.com/openconfig/public.git",
        clone_name: "openconfig-public",
        subdirs: &["release/models"],
    },
    YangSource {
        vendor: "cisco",
        repo_url: "https://github.com/CiscoDevNet/yang.git",
        clone_name: "cisco-devnet-yang",
        subdirs: &["vendor/cisco/xr"],
    },
    YangSource {
        vendor: "juniper",
        repo_url: "https://github.com/Juniper/yang.git",
        clone_name: "juniper-yang",
        subdirs: &["."],
    },
    YangSource {
        vendor: "arista",
        repo_url: "https://github.com/aristanetworks/yang.git",
        clone_name: "arista-yang",
        subdirs: &["."],
    },
    YangSource {
        vendor: "nokia",
        repo_url: "https://github.com/nokia/srlinux-yang-models.git",
        clone_name: "nokia-srlinux-yang-models",
        subdirs: &["srlinux"],
    },
];

pub struct YangLibrary {
    root: PathBuf,
    cache_root: PathBuf,
    bundle_key_env: String,
}

impl YangLibrary {
    pub fn open(
        root: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
        bundle_key_env: impl Into<String>,
    ) -> Result<Self> {
        let root = root.into();
        let cache_root = cache_root.into();
        std::fs::create_dir_all(root.join(MODULES_DIR))
            .with_context(|| format!("failed to create YANG library '{}'", root.display()))?;
        std::fs::create_dir_all(&cache_root)
            .with_context(|| format!("failed to create YANG cache '{}'", cache_root.display()))?;
        Ok(Self {
            root,
            cache_root,
            bundle_key_env: bundle_key_env.into(),
        })
    }

    pub fn load_state(&self) -> Result<YangLibraryState> {
        let path = self.root.join(INDEX_FILE);
        if !path.exists() {
            return Ok(YangLibraryState::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read YANG index '{}'", path.display()))?;
        serde_json::from_str(&raw).context("invalid YANG index JSON")
    }

    pub fn list_modules(&self) -> Result<Vec<YangModuleRecord>> {
        let mut modules = self.load_state()?.modules;
        modules.sort_by(|a, b| {
            a.vendor_scope
                .cmp(&b.vendor_scope)
                .then_with(|| a.module_name.cmp(&b.module_name))
                .then_with(|| a.revision.cmp(&b.revision))
        });
        Ok(modules)
    }

    pub fn set_module_trust(
        &self,
        module_name: &str,
        revision: Option<&str>,
        trust: &str,
    ) -> Result<YangModuleRecord> {
        let mut state = self.load_state()?;
        let module = state
            .modules
            .iter_mut()
            .find(|module| {
                module.module_name == module_name
                    && revision
                        .map(|expected| module.revision == expected)
                        .unwrap_or(true)
            })
            .ok_or_else(|| anyhow!("module '{}' not found", module_name))?;
        module.trust = trust.to_string();
        let updated = module.clone();
        self.save_state(&state)?;
        Ok(updated)
    }

    pub fn import_directory(
        &self,
        source_dir: &Path,
        options: &YangImportOptions,
        catalogue_dir: &Path,
    ) -> Result<YangImportReport> {
        if !source_dir.exists() {
            bail!(
                "YANG import source '{}' does not exist",
                source_dir.display()
            );
        }
        let mut state = self.load_state()?;
        let mut imported = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;
        let mut changed_modules = Vec::new();
        let mut seen_keys = BTreeSet::new();

        for file in collect_yang_files(source_dir)? {
            let raw = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read YANG file '{}'", file.display()))?;
            let parsed = parse_yang_metadata(&raw).with_context(|| {
                format!("failed to parse YANG metadata from '{}'", file.display())
            })?;
            let checksum_sha256 = sha256_hex(raw.as_bytes());
            let revision = parsed.revision.unwrap_or_else(|| "unknown".to_string());
            let file_name = format!(
                "{}@{}.yang",
                sanitize(&parsed.module_name),
                sanitize(&revision)
            );
            let relative_path = format!("{MODULES_DIR}/{file_name}");
            let module_path = self.root.join(&relative_path);
            let record = YangModuleRecord {
                module_name: parsed.module_name,
                revision,
                namespace: parsed.namespace.unwrap_or_default(),
                organization: parsed.organization.unwrap_or_default(),
                source_kind: options.source_kind.clone(),
                source_ref: options.source_ref.clone(),
                vendor_scope: options.vendor_scope.clone(),
                trust: options.trust.clone(),
                relative_path,
                checksum_sha256,
                imported_at_ns: now_ns(),
            };
            let key = format!("{}@{}", record.module_name, record.revision);
            if !seen_keys.insert(key) {
                skipped += 1;
                continue;
            }

            let existing = state.modules.iter().position(|module| {
                module.module_name == record.module_name && module.revision == record.revision
            });
            match existing {
                Some(index) if state.modules[index].checksum_sha256 == record.checksum_sha256 => {
                    skipped += 1;
                }
                Some(index) => {
                    std::fs::write(&module_path, raw.as_bytes()).with_context(|| {
                        format!("failed to update YANG module '{}'", module_path.display())
                    })?;
                    state.modules[index] = record.clone();
                    changed_modules.push(record);
                    updated += 1;
                }
                None => {
                    std::fs::write(&module_path, raw.as_bytes()).with_context(|| {
                        format!("failed to import YANG module '{}'", module_path.display())
                    })?;
                    state.modules.push(record.clone());
                    changed_modules.push(record);
                    imported += 1;
                }
            }
        }

        rebuild_path_index(&mut state, catalogue_dir);
        self.save_state(&state)?;

        Ok(YangImportReport {
            imported,
            updated,
            skipped,
            modules: changed_modules,
        })
    }

    pub fn sync(
        &self,
        vendor_filter: Option<&str>,
        catalogue_dir: &Path,
    ) -> Result<YangSyncReport> {
        let selected = DEFAULT_SOURCES
            .iter()
            .filter(|source| vendor_filter.is_none_or(|vendor| source.vendor == vendor))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            bail!(
                "no YANG source matched filter '{}'",
                vendor_filter.unwrap_or_default()
            );
        }

        let mut imported = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;
        let mut sources = Vec::new();
        let sources_root = self.cache_root.join("sources");
        std::fs::create_dir_all(&sources_root)?;

        for source in selected {
            let clone_dir = sources_root.join(source.clone_name);
            if clone_dir.exists() {
                run_git(Some(&clone_dir), ["pull", "--ff-only", "--quiet"])?;
            } else {
                run_git(
                    None,
                    [
                        "clone",
                        "--depth=1",
                        "--quiet",
                        source.repo_url,
                        &clone_dir.to_string_lossy(),
                    ],
                )?;
            }
            let revision = git_rev_parse(&clone_dir)?;
            let source_ref = format!("{}@{}", source.repo_url, revision);
            sources.push(source_ref.clone());

            for subdir in source.subdirs {
                let dir = clone_dir.join(subdir);
                if !dir.exists() {
                    continue;
                }
                let report = self.import_directory(
                    &dir,
                    &YangImportOptions {
                        source_kind: "sync".to_string(),
                        source_ref: source_ref.clone(),
                        vendor_scope: source.vendor.to_string(),
                        trust: "trusted".to_string(),
                    },
                    catalogue_dir,
                )?;
                imported += report.imported;
                updated += report.updated;
                skipped += report.skipped;
            }
        }

        Ok(YangSyncReport {
            sources,
            imported,
            updated,
            skipped,
        })
    }

    pub fn search(&self, query: &str) -> Result<YangSearchResult> {
        let state = self.load_state()?;
        let q = query.trim().to_ascii_lowercase();
        let modules = state
            .modules
            .into_iter()
            .filter(|module| {
                if q.is_empty() {
                    return true;
                }
                module.module_name.to_ascii_lowercase().contains(&q)
                    || module.namespace.to_ascii_lowercase().contains(&q)
                    || module.organization.to_ascii_lowercase().contains(&q)
                    || module.vendor_scope.to_ascii_lowercase().contains(&q)
            })
            .collect();
        let paths = state
            .path_index
            .into_iter()
            .filter(|entry| {
                if q.is_empty() {
                    return true;
                }
                entry.module_name.to_ascii_lowercase().contains(&q)
                    || entry.profile_name.to_ascii_lowercase().contains(&q)
                    || entry.path.to_ascii_lowercase().contains(&q)
                    || entry.rationale.to_ascii_lowercase().contains(&q)
            })
            .collect();
        Ok(YangSearchResult {
            query: query.to_string(),
            modules,
            paths,
        })
    }

    pub fn create_bundle(
        &self,
        vendor: &str,
        version_filter: Option<&str>,
        output_path: &Path,
    ) -> Result<YangBundleManifest> {
        let state = self.load_state()?;
        let key = self.bundle_key()?;
        let version_filter = version_filter.unwrap_or_default();
        let selected = state
            .modules
            .iter()
            .filter(|module| {
                (vendor.is_empty() || module.vendor_scope.eq_ignore_ascii_case(vendor))
                    && (version_filter.is_empty()
                        || module.revision.contains(version_filter)
                        || module.source_ref.contains(version_filter))
            })
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            bail!(
                "no YANG modules matched vendor '{}' and version filter '{}'",
                vendor,
                version_filter
            );
        }

        let manifest = YangBundleManifest {
            bundle_version: 1,
            vendor: vendor.to_string(),
            version_filter: version_filter.to_string(),
            source: self.root.display().to_string(),
            created_at_ns: now_ns(),
            modules: selected
                .iter()
                .map(|module| YangBundleModule {
                    module_name: module.module_name.clone(),
                    revision: module.revision.clone(),
                    checksum_sha256: module.checksum_sha256.clone(),
                    archive_path: module.relative_path.clone(),
                })
                .collect(),
            signature_alg: "hmac-sha256".to_string(),
        };
        let manifest_json = serde_json::to_vec_pretty(&manifest)
            .context("failed to serialize YANG bundle manifest")?;
        let signature = sign_bytes(&key, &manifest_json)?;

        let output = File::create(output_path)
            .with_context(|| format!("failed to create bundle '{}'", output_path.display()))?;
        let mut builder = tar::Builder::new(output);
        append_bytes(&mut builder, BUNDLE_MANIFEST_PATH, &manifest_json)?;
        append_bytes(&mut builder, BUNDLE_SIGNATURE_PATH, signature.as_bytes())?;
        for module in &selected {
            let mut bytes = Vec::new();
            File::open(self.root.join(&module.relative_path))
                .with_context(|| format!("failed to open module '{}'", module.relative_path))?
                .read_to_end(&mut bytes)?;
            append_bytes(&mut builder, &module.relative_path, &bytes)?;
        }
        builder.finish().context("failed to finish YANG bundle")?;
        Ok(manifest)
    }

    pub fn install_bundle(
        &self,
        bundle_path: &Path,
        catalogue_dir: &Path,
    ) -> Result<YangImportReport> {
        let temp_root = self
            .cache_root
            .join("bundle-import")
            .join(format!("bundle-{}", now_ns()));
        std::fs::create_dir_all(&temp_root)?;
        let file = File::open(bundle_path)
            .with_context(|| format!("failed to open bundle '{}'", bundle_path.display()))?;
        let mut archive = tar::Archive::new(file);
        archive
            .unpack(&temp_root)
            .with_context(|| format!("failed to unpack bundle '{}'", bundle_path.display()))?;

        let manifest_path = temp_root.join(BUNDLE_MANIFEST_PATH);
        let signature_path = temp_root.join(BUNDLE_SIGNATURE_PATH);
        let manifest_json = std::fs::read(&manifest_path)
            .with_context(|| format!("failed to read '{}'", manifest_path.display()))?;
        let signature = std::fs::read_to_string(&signature_path)
            .with_context(|| format!("failed to read '{}'", signature_path.display()))?;
        let key = self.bundle_key()?;
        verify_signature(&key, &manifest_json, signature.trim())?;
        let manifest: YangBundleManifest =
            serde_json::from_slice(&manifest_json).context("invalid YANG bundle manifest")?;

        for module in &manifest.modules {
            let module_path = temp_root.join(&module.archive_path);
            let checksum = sha256_hex(&std::fs::read(&module_path)?);
            if checksum != module.checksum_sha256 {
                bail!(
                    "bundle checksum mismatch for module '{}' revision '{}'",
                    module.module_name,
                    module.revision
                );
            }
        }

        let import_root = temp_root.join(MODULES_DIR);
        let result = self.import_directory(
            &import_root,
            &YangImportOptions {
                source_kind: "bundle".to_string(),
                source_ref: bundle_path.display().to_string(),
                vendor_scope: manifest.vendor,
                trust: "trusted".to_string(),
            },
            catalogue_dir,
        )?;
        let _ = std::fs::remove_dir_all(&temp_root);
        Ok(result)
    }

    pub fn bundle_key_env(&self) -> &str {
        &self.bundle_key_env
    }

    fn bundle_key(&self) -> Result<Vec<u8>> {
        let key = std::env::var(&self.bundle_key_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "set {} to create or install signed YANG bundles",
                    self.bundle_key_env
                )
            })?;
        Ok(key.into_bytes())
    }

    fn save_state(&self, state: &YangLibraryState) -> Result<()> {
        let path = self.root.join(INDEX_FILE);
        let raw = serde_json::to_vec_pretty(state).context("failed to serialize YANG index")?;
        std::fs::write(&path, raw)
            .with_context(|| format!("failed to write YANG index '{}'", path.display()))
    }
}

pub fn evaluate_profile_requirements(
    library_state: &YangLibraryState,
    required_models: &[String],
    required_any_models: &[String],
) -> Option<String> {
    let installed = library_state
        .modules
        .iter()
        .map(|module| module.module_name.as_str())
        .collect::<BTreeSet<_>>();

    let missing_required = required_models
        .iter()
        .filter(|required| !installed.contains(required.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_required.is_empty() {
        return Some(format!(
            "local YANG library is missing required models {:?}",
            missing_required
        ));
    }

    if !required_any_models.is_empty()
        && !required_any_models
            .iter()
            .any(|required| installed.contains(required.as_str()))
    {
        return Some(format!(
            "local YANG library is missing every alternative model {:?}",
            required_any_models
        ));
    }

    None
}

fn rebuild_path_index(state: &mut YangLibraryState, catalogue_dir: &Path) {
    let catalogue = load_catalogue(catalogue_dir);
    let mut index = Vec::new();
    for profile in catalogue.all_profiles() {
        for path in &profile.paths {
            let module_names = if !path.required_models.is_empty() {
                path.required_models.clone()
            } else {
                path.required_any_models.clone()
            };
            for module_name in module_names {
                index.push(YangPathIndexEntry {
                    module_name,
                    profile_name: profile.name.clone(),
                    path: path.path.clone(),
                    origin: path.origin.clone(),
                    rationale: path.rationale.clone(),
                });
            }
        }
    }
    index.sort_by(|a, b| {
        a.module_name
            .cmp(&b.module_name)
            .then_with(|| a.profile_name.cmp(&b.profile_name))
            .then_with(|| a.path.cmp(&b.path))
    });
    index.dedup_by(|a, b| {
        a.module_name == b.module_name && a.profile_name == b.profile_name && a.path == b.path
    });
    state.path_index = index;
}

fn collect_yang_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)
            .with_context(|| format!("failed to read '{}'", path.display()))?
        {
            let entry = entry?;
            let child = entry.path();
            if child.is_dir() {
                stack.push(child);
            } else if child
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("yang"))
            {
                files.push(child);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Default)]
struct ParsedYangMetadata {
    module_name: String,
    revision: Option<String>,
    namespace: Option<String>,
    organization: Option<String>,
}

fn parse_yang_metadata(raw: &str) -> Result<ParsedYangMetadata> {
    let mut parsed = ParsedYangMetadata::default();
    for line in raw.lines() {
        let trimmed = line.trim();
        if parsed.module_name.is_empty()
            && (trimmed.starts_with("module ") || trimmed.starts_with("submodule "))
        {
            parsed.module_name = trimmed
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .trim_end_matches('{')
                .trim_end_matches(';')
                .trim_matches('"')
                .to_string();
            continue;
        }
        if parsed.namespace.is_none() && trimmed.starts_with("namespace ") {
            parsed.namespace = extract_quoted_or_bare_value(trimmed);
            continue;
        }
        if parsed.organization.is_none() && trimmed.starts_with("organization ") {
            parsed.organization = extract_quoted_or_bare_value(trimmed);
            continue;
        }
        if parsed.revision.is_none() && trimmed.starts_with("revision ") {
            parsed.revision = extract_quoted_or_bare_value(trimmed);
        }
    }
    if parsed.module_name.trim().is_empty() {
        bail!("missing module/submodule declaration");
    }
    Ok(parsed)
}

fn extract_quoted_or_bare_value(line: &str) -> Option<String> {
    if let Some(start) = line.find('"') {
        let rest = &line[start + 1..];
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    line.split_whitespace().nth(1).map(|value| {
        value
            .trim_end_matches('{')
            .trim_end_matches(';')
            .to_string()
    })
}

fn sanitize(value: &str) -> String {
    value.replace(['/', '\\', ':', ' '], "_")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn sign_bytes(key: &[u8], bytes: &[u8]) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(key).context("invalid YANG bundle signing key")?;
    mac.update(bytes);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn verify_signature(key: &[u8], bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sign_bytes(key, bytes)?;
    // Constant-time comparison — prevents timing side-channel on HMAC output
    if actual.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1 {
        Ok(())
    } else {
        bail!("YANG bundle signature verification failed")
    }
}

fn append_bytes(builder: &mut tar::Builder<File>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, bytes)
        .with_context(|| format!("failed to append '{}' to YANG bundle", path))
}

fn run_git<I, S>(cwd: Option<&Path>, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let status = cmd.status().context("git is required for yang-sync")?;
    if status.success() {
        Ok(())
    } else {
        bail!("git command failed with exit code {:?}", status.code())
    }
}

fn git_rev_parse(repo_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("git is required for yang-sync")?;
    if !output.status.success() {
        bail!(
            "failed to determine git revision for '{}'",
            repo_dir.display()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn now_ns() -> i64 {
    crate::graph::common::now_ns()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_yang_metadata() {
        let parsed = parse_yang_metadata(
            r#"
            module openconfig-bgp {
              namespace "http://openconfig.net/yang/bgp";
              organization "OpenConfig";
              revision "2024-01-01" {
              }
            }
            "#,
        )
        .expect("parsed metadata");
        assert_eq!(parsed.module_name, "openconfig-bgp");
        assert_eq!(parsed.revision.as_deref(), Some("2024-01-01"));
        assert_eq!(
            parsed.namespace.as_deref(),
            Some("http://openconfig.net/yang/bgp")
        );
    }

    #[test]
    fn path_requirement_check_reports_missing_models() {
        let state = YangLibraryState {
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

        let warning =
            evaluate_profile_requirements(&state, &["openconfig-bgp".to_string()], &Vec::new())
                .expect("missing model warning");
        assert!(warning.contains("openconfig-bgp"));
    }

    #[test]
    fn search_matches_modules_and_paths() {
        let state = YangLibraryState {
            modules: vec![YangModuleRecord {
                module_name: "openconfig-bgp".to_string(),
                revision: "2024-01-01".to_string(),
                namespace: "urn:oc:bgp".to_string(),
                organization: "OpenConfig".to_string(),
                source_kind: "manual".to_string(),
                source_ref: "test".to_string(),
                vendor_scope: "openconfig".to_string(),
                trust: "trusted".to_string(),
                relative_path: "modules/openconfig-bgp@2024-01-01.yang".to_string(),
                checksum_sha256: "abc".to_string(),
                imported_at_ns: 1,
            }],
            path_index: vec![YangPathIndexEntry {
                module_name: "openconfig-bgp".to_string(),
                profile_name: "dc_leaf_minimal".to_string(),
                path: "network-instances/network-instance/protocols/protocol/bgp".to_string(),
                origin: "openconfig".to_string(),
                rationale: "BGP state".to_string(),
            }],
        };
        let root = tempfile::tempdir().expect("tempdir");
        let library = YangLibrary::open(root.path(), root.path().join("cache"), "TEST_KEY")
            .expect("open library");
        library.save_state(&state).expect("save state");
        let result = library.search("bgp").expect("search");
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.paths.len(), 1);
    }
}

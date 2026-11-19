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
/// Maximum size of a single imported .yang file.
pub const MAX_YANG_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum number of .yang files accepted by one import/sync pass.
pub const MAX_YANG_FILES_PER_IMPORT: usize = 50_000;
/// Maximum aggregate bytes read during one import/sync pass.
pub const MAX_YANG_TOTAL_IMPORT_BYTES: u64 = 1024 * 1024 * 1024;
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
    /// License/provenance note carried from the source manifest.
    #[serde(default)]
    pub license: String,
    /// Platform/NOS applicability tags from the source manifest.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub pinned_ref: String,
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
    /// Non-fatal issues (oversized files, filtered prefixes, conflicts).
    #[serde(default)]
    pub warnings: Vec<String>,
    pub modules: Vec<YangModuleRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct YangSyncReport {
    pub sources: Vec<String>,
    /// Per-source diagnostics (missing subdir, ref resolution, etc).
    #[serde(default)]
    pub warnings: Vec<String>,
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
    pub license: String,
    pub platforms: Vec<String>,
    pub pinned_ref: String,
    /// When non-empty, only files whose name starts with one of these
    /// prefixes are imported.
    pub filename_prefixes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YangBundleManifest {
    pub bundle_version: u32,
    pub vendor: String,
    pub version_filter: String,
    pub source: String,
    pub created_at_ns: i64,
    pub modules: Vec<YangBundleModule>,
    /// Content identity: sha256 over sorted "name@revision@checksum" lines.
    /// Empty only in pre-M6 bundles.
    #[serde(default)]
    pub bundle_id: String,
    pub signature_alg: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YangBundleModule {
    pub module_name: String,
    pub revision: String,
    pub checksum_sha256: String,
    pub archive_path: String,
}

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

    /// Filesystem root of the YANG library (index + module blobs).
    pub fn root(&self) -> &Path {
        &self.root
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
        let updated = 0usize;
        let mut skipped = 0usize;
        let mut changed_modules = Vec::new();
        let mut warnings = Vec::new();
        let mut seen_keys = BTreeSet::new();
        let mut total_bytes = 0u64;

        let files = collect_yang_files(source_dir)?;
        if files.len() > MAX_YANG_FILES_PER_IMPORT {
            bail!(
                "YANG import exceeds {} file limit ({} files)",
                MAX_YANG_FILES_PER_IMPORT,
                files.len()
            );
        }

        for file in files {
            let file_name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            if !options.filename_prefixes.is_empty()
                && !options
                    .filename_prefixes
                    .iter()
                    .any(|p| file_name.starts_with(p.as_str()))
            {
                skipped += 1;
                continue;
            }
            let meta = std::fs::metadata(&file)
                .with_context(|| format!("failed to stat '{}'", file.display()))?;
            if meta.len() > MAX_YANG_FILE_BYTES {
                warnings.push(format!(
                    "skipped oversized YANG file '{}' ({} bytes > {} limit)",
                    file.display(),
                    meta.len(),
                    MAX_YANG_FILE_BYTES
                ));
                skipped += 1;
                continue;
            }
            total_bytes += meta.len();
            if total_bytes > MAX_YANG_TOTAL_IMPORT_BYTES {
                bail!(
                    "YANG import exceeds {} total byte limit",
                    MAX_YANG_TOTAL_IMPORT_BYTES
                );
            }
            let raw = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read YANG file '{}'", file.display()))?;
            let parsed = parse_yang_metadata(&raw).with_context(|| {
                format!("failed to parse YANG metadata from '{}'", file.display())
            })?;
            let checksum_sha256 = sha256_hex(raw.as_bytes());
            let revision = parsed.revision.unwrap_or_else(|| "unknown".to_string());

            // Content-addressed immutable storage: module bytes live under
            // modules/blobs/<sha256>.yang and are never overwritten.
            let relative_path = format!("{MODULES_DIR}/blobs/{checksum_sha256}.yang");
            let module_path = self.root.join(&relative_path);
            if !module_path.exists() {
                if let Some(parent) = module_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&module_path, raw.as_bytes()).with_context(|| {
                    format!("failed to store YANG module '{}'", module_path.display())
                })?;
            }

            let record = YangModuleRecord {
                module_name: parsed.module_name,
                revision,
                namespace: parsed.namespace.unwrap_or_default(),
                organization: parsed.organization.unwrap_or_default(),
                source_kind: options.source_kind.clone(),
                source_ref: options.source_ref.clone(),
                vendor_scope: options.vendor_scope.clone(),
                trust: options.trust.clone(),
                license: options.license.clone(),
                platforms: options.platforms.clone(),
                pinned_ref: options.pinned_ref.clone(),
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
                module.module_name == record.module_name
                    && module.revision == record.revision
                    && module.checksum_sha256 == record.checksum_sha256
            });
            match existing {
                Some(_) => {
                    skipped += 1;
                }
                None => {
                    // Same name+revision with different content: never
                    // overwrite or silently replace — append a distinct
                    // content record; active selection is deterministic
                    // (trust rank, then newest import, then checksum).
                    let conflict = state.modules.iter().any(|module| {
                        module.module_name == record.module_name
                            && module.revision == record.revision
                    });
                    if conflict {
                        warnings.push(format!(
                            "conflicting content for {}@{}: stored as distinct record {}",
                            record.module_name,
                            record.revision,
                            &record.checksum_sha256[..12.min(record.checksum_sha256.len())]
                        ));
                    }
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
            warnings,
            modules: changed_modules,
        })
    }

    /// Sync modules from the canonical source manifest
    /// (`config/yang_sources.toml`). Every source is checked out at its
    /// pinned ref — moving branches are never followed.
    pub fn sync(
        &self,
        vendor_filter: Option<&str>,
        catalogue_dir: &Path,
    ) -> Result<YangSyncReport> {
        self.sync_from_manifest(None, vendor_filter, catalogue_dir)
    }

    /// Sync from an explicit manifest (tests / alternate locations).
    /// `platform_filter` restricts sources by their `platforms` tags.
    pub fn sync_from_manifest(
        &self,
        manifest_path: Option<&Path>,
        vendor_filter: Option<&str>,
        catalogue_dir: &Path,
    ) -> Result<YangSyncReport> {
        let manifest = match manifest_path {
            Some(path) => crate::yang_source::load_manifest(path)?,
            None => crate::yang_source::load_default_manifest()?,
        };
        let selected = manifest
            .source
            .iter()
            .filter(|s| s.enabled)
            .filter(|s| vendor_filter.is_none_or(|v| s.vendor == v || s.id == v))
            .collect::<Vec<_>>();
        if selected.is_empty() {
            bail!(
                "no enabled YANG source matched filter '{}'",
                vendor_filter.unwrap_or("<all>")
            );
        }

        let mut imported = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;
        let mut sources = Vec::new();
        let mut warnings = Vec::new();
        let sources_root = self.cache_root.join("sources");
        std::fs::create_dir_all(&sources_root)?;

        for source in selected {
            let clone_dir = sources_root.join(&source.id);
            match checkout_pinned(&clone_dir, &source.repo, &source.pinned_ref) {
                Ok(resolved_ref) => {
                    let source_ref = format!("{}@{}", source.repo, resolved_ref);
                    sources.push(source_ref.clone());
                    for subdir in &source.subdirs {
                        let dir = clone_dir.join(subdir);
                        if !dir.exists() || !dir.is_dir() {
                            warnings.push(format!(
                                "source '{}' subdir '{}' not present at pinned ref",
                                source.id, subdir
                            ));
                            continue;
                        }
                        let report = self.import_directory(
                            &dir,
                            &YangImportOptions {
                                source_kind: "sync".to_string(),
                                source_ref: source_ref.clone(),
                                vendor_scope: source.vendor.clone(),
                                trust: "trusted".to_string(),
                                license: source.license.clone(),
                                platforms: source.platforms.clone(),
                                pinned_ref: source.pinned_ref.clone(),
                                filename_prefixes: source.prefixes.clone(),
                            },
                            catalogue_dir,
                        )?;
                        imported += report.imported;
                        // Content-addressed store means "updated" only counts
                        // genuinely new content records; identical re-syncs
                        // surface as skipped.
                        updated += report.updated;
                        skipped += report.skipped;
                        warnings.extend(
                            report
                                .warnings
                                .iter()
                                .map(|w| format!("source '{}': {}", source.id, w)),
                        );
                    }
                }
                Err(e) => {
                    warnings.push(format!(
                        "source '{}' ({}) failed: {} — existing library unchanged",
                        source.id, source.repo, e
                    ));
                }
            }
        }

        if sources.is_empty() && !warnings.is_empty() {
            bail!("all YANG sources failed: {}", warnings.join("; "));
        }

        Ok(YangSyncReport {
            sources,
            warnings,
            imported,
            updated,
            skipped,
        })
    }

    /// Deterministic active-module view: for each (name, revision) content
    /// conflict, select the winner by trust rank, then newest import, then
    /// checksum. Previously stored content is never removed.
    pub fn active_modules(&self) -> Result<Vec<YangModuleRecord>> {
        let state = self.load_state()?;
        Ok(select_active_records(&state.modules))
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

        let bundle_modules: Vec<YangBundleModule> = selected
            .iter()
            .map(|module| YangBundleModule {
                module_name: module.module_name.clone(),
                revision: module.revision.clone(),
                checksum_sha256: module.checksum_sha256.clone(),
                archive_path: module.relative_path.clone(),
            })
            .collect();
        let manifest = YangBundleManifest {
            bundle_version: 1,
            vendor: vendor.to_string(),
            version_filter: version_filter.to_string(),
            source: self.root.display().to_string(),
            created_at_ns: now_ns(),
            bundle_id: compute_bundle_id(&bundle_modules),
            modules: bundle_modules,
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
        if !manifest.bundle_id.is_empty() {
            let expected = compute_bundle_id(&manifest.modules);
            if expected != manifest.bundle_id {
                bail!("YANG bundle content identity mismatch");
            }
        }

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
                license: String::new(),
                platforms: Vec::new(),
                pinned_ref: String::new(),
                filename_prefixes: Vec::new(),
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

/// Deterministic bundle content identity over the module manifest.
pub fn compute_bundle_id(modules: &[YangBundleModule]) -> String {
    let mut lines: Vec<String> = modules
        .iter()
        .map(|m| format!("{}@{}@{}", m.module_name, m.revision, m.checksum_sha256))
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for line in lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    format!("bundle-{}", hex::encode(&hasher.finalize()[..12]))
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

fn git_output<I, S>(cwd: Option<&Path>, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd.output().context("git is required for yang-sync")?;
    if !output.status.success() {
        bail!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check out `repo` at exactly `pinned_ref` in `dir`, returning the resolved
/// commit SHA. Uses a bounded single-commit fetch of the pinned object so a
/// moving default branch can never leak in. Re-running always lands on the
/// pinned ref (idempotent).
fn checkout_pinned(dir: &Path, repo: &str, pinned_ref: &str) -> Result<String> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        run_git(Some(dir), ["init", "--quiet"])?;
        run_git(Some(dir), ["remote", "add", "origin", repo])?;
    }
    // Fetch exactly the pinned object (GitHub allows fetching reachable SHAs
    // and tags). --depth 1 keeps the cache bounded. When the direct fetch
    // fails (e.g. local mirrors that don't allow unadvertised wants), fall
    // back to a bounded ref fetch and check the object out by name.
    if run_git(
        Some(dir),
        ["fetch", "--depth=1", "--quiet", "origin", pinned_ref],
    )
    .is_err()
    {
        run_git(
            Some(dir),
            [
                "fetch",
                "--depth=100",
                "--quiet",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ],
        )
        .with_context(|| format!("failed to fetch pinned ref '{}' from {}", pinned_ref, repo))?;
        // Verify the pinned object exists locally before checkout.
        let cat = git_output(
            Some(dir),
            ["cat-file", "-e", &format!("{}^{{commit}}", pinned_ref)],
        )
        .or_else(|_| git_output(Some(dir), ["cat-file", "-e", pinned_ref]));
        if cat.is_err() {
            bail!(
                "pinned ref '{}' not found in {} after bounded fetch",
                pinned_ref,
                repo
            );
        }
        run_git(Some(dir), ["checkout", "--quiet", pinned_ref])?;
    } else {
        run_git(Some(dir), ["checkout", "--quiet", "FETCH_HEAD"])?;
    }
    let head = git_output(Some(dir), ["rev-parse", "HEAD"])?;
    if pinned_ref.len() == 40
        && pinned_ref.bytes().all(|b| b.is_ascii_hexdigit())
        && head != pinned_ref
    {
        bail!(
            "pinned ref '{}' resolved to '{}' — refusing non-matching checkout",
            pinned_ref,
            head
        );
    }
    Ok(head)
}

/// Trust ordering for active-module selection: trusted > experimental >
/// anything else.
pub fn trust_rank(trust: &str) -> u8 {
    match trust {
        "trusted" => 2,
        "experimental" => 1,
        _ => 0,
    }
}

/// Deterministic winner selection for same (module_name, revision) content
/// conflicts: highest trust rank, then newest import, then lowest checksum
/// for a stable total order. All records remain stored for rollback/audit.
pub fn select_active_records(records: &[YangModuleRecord]) -> Vec<YangModuleRecord> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<&YangModuleRecord>> =
        std::collections::BTreeMap::new();
    for record in records {
        groups
            .entry((record.module_name.clone(), record.revision.clone()))
            .or_default()
            .push(record);
    }
    let mut out = Vec::new();
    for (_key, mut group) in groups {
        group.sort_by(|a, b| {
            trust_rank(&b.trust)
                .cmp(&trust_rank(&a.trust))
                .then_with(|| b.imported_at_ns.cmp(&a.imported_at_ns))
                .then_with(|| a.checksum_sha256.cmp(&b.checksum_sha256))
        });
        out.push(group[0].clone());
    }
    out.sort_by(|a, b| {
        a.vendor_scope
            .cmp(&b.vendor_scope)
            .then_with(|| a.module_name.cmp(&b.module_name))
            .then_with(|| a.revision.cmp(&b.revision))
    });
    out
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
                license: String::new(),
                platforms: Vec::new(),
                pinned_ref: String::new(),
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
                license: String::new(),
                platforms: Vec::new(),
                pinned_ref: String::new(),
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

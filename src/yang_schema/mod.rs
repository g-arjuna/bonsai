//! Effective YANG schema catalogue (M6).
//!
//! Layers:
//! - `parser`: RFC 7950/7951 module parser producing a typed AST.
//! - `compiler`: staged resolution (imports, includes, uses/groupings,
//!   augments, deviations, features) into a deterministic compiled catalogue
//!   and canonical schema-path index.
//!
//! The compiled output is content-addressed (`schema_id`) and stored under
//! `<yang_library>/compiled/` with an atomic `active.json` pointer so a
//! failed compile never disturbs the active catalogue and rollback is a
//! pointer switch.

mod compiler;
mod parser;

pub use compiler::{
    CompileOptions, CompiledCatalogue, CompiledFeature, CompiledModule, ModuleInput,
    SchemaPathEntry, compile, filter_by_features,
};
pub use parser::{Module, parse_module};

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::yang::YangLibrary;

pub const COMPILED_DIR: &str = "compiled";
pub const ACTIVE_POINTER: &str = "active.json";
const INDEX_FILE: &str = "index.json";
/// Bound on compiled bundles retained on disk (oldest evicted first).
pub const MAX_COMPILED_BUNDLES: usize = 32;
/// Bound on total size of one compiled bundle JSON.
pub const MAX_COMPILED_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledBundleInfo {
    pub schema_id: String,
    pub compiled_at_ns: i64,
    pub module_count: usize,
    pub path_count: usize,
    pub diagnostics: usize,
    /// True for the bundle currently referenced by `active.json`.
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActivePointer {
    schema_id: String,
}

/// Load and parse every module record in the library into compiler inputs.
/// Unparseable files are recorded as diagnostics, never fatal — a partially
/// valid library still compiles what it can, and the diagnostics explain the
/// gaps. Records with trust != "trusted" are included only when
/// `include_untrusted` is set.
pub fn load_module_inputs(
    library: &YangLibrary,
    include_untrusted: bool,
) -> Result<(Vec<ModuleInput>, Vec<String>)> {
    // Active-module selection resolves same name+revision content
    // conflicts deterministically; all stored content remains available
    // for rollback via bundle install.
    let records = library.active_modules()?;
    let mut inputs = Vec::new();
    let mut diagnostics = Vec::new();
    for record in records {
        if !include_untrusted && record.trust != "trusted" {
            continue;
        }
        let path = library.root().join(&record.relative_path);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                diagnostics.push(format!(
                    "module '{}@{}' unreadable at '{}': {}",
                    record.module_name,
                    record.revision,
                    path.display(),
                    e
                ));
                continue;
            }
        };
        match parse_module(&raw) {
            Ok(module) => inputs.push(ModuleInput {
                module,
                vendor_scope: record.vendor_scope.clone(),
                source_kind: record.source_kind.clone(),
                source_ref: record.source_ref.clone(),
                trust: record.trust.clone(),
                checksum_sha256: record.checksum_sha256.clone(),
            }),
            Err(e) => diagnostics.push(format!(
                "module '{}@{}' parse failed: {}",
                record.module_name, record.revision, e
            )),
        }
    }
    Ok((inputs, diagnostics))
}

/// Compile the library's modules into a deterministic compiled catalogue and
/// persist it as an inactive bundle. Returns the compiled catalogue.
pub fn compile_library(library: &YangLibrary, trusted_only: bool) -> Result<CompiledCatalogue> {
    let (inputs, mut pre_diagnostics) = load_module_inputs(library, !trusted_only)?;
    let options = CompileOptions { trusted_only };
    let mut catalogue = compile(inputs, &options);
    catalogue
        .diagnostics
        .splice(0..0, pre_diagnostics.drain(..));
    persist_compiled(library, &catalogue)?;
    Ok(catalogue)
}

fn compiled_dir(library: &YangLibrary) -> PathBuf {
    library.root().join(COMPILED_DIR)
}

fn bundle_path(library: &YangLibrary, schema_id: &str) -> PathBuf {
    compiled_dir(library).join(format!("{}.json", schema_id))
}

/// Write the compiled catalogue to `compiled/<schema_id>.json` atomically and
/// refresh `compiled/index.json`. Does NOT activate it.
pub fn persist_compiled(library: &YangLibrary, catalogue: &CompiledCatalogue) -> Result<()> {
    let dir = compiled_dir(library);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create '{}'", dir.display()))?;
    let target = bundle_path(library, &catalogue.schema_id);
    let tmp = dir.join(format!(".{}.tmp", catalogue.schema_id));
    let raw = serde_json::to_vec(catalogue).context("failed to serialize compiled catalogue")?;
    if raw.len() as u64 > MAX_COMPILED_BUNDLE_BYTES {
        bail!("compiled catalogue exceeds size limit");
    }
    std::fs::write(&tmp, &raw)
        .with_context(|| format!("failed to write compiled bundle '{}'", tmp.display()))?;
    std::fs::rename(&tmp, &target).with_context(|| {
        format!(
            "failed to activate compiled bundle write '{}'",
            target.display()
        )
    })?;
    refresh_compiled_index(library)?;
    Ok(())
}

fn refresh_compiled_index(library: &YangLibrary) -> Result<()> {
    let dir = compiled_dir(library);
    let active = load_active(library).ok().flatten();
    let mut infos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some(ACTIVE_POINTER)
                || path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.') || n == INDEX_FILE)
            {
                continue;
            }
            let schema_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            // Read header fields cheaply by loading the whole file; bundles
            // are bounded by MAX_COMPILED_BUNDLE_BYTES.
            let (module_count, path_count, diagnostics, compiled_at_ns) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .map(|v| {
                    (
                        v.get("module_count").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                        v.get("path_count").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                        v.get("diagnostics")
                            .and_then(|x| x.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0),
                        v.get("compiled_at_ns")
                            .and_then(|x| x.as_i64())
                            .unwrap_or(0),
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            infos.push(CompiledBundleInfo {
                schema_id,
                compiled_at_ns,
                module_count,
                path_count,
                diagnostics,
                active: active.as_deref()
                    == Some(
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default(),
                    ),
            });
        }
    }
    infos.sort_by(|a, b| b.compiled_at_ns.cmp(&a.compiled_at_ns));
    // Bounded retention: keep newest MAX_COMPILED_BUNDLES, evict the rest —
    // but never evict the active bundle.
    let active_id = active.clone();
    let mut kept = 0usize;
    for info in &infos {
        let is_active = Some(info.schema_id.as_str()) == active_id.as_deref();
        if kept >= MAX_COMPILED_BUNDLES && !is_active {
            let _ = std::fs::remove_file(bundle_path(library, &info.schema_id));
            continue;
        }
        kept += 1;
    }
    // Rebuild infos after eviction.
    let mut final_infos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) != Some("json")
                || name == ACTIVE_POINTER
                || name == INDEX_FILE
                || name.starts_with('.')
            {
                continue;
            }
            let schema_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes)
            {
                final_infos.push(CompiledBundleInfo {
                    schema_id: schema_id.clone(),
                    compiled_at_ns: v
                        .get("compiled_at_ns")
                        .and_then(|x| x.as_i64())
                        .unwrap_or(0),
                    module_count: v.get("module_count").and_then(|x| x.as_u64()).unwrap_or(0)
                        as usize,
                    path_count: v.get("path_count").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                    diagnostics: v
                        .get("diagnostics")
                        .and_then(|x| x.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0),
                    active: active_id.as_deref() == Some(schema_id.as_str()),
                });
            }
        }
    }
    final_infos.sort_by(|a, b| b.compiled_at_ns.cmp(&a.compiled_at_ns));
    let index_path = dir.join(INDEX_FILE);
    let tmp = dir.join(format!(".{}.tmp", INDEX_FILE));
    let raw = serde_json::to_vec_pretty(&final_infos).context("serialize compiled index")?;
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, &index_path)?;
    Ok(())
}

fn active_pointer_path(library: &YangLibrary) -> PathBuf {
    compiled_dir(library).join(ACTIVE_POINTER)
}

fn load_active(library: &YangLibrary) -> Result<Option<String>> {
    let path = active_pointer_path(library);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let pointer: ActivePointer = serde_json::from_str(&raw).context("invalid active pointer")?;
    Ok(Some(pointer.schema_id))
}

/// Atomically point `active.json` at an existing compiled bundle.
/// Rollback = activate an older schema_id. Fails if the bundle is absent.
pub fn activate_compiled(library: &YangLibrary, schema_id: &str) -> Result<()> {
    let path = bundle_path(library, schema_id);
    if !path.exists() {
        bail!("compiled bundle '{}' not found", schema_id);
    }
    let dir = compiled_dir(library);
    std::fs::create_dir_all(&dir)?;
    let pointer = ActivePointer {
        schema_id: schema_id.to_string(),
    };
    let raw = serde_json::to_vec(&pointer)?;
    let tmp = dir.join(".active.tmp");
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, active_pointer_path(library))?;
    refresh_compiled_index(library)?;
    Ok(())
}

/// Load the active compiled catalogue, if any.
pub fn load_active_catalogue(library: &YangLibrary) -> Result<Option<CompiledCatalogue>> {
    let Some(schema_id) = load_active(library)? else {
        return Ok(None);
    };
    let path = bundle_path(library, &schema_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&path)
        .with_context(|| format!("failed to read compiled bundle '{}'", path.display()))?;
    let catalogue: CompiledCatalogue =
        serde_json::from_slice(&raw).context("invalid compiled catalogue JSON")?;
    Ok(Some(catalogue))
}

/// List retained compiled bundles (newest first).
pub fn list_compiled(library: &YangLibrary) -> Result<Vec<CompiledBundleInfo>> {
    refresh_compiled_index(library)?;
    let index_path = compiled_dir(library).join(INDEX_FILE);
    if !index_path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&index_path)?;
    serde_json::from_str(&raw).context("invalid compiled index")
}

/// Schema-path search over the active compiled catalogue.
/// `query` matches module, namespace, path and description substrings;
/// `module_filter` restricts to one module name; `config_only`/`state_only`
/// filter on the effective config flag.
#[derive(Clone, Debug, Default)]
pub struct SchemaPathQuery {
    pub query: String,
    pub module: Option<String>,
    pub kind: Option<String>,
    pub config: Option<bool>,
    pub required_feature: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SchemaPathSearchResult {
    pub schema_id: Option<String>,
    pub total: usize,
    pub paths: Vec<SchemaPathEntry>,
}

pub fn search_active_paths(
    library: &YangLibrary,
    query: &SchemaPathQuery,
) -> Result<SchemaPathSearchResult> {
    let Some(catalogue) = load_active_catalogue(library)? else {
        return Ok(SchemaPathSearchResult {
            schema_id: None,
            total: 0,
            paths: Vec::new(),
        });
    };
    let q = query.query.trim().to_ascii_lowercase();
    let mut out: Vec<SchemaPathEntry> = catalogue
        .paths
        .iter()
        .filter(|e| {
            if let Some(m) = &query.module
                && e.module != *m
            {
                return false;
            }
            if let Some(k) = &query.kind
                && e.kind != *k
            {
                return false;
            }
            if let Some(c) = query.config
                && e.config != c
            {
                return false;
            }
            if let Some(f) = &query.required_feature
                && !e
                    .required_features
                    .iter()
                    .any(|rf| rf == f || rf.ends_with(&format!(":{}", f)))
            {
                return false;
            }
            if q.is_empty() {
                return true;
            }
            e.path.to_ascii_lowercase().contains(&q)
                || e.module.to_ascii_lowercase().contains(&q)
                || e.namespace.to_ascii_lowercase().contains(&q)
                || e.description
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&q)
                || e.type_name
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&q)
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.module.cmp(&b.module)));
    let total = out.len();
    if query.limit > 0 {
        out.truncate(query.limit);
    }
    Ok(SchemaPathSearchResult {
        schema_id: Some(catalogue.schema_id),
        total,
        paths: out,
    })
}

/// Load a specific compiled bundle by schema id (for inspection/rollback
/// preview without activating).
pub fn load_compiled(library: &YangLibrary, schema_id: &str) -> Result<CompiledCatalogue> {
    let path = bundle_path(library, schema_id);
    if !path.exists() {
        bail!("compiled bundle '{}' not found", schema_id);
    }
    let raw = std::fs::read(&path)?;
    serde_json::from_slice(&raw).context("invalid compiled catalogue JSON")
}

/// Convenience: parse + compile directly from an in-memory module set
/// (used by tests and offline tooling).
pub fn compile_parsed(inputs: Vec<ModuleInput>, options: &CompileOptions) -> CompiledCatalogue {
    compile(inputs, options)
}

/// Feature helpers exposed for callers resolving applicability.
pub fn collect_module_features(catalogue: &CompiledCatalogue) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for m in &catalogue.modules {
        if !m.prefix.is_empty() {
            for f in &m.features {
                out.insert(format!("{}:{}", m.prefix, f.name));
            }
        }
        for f in &m.features {
            out.insert(f.name.clone());
        }
    }
    out
}

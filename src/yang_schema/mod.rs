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

pub mod acquisition;
pub mod applicability;
mod compiler;
mod parser;

pub use applicability::{
    APPLICABILITY_RESULT_VERSION, AdvertisedModel, ApplicabilityInput, ApplicabilityResult,
    ApplicabilityStatus, CapabilityEvidence, DeviceBinding, DeviceModuleEvidence,
    MAX_MODULE_RESULTS, MAX_PATH_RESULTS, ModuleClassification, ModuleEvidenceSource, ModuleResult,
    PathResult, ReasonCode, YangLibraryDatastore, YangLibraryEvidence, YangLibraryModule,
    YangLibraryModuleSet, YangLibraryOutcome, YangLibrarySchema, YangLibraryTree,
    resolve_applicability,
};
pub use compiler::FeatureTruth;
pub use compiler::{
    CompileOptions, CompiledCatalogue, CompiledFeature, CompiledModule, ModuleInput,
    SchemaPathEntry, compile, compile_with_diagnostics, eval_feature_expr_tristate,
    filter_by_features,
};
pub use parser::{Module, parse_module};

use std::collections::BTreeSet;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
            Ok(module) => {
                // C4: the stored metadata must agree with the parsed
                // declaration — a blob whose name/revision was mislabeled at
                // import time must not silently index under the wrong key.
                let declared_name = module.name.clone();
                let declared_rev = module.latest_revision();
                // Revision is enforced only when the record carries a real
                // revision — "unknown" means the line-based metadata parser
                // could not determine one (e.g. single-line declarations),
                // which is not evidence of mislabeling.
                let rev_mismatch = !record.revision.is_empty()
                    && record.revision != "unknown"
                    && declared_rev != record.revision;
                if declared_name != record.module_name || rev_mismatch {
                    diagnostics.push(format!(
                        "module record '{}'@'{}' content declares '{}'@'{}' — record/content mismatch",
                        record.module_name, record.revision, declared_name, declared_rev
                    ));
                    continue;
                }
                // Verify blob integrity before compilation.
                use sha2::{Digest, Sha256};
                let digest = hex::encode(Sha256::digest(raw.as_bytes()));
                if !record.checksum_sha256.is_empty() && digest != record.checksum_sha256 {
                    diagnostics.push(format!(
                        "module '{}@{}' checksum mismatch: stored record claims {} but content hashes to {}",
                        record.module_name, record.revision, record.checksum_sha256, digest
                    ));
                    continue;
                }
                inputs.push(ModuleInput {
                    module,
                    vendor_scope: record.vendor_scope.clone(),
                    source_kind: record.source_kind.clone(),
                    source_ref: record.source_ref.clone(),
                    trust: record.trust.clone(),
                    checksum_sha256: record.checksum_sha256.clone(),
                    approved: record.approved,
                    license: record.license.clone(),
                    platforms: record.platforms.clone(),
                    pinned_ref: record.pinned_ref.clone(),
                    source_id: record.source_id.clone(),
                })
            }
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
    let (inputs, pre_diagnostics) = load_module_inputs(library, !trusted_only)?;
    let options = CompileOptions { trusted_only };
    // Pre-compilation diagnostics (unreadable/mislabeled/corrupt records)
    // participate in status and schema identity — they can never be
    // silently downgraded to `complete`.
    let catalogue = compile_with_diagnostics(inputs, &options, pre_diagnostics);
    persist_compiled(library, &catalogue)?;
    Ok(catalogue)
}

fn compiled_dir(library: &YangLibrary) -> PathBuf {
    library.root().join(COMPILED_DIR)
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Outcome of an atomic no-replace publication attempt.
pub(crate) enum PublishOutcome {
    /// This call created the destination.
    Published,
    /// The destination already existed; the existing bytes are untouched.
    Exists,
}

/// Write `bytes` to a unique tmp file in `dir` (pid + process-wide
/// counter), fsync it, and return the tmp path. The caller decides how to
/// publish; tmp cleanup is the caller's responsibility on every path.
fn write_tmp_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write;
    let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{}.{}.{}.tmp", name, std::process::id(), seq));
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("failed to create tmp '{}'", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// Write `bytes` to `dir/name` atomically: a unique tmp file (pid +
/// process-wide counter — pid alone is NOT unique across threads), fsync,
/// then rename over the target. Concurrent writers can never share a tmp
/// path, so a torn write can never be published.
pub(crate) fn write_file_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = write_tmp_file(dir, name, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, dir.join(name)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e)
            .with_context(|| format!("failed to atomically replace '{}/{}'", dir.display(), name));
    }
    Ok(())
}

/// Write `bytes` to `dir/name` only if `name` does not already exist —
/// atomic no-replace publication. A check-then-rename sequence is NOT
/// first-writer-wins across processes: two writers can both observe an
/// absent destination and rename over each other. `hard_link` on the
/// same-directory tmp→target is atomic and fails `AlreadyExists`, which
/// enforces first-writer-wins; the loser then verifies the winner's
/// content. Returns `Exists` without touching the destination.
pub(crate) fn write_file_atomic_noreplace(
    dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<PublishOutcome> {
    let tmp = write_tmp_file(dir, name, bytes)?;
    let target = dir.join(name);
    match std::fs::hard_link(&tmp, &target) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(PublishOutcome::Published)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            Ok(PublishOutcome::Exists)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e).with_context(|| format!("failed to publish '{}/{}'", dir.display(), name))
        }
    }
}

fn bundle_path(library: &YangLibrary, schema_id: &str) -> PathBuf {
    compiled_dir(library).join(format!("{}.json", schema_id))
}

/// Strict schema-id validation. Ids are generated as `ysc-<32 lowercase
/// hex>`; anything else (including path separators, `..`, absolute paths)
/// is rejected before it can reach a filesystem join.
fn valid_schema_id(schema_id: &str) -> bool {
    let Some(hex) = schema_id.strip_prefix("ysc-") else {
        return false;
    };
    hex.len() == 32
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Why bundle integrity could not be established. Retention must
/// distinguish proven-invalid content (safe to evict) from unreadable
/// files (integrity unknown — fail closed, preserve the bytes).
enum VerifyFailure {
    /// Content is definitively not a valid bundle for this identity:
    /// oversized, unparseable, identity-mismatched or hash-mismatched.
    Invalid(anyhow::Error),
    /// The file could not be stat'ed or read — integrity is unknown.
    Unreadable(anyhow::Error),
}

/// Deserialize + integrity-check a compiled bundle: the embedded
/// `schema_id` must equal the requested id AND recompute identically from
/// the bundle's semantic content. Classifies failures so callers can
/// fail closed differently for corrupt content vs transient IO errors.
fn verify_bundle(
    library: &YangLibrary,
    schema_id: &str,
) -> std::result::Result<CompiledCatalogue, VerifyFailure> {
    if !valid_schema_id(schema_id) {
        return Err(VerifyFailure::Invalid(anyhow::anyhow!(
            "invalid compiled schema id '{}'",
            schema_id
        )));
    }
    let path = bundle_path(library, schema_id);
    // Bound the read: the write path enforces MAX_COMPILED_BUNDLE_BYTES,
    // but a corrupt or hostile file placed in compiled/ must not be
    // materialized into memory unbounded.
    let len = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) => {
            return Err(VerifyFailure::Unreadable(anyhow::Error::new(e).context(
                format!("failed to stat compiled bundle '{}'", path.display()),
            )));
        }
    };
    if len > MAX_COMPILED_BUNDLE_BYTES {
        return Err(VerifyFailure::Invalid(anyhow::anyhow!(
            "compiled bundle '{}' exceeds size limit ({} > {} bytes)",
            schema_id,
            len,
            MAX_COMPILED_BUNDLE_BYTES
        )));
    }
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) => {
            return Err(VerifyFailure::Unreadable(anyhow::Error::new(e).context(
                format!("failed to read compiled bundle '{}'", path.display()),
            )));
        }
    };
    let catalogue: CompiledCatalogue = match serde_json::from_slice(&raw) {
        Ok(c) => c,
        Err(e) => {
            return Err(VerifyFailure::Invalid(
                anyhow::Error::new(e).context("invalid compiled catalogue JSON"),
            ));
        }
    };
    if catalogue.schema_id != schema_id {
        return Err(VerifyFailure::Invalid(anyhow::anyhow!(
            "compiled bundle '{}' embeds schema_id '{}' — identity mismatch",
            schema_id,
            catalogue.schema_id
        )));
    }
    let recomputed = compiler::recompute_schema_id(&catalogue);
    if recomputed != schema_id {
        return Err(VerifyFailure::Invalid(anyhow::anyhow!(
            "compiled bundle '{}' fails integrity check: content hashes to '{}'",
            schema_id,
            recomputed
        )));
    }
    Ok(catalogue)
}

/// Deserialize + integrity-check a compiled bundle, failing closed on
/// either proven-invalid content or unreadable files.
fn read_verified_bundle(library: &YangLibrary, schema_id: &str) -> Result<CompiledCatalogue> {
    verify_bundle(library, schema_id).map_err(|e| match e {
        VerifyFailure::Invalid(e) | VerifyFailure::Unreadable(e) => e,
    })
}

/// Write the compiled catalogue to `compiled/<schema_id>.json` atomically and
/// refresh `compiled/index.json`. Does NOT activate it.
pub fn persist_compiled(library: &YangLibrary, catalogue: &CompiledCatalogue) -> Result<()> {
    if !valid_schema_id(&catalogue.schema_id) {
        bail!("refusing to persist bundle with invalid schema id");
    }
    // Identity must match content — never store a bundle under a lying id.
    let recomputed = compiler::recompute_schema_id(catalogue);
    if recomputed != catalogue.schema_id {
        bail!(
            "compiled catalogue identity mismatch: schema_id '{}' but content hashes to '{}'",
            catalogue.schema_id,
            recomputed
        );
    }
    let dir = compiled_dir(library);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create '{}'", dir.display()))?;
    let target = bundle_path(library, &catalogue.schema_id);
    let raw = serde_json::to_vec(catalogue).context("failed to serialize compiled catalogue")?;
    if raw.len() as u64 > MAX_COMPILED_BUNDLE_BYTES {
        bail!("compiled catalogue exceeds size limit");
    }
    // Bundles are immutable and content-addressed: an existing file under
    // this identity must verify (embedded id + recomputed hash). A second
    // compile of identical input yields the same schema_id but a different
    // `compiled_at_ns`, so byte-equality is NOT the invariant — verified
    // semantic identity is. Publication is first-writer-wins via atomic
    // no-replace link: a check-then-rename would let two concurrent
    // processes both observe an absent destination and rename over each
    // other, silently replacing already-published bytes.
    if target.exists() {
        match read_verified_bundle(library, &catalogue.schema_id) {
            Ok(_) => {
                refresh_compiled_index(library)?;
                return Ok(());
            }
            Err(e) => bail!(
                "compiled bundle '{}' exists but fails integrity verification ({}); refusing to overwrite",
                catalogue.schema_id,
                e
            ),
        }
    }
    match write_file_atomic_noreplace(&dir, &format!("{}.json", catalogue.schema_id), &raw)? {
        PublishOutcome::Published => {}
        // Lost a cross-process publication race: the winner's bytes are
        // already durable. Verify them — identical schema_id implies
        // identical semantics, so a verified winner is an idempotent
        // success; a corrupt winner fails closed.
        PublishOutcome::Exists => match read_verified_bundle(library, &catalogue.schema_id) {
            Ok(_) => {}
            Err(e) => bail!(
                "compiled bundle '{}' was concurrently published but fails integrity verification ({}); refusing to overwrite",
                catalogue.schema_id,
                e
            ),
        },
    }
    refresh_compiled_index(library)?;
    Ok(())
}

fn refresh_compiled_index(library: &YangLibrary) -> Result<()> {
    let dir = compiled_dir(library);
    // Fail closed: an unreadable or malformed active pointer must never be
    // treated as proof that no active bundle exists — destructive retention
    // below relies on this identity to protect the live catalogue.
    let active = load_active(library)?;
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
            // Streamed header parse: serde skips unknown fields without
            // building a DOM and `from_reader` consumes the file
            // incrementally, so indexing a 512 MiB bundle costs bounded
            // memory (CPU is still O(file size) — documented limit).
            let header = std::fs::File::open(&path)
                .ok()
                .and_then(|f| serde_json::from_reader::<_, BundleHeader>(BufReader::new(f)).ok());
            let (module_count, path_count, diagnostics, compiled_at_ns) = header
                .as_ref()
                .filter(|h| h.schema_id == schema_id)
                .map(|h| {
                    (
                        h.module_count,
                        h.path_count,
                        h.diagnostics.len(),
                        h.compiled_at_ns,
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
    infos.sort_by_key(|i| std::cmp::Reverse(i.compiled_at_ns));
    // Bounded retention: keep newest MAX_COMPILED_BUNDLES, evict the rest —
    // but never evict the active bundle. Eviction is destructive, so the
    // ordering must not rest on unverified metadata: a forged bundle can
    // carry a valid `ysc-` filename, a matching embedded schema_id and a
    // fabricated `compiled_at_ns` (e.g. i64::MAX) while its semantic
    // content is invalid. A header-consistent embedded id is NOT proof of
    // integrity — only a recomputed content hash is.
    //
    // Verification is therefore lazy and bounded: it runs only when the
    // bundle count actually exceeds the retention bound (steady-state
    // refreshes never materialize bundle bodies), walks candidates in
    // claimed-newest order, and stops verifying once the retained set is
    // full — bundles beyond the cutoff are evicted on ordering alone
    // because 32 integrity-verified newer bundles already justify it.
    let active_id = active.clone();
    if infos.len() > MAX_COMPILED_BUNDLES {
        let mut kept = 0usize;
        let mut preserved_unreadable = 0usize;
        for info in &infos {
            let is_active = Some(info.schema_id.as_str()) == active_id.as_deref();
            if is_active {
                kept += 1;
                continue;
            }
            // Every eviction candidate is verified before deletion —
            // including files beyond the retention cutoff. Ordering alone
            // is untrusted metadata, and a file that cannot be read has
            // unknown integrity: failure to verify is not permission to
            // delete. Verified bundles past the bound and proven-invalid
            // bundles are evicted; unreadable bundles are preserved and
            // reported so the bound can be temporarily exceeded.
            match verify_bundle(library, &info.schema_id) {
                Ok(_) => {
                    if kept >= MAX_COMPILED_BUNDLES {
                        let _ = std::fs::remove_file(bundle_path(library, &info.schema_id));
                    } else {
                        kept += 1;
                    }
                }
                // Proven-invalid bundles are evicted immediately: every
                // read path rejects them, so they must not occupy a
                // retention slot or outrank legitimate bundles.
                Err(VerifyFailure::Invalid(_)) => {
                    let _ = std::fs::remove_file(bundle_path(library, &info.schema_id));
                }
                // Integrity cannot be established (transient IO): fail
                // closed — preserve the file but grant it no trusted
                // retention slot.
                Err(VerifyFailure::Unreadable(e)) => {
                    preserved_unreadable += 1;
                    tracing::warn!(
                        schema_id = %info.schema_id,
                        error = %e,
                        "compiled bundle unreadable; preserved without a retention slot"
                    );
                }
            }
        }
        if preserved_unreadable > 0 {
            tracing::warn!(
                preserved = preserved_unreadable,
                bound = MAX_COMPILED_BUNDLES,
                "compiled bundle retention preserved unreadable bundles; \
                 on-disk bundle count may temporarily exceed the bound"
            );
        }
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
            if let Ok(f) = std::fs::File::open(&path)
                && let Ok(h) = serde_json::from_reader::<_, BundleHeader>(BufReader::new(f))
                // Retention ordering must not trust unverified metadata:
                // a foreign or content-swapped bundle whose embedded
                // schema_id does not match its filename sorts as epoch 0
                // and is evicted before any verified bundle.
                && h.schema_id == schema_id
            {
                final_infos.push(CompiledBundleInfo {
                    schema_id: schema_id.clone(),
                    compiled_at_ns: h.compiled_at_ns,
                    module_count: h.module_count,
                    path_count: h.path_count,
                    diagnostics: h.diagnostics.len(),
                    active: active_id.as_deref() == Some(schema_id.as_str()),
                });
            }
        }
    }
    final_infos.sort_by_key(|i| std::cmp::Reverse(i.compiled_at_ns));
    let raw = serde_json::to_vec_pretty(&final_infos).context("serialize compiled index")?;
    write_file_atomic(&dir, INDEX_FILE, &raw)
}

/// Header-only view of a compiled bundle for index refresh — unknown
/// fields are skipped by serde without materializing the full document.
#[derive(Deserialize)]
struct BundleHeader {
    /// Embedded identity — must equal the bundle's filename stem for the
    /// header to be trusted for retention/index metadata.
    #[serde(default)]
    schema_id: String,
    #[serde(default)]
    module_count: usize,
    #[serde(default)]
    path_count: usize,
    #[serde(default)]
    compiled_at_ns: i64,
    #[serde(default)]
    diagnostics: Vec<serde::de::IgnoredAny>,
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
    // A syntactically valid pointer naming a non-bundle id would silently
    // unprotect every real bundle during retention — reject it as malformed.
    if !valid_schema_id(&pointer.schema_id) {
        bail!(
            "active pointer references invalid schema id '{}'",
            pointer.schema_id
        );
    }
    Ok(Some(pointer.schema_id))
}

/// Atomically point `active.json` at an existing compiled bundle.
/// Rollback = activate an older schema_id. Fails if the bundle is absent.
pub fn activate_compiled(library: &YangLibrary, schema_id: &str) -> Result<()> {
    // Verify the bundle exists AND passes integrity checks BEFORE moving
    // the pointer — a corrupt or mismatched bundle can never become active,
    // and a failed activation leaves the previous catalogue untouched.
    let _verified = read_verified_bundle(library, schema_id)?;
    let dir = compiled_dir(library);
    std::fs::create_dir_all(&dir)?;
    let pointer = ActivePointer {
        schema_id: schema_id.to_string(),
    };
    let raw = serde_json::to_vec(&pointer)?;
    write_file_atomic(&dir, ACTIVE_POINTER, &raw)?;
    refresh_compiled_index(library)?;
    Ok(())
}

/// Load the active compiled catalogue, if any. A corrupt or tampered
/// active bundle fails closed (error), never silently returns partial data.
pub fn load_active_catalogue(library: &YangLibrary) -> Result<Option<CompiledCatalogue>> {
    let Some(schema_id) = load_active(library)? else {
        return Ok(None);
    };
    read_verified_bundle(library, &schema_id).map(Some)
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
    read_verified_bundle(library, schema_id)
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

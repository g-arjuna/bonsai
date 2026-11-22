//! Effective-schema compiler: resolves imports, includes, uses/groupings,
//! augments, deviations and features into a deterministic compiled catalogue
//! plus a canonical schema-path index.
//!
//! Resolution is staged and bounded: unresolvable imports, includes, uses
//! targets or typedefs are recorded as diagnostics and the affected subtree
//! is kept with raw type names — never silently dropped, never fabricated.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::parser::{Module, Node, Stmt, TypeSpec};

pub const COMPILED_SCHEMA_VERSION: u32 = 1;
/// Bound on resolution passes for nested augments.
const MAX_RESOLUTION_PASSES: usize = 8;
/// Bound on uses-expansion depth (grouping cycles).
const MAX_USES_DEPTH: usize = 32;
/// Bound on path-index entries emitted per compile.
pub const MAX_PATH_ENTRIES: usize = 500_000;

// ---------------------------------------------------------------------------
// Output model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledModule {
    pub name: String,
    pub revision: String,
    pub namespace: String,
    pub prefix: String,
    pub organization: String,
    pub description: String,
    pub yang_version: String,
    pub is_submodule: bool,
    pub belong_to: Option<String>,
    pub vendor_scope: String,
    pub source_kind: String,
    pub source_ref: String,
    pub trust: String,
    pub checksum_sha256: String,
    /// Operator approval marker for same name+revision content conflicts.
    /// At most one record per conflict group should carry it; selection
    /// prefers approved content over merely newest imports.
    #[serde(default)]
    pub approved: bool,
    /// License/provenance carried from the source manifest so downstream
    /// applicability resolution never has to infer platform from names.
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub pinned_ref: String,
    #[serde(default)]
    pub source_id: String,
    pub resolution: String, // "complete" | "partial"
    pub features: Vec<CompiledFeature>,
    pub node_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledFeature {
    pub name: String,
    pub description: String,
    pub if_features: Vec<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaPathEntry {
    /// Module that declares this node (augment provenance for augmented nodes).
    pub module: String,
    pub module_revision: String,
    pub namespace: String,
    /// Module prefix — the closest thing YANG has to a gNMI origin hint.
    /// Devices may use a different origin; this is provenance, not a promise.
    pub module_prefix: String,
    /// Canonical schema path: "/seg/seg[key1][key2]/leaf". Choice and case
    /// names are NOT path segments (they are not instance-data nodes).
    pub path: String,
    pub kind: String, // container|list|leaf|leaf-list|anyxml|anydata
    /// Effective config flag (inherited; YANG default true).
    pub config: bool,
    /// Declared `config` on this node, if any.
    pub declared_config: Option<bool>,
    pub mandatory: bool,
    pub presence: Option<String>,
    pub type_name: Option<String>,
    /// Resolved base type after typedef unwrapping (builtin name, or the
    /// typedef-qualified name when unresolved).
    pub resolved_type: Option<String>,
    pub units: Option<String>,
    pub default: Option<String>,
    pub description: Option<String>,
    pub keys: Vec<String>,
    pub min_elements: Option<u64>,
    pub max_elements: Option<u64>,
    pub ordered_by: Option<String>,
    /// if-feature requirements as written (may be prefix:feature or bare).
    pub required_features: Vec<String>,
    pub deviated: bool,
    pub deviation_notes: Vec<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompiledCatalogue {
    pub schema_version: u32,
    /// Deterministic content identity over the input module set.
    pub schema_id: String,
    pub compiled_at_ns: i64,
    pub module_count: usize,
    pub path_count: usize,
    /// Effective-schema status:
    /// - "complete": every parsed construct resolved; zero diagnostics.
    /// - "partial": usable schema emitted, but diagnostics record unresolved
    ///   imports, uses targets, types or unsupported constructs.
    /// - "failed": no modules could be compiled.
    /// - "unknown": persisted bundle predating this field.
    #[serde(default = "unknown_status")]
    pub status: String,
    pub modules: Vec<CompiledModule>,
    pub paths: Vec<SchemaPathEntry>,
    pub diagnostics: Vec<String>,
    /// Operator-approved module-revision pins this bundle was compiled
    /// under (M6F-F). Provenance only — deliberately excluded from
    /// `schema_id`, which remains pure effective-content identity so
    /// identical closures deduplicate to one bundle.
    #[serde(default)]
    pub module_selection: BTreeMap<String, String>,
}

fn unknown_status() -> String {
    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

struct ModuleCtx {
    module: Module,
    /// prefix -> imported module name
    imports: BTreeMap<String, String>,
    /// typedef name -> TypeSpec (module + included submodules)
    typedefs: BTreeMap<String, TypeSpec>,
    /// grouping name -> children stmts
    groupings: BTreeMap<String, Vec<Stmt>>,
    /// feature name (unqualified) -> Feature
    features: BTreeMap<String, super::parser::Feature>,
    /// identity name -> base
    identities: BTreeMap<String, String>,
    provenance: Provenance,
}

#[derive(Clone, Default)]
struct Provenance {
    vendor_scope: String,
    source_kind: String,
    source_ref: String,
    trust: String,
    checksum_sha256: String,
    approved: bool,
    license: String,
    platforms: Vec<String>,
    pinned_ref: String,
    source_id: String,
}

#[derive(Clone)]
struct ResolvedNode {
    name: String,
    kind: &'static str,
    node: Node,
    children: Vec<ResolvedNode>,
    /// Stmts deferred (uses already expanded; augments applied later).
    provenance_module: String,
    provenance_prefix: String,
    provenance_revision: String,
    provenance_namespace: String,
    required_features: Vec<String>,
    deviated: bool,
    deviation_notes: Vec<String>,
    /// `deviate not-supported` removes the node (and its subtree) from the
    /// effective schema — the declaration exists but the node is not part
    /// of the compiled data tree.
    pruned: bool,
}

impl ResolvedNode {
    fn segment(&self) -> String {
        match self.kind {
            "list" => {
                if self.node.keys.is_empty() {
                    format!("{}[]", self.name)
                } else {
                    let keys = self
                        .node
                        .keys
                        .iter()
                        .map(|k| format!("[{}]", k))
                        .collect::<String>();
                    format!("{}{}", self.name, keys)
                }
            }
            _ => self.name.clone(),
        }
    }
}

/// Input for one module: parsed AST plus provenance metadata.
pub struct ModuleInput {
    pub module: Module,
    pub vendor_scope: String,
    pub source_kind: String,
    pub source_ref: String,
    pub trust: String,
    pub checksum_sha256: String,
    /// Operator approval marker for content-conflict selection.
    pub approved: bool,
    pub license: String,
    pub platforms: Vec<String>,
    pub pinned_ref: String,
    pub source_id: String,
}

pub struct CompileOptions {
    /// When true, modules whose trust != "trusted" are excluded.
    pub trusted_only: bool,
    /// Explicit module-revision selection (M6F-F): module name -> exact
    /// revision date the operator approved for this compilation. A pinned
    /// name keeps ONLY inputs whose module declares that revision; a pin
    /// with no matching input is an explicit diagnostic and the module is
    /// absent — never silently substituted with a different revision.
    /// Unpinned names keep the default newest-revision policy.
    pub module_revisions: BTreeMap<String, String>,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            trusted_only: true,
            module_revisions: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Compile driver
// ---------------------------------------------------------------------------

pub fn compile(inputs: Vec<ModuleInput>, options: &CompileOptions) -> CompiledCatalogue {
    compile_inner(inputs, options, Vec::new())
}

/// Compile with caller-supplied pre-diagnostics (e.g. unreadable or
/// unparseable module records from `load_module_inputs`). Every diagnostic
/// participates in the final status and schema identity — a catalogue can
/// never report `complete` while failures are recorded.
pub fn compile_with_diagnostics(
    inputs: Vec<ModuleInput>,
    options: &CompileOptions,
    pre_diagnostics: Vec<String>,
) -> CompiledCatalogue {
    compile_inner(inputs, options, pre_diagnostics)
}

fn compile_inner(
    inputs: Vec<ModuleInput>,
    options: &CompileOptions,
    mut diagnostics: Vec<String>,
) -> CompiledCatalogue {
    // Group parsed modules by name; keep highest revision per name.
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut kept: Vec<Option<ModuleInput>> = Vec::new();
    for (idx, input) in inputs.into_iter().enumerate() {
        if options.trusted_only && input.trust != "trusted" {
            kept.push(None);
            continue;
        }
        by_name
            .entry(input.module.name.clone())
            .or_default()
            .push(idx);
        kept.push(Some(input));
    }

    // Explicit module-revision pins (M6F-F). A pinned name keeps ONLY
    // inputs whose module declares exactly the pinned revision; pins are
    // applied before the default newest-revision policy and never
    // substitute a different revision — an unresolvable pin is an explicit
    // diagnostic and the module is absent from the compiled set, so
    // dependent imports surface as missing-import diagnostics and the
    // catalogue stays non-authoritative.
    let mut pinned_names: BTreeSet<String> = BTreeSet::new();
    let mut drop_idx = BTreeSet::new();
    for (name, revision) in &options.module_revisions {
        pinned_names.insert(name.clone());
        if name.trim().is_empty() || revision.trim().is_empty() {
            diagnostics.push(
                "invalid module-revision pin: module name and revision must be non-empty"
                    .to_string(),
            );
            continue;
        }
        let Some(idxs) = by_name.get(name) else {
            diagnostics.push(format!(
                "module '{}' pinned to revision '{}' is not present in the compiled set",
                name, revision
            ));
            continue;
        };
        let matching: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|i| {
                kept[*i]
                    .as_ref()
                    .is_some_and(|input| input.module.latest_revision() == *revision)
            })
            .collect();
        if matching.is_empty() {
            let available = idxs
                .iter()
                .filter_map(|i| kept[*i].as_ref())
                .map(|input| input.module.latest_revision())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(format!(
                "module '{}' pinned to revision '{}' but the compiled set provides revision(s) [{}]",
                name, revision, available
            ));
            for &idx in idxs {
                drop_idx.insert(idx);
            }
            continue;
        }
        // Same name+revision content conflicts resolve deterministically:
        // lowest checksum, then lowest source ref — independent of input
        // order. Every other candidate is dropped with a diagnostic.
        let mut sorted = matching;
        sorted.sort_by(|a, b| {
            let ia = kept[*a].as_ref().unwrap();
            let ib = kept[*b].as_ref().unwrap();
            ia.checksum_sha256
                .cmp(&ib.checksum_sha256)
                .then_with(|| ia.source_ref.cmp(&ib.source_ref))
        });
        for &idx in sorted.iter().skip(1) {
            drop_idx.insert(idx);
            diagnostics.push(format!(
                "module '{}' has conflicting content for pinned revision '{}'; keeping checksum {}, dropped checksum {}",
                name,
                revision,
                kept[sorted[0]].as_ref().unwrap().checksum_sha256,
                kept[idx].as_ref().unwrap().checksum_sha256
            ));
        }
        let kept_idx = sorted[0];
        for &idx in idxs {
            if idx != kept_idx {
                drop_idx.insert(idx);
                if !sorted.contains(&idx) {
                    diagnostics.push(format!(
                        "module '{}' has multiple revisions; pinned '{}', dropped '{}'",
                        name,
                        revision,
                        kept[idx].as_ref().unwrap().module.latest_revision()
                    ));
                }
            }
        }
    }

    // For duplicate module names keep only the newest revision — except
    // pinned names, already resolved above.
    for (name, idxs) in &by_name {
        if pinned_names.contains(name) || idxs.len() <= 1 {
            continue;
        }
        {
            let mut sorted: Vec<usize> = idxs.clone();
            sorted.sort_by(|a, b| {
                let ia = kept[*a].as_ref().unwrap();
                let ib = kept[*b].as_ref().unwrap();
                // Newest revision first; ties broken by content identity so
                // the result is independent of input ordering.
                ib.module
                    .latest_revision()
                    .cmp(&ia.module.latest_revision())
                    .then_with(|| ia.checksum_sha256.cmp(&ib.checksum_sha256))
                    .then_with(|| ia.source_ref.cmp(&ib.source_ref))
            });
            for &idx in sorted.iter().skip(1) {
                drop_idx.insert(idx);
                diagnostics.push(format!(
                    "module '{}' has multiple revisions; keeping {}, dropped {}",
                    kept[idx].as_ref().unwrap().module.name,
                    kept[sorted[0]].as_ref().unwrap().module.latest_revision(),
                    kept[idx].as_ref().unwrap().module.latest_revision()
                ));
            }
        }
    }

    // Build contexts.
    let mut ctxs: BTreeMap<String, ModuleCtx> = BTreeMap::new();
    for (idx, slot) in kept.iter().enumerate() {
        if drop_idx.contains(&idx) {
            continue;
        }
        let Some(input) = slot else { continue };
        ctxs.insert(
            input.module.name.clone(),
            build_ctx(input, &mut diagnostics),
        );
    }

    // Merge included submodules into their parent module context: a
    // submodule's typedefs, groupings, features, identities and data
    // definition statements belong to the parent's effective schema.
    merge_submodules(&mut ctxs, &mut diagnostics);

    // Parser-level diagnostics (skipped unsupported statements, unexpected
    // tokens) are part of compilation truth — never silent.
    for ctx in ctxs.values() {
        for d in ctx.module.diagnostics.iter().take(100) {
            diagnostics.push(format!("module '{}': {}", ctx.module.name, d));
        }
    }

    // Missing imports/includes are explicit diagnostics, never silent.
    for ctx in ctxs.values() {
        if ctx.module.is_submodule {
            continue;
        }
        for import in &ctx.module.imports {
            match ctxs.get(&import.module) {
                None => diagnostics.push(format!(
                    "module '{}' imports '{}', which is not present in the compiled set",
                    ctx.module.name, import.module
                )),
                Some(target) => {
                    if let Some(rev) = &import.revision {
                        let loaded = target.module.latest_revision();
                        if *rev != loaded {
                            diagnostics.push(format!(
                                "module '{}' imports '{}'@'{}' but compiled set provides revision '{}'",
                                ctx.module.name, import.module, rev, loaded
                            ));
                        }
                    }
                }
            }
        }
        for inc in &ctx.module.includes {
            if !ctxs.contains_key(inc) {
                diagnostics.push(format!(
                    "module '{}' includes '{}', which is not present in the compiled set",
                    ctx.module.name, inc
                ));
            }
        }
    }

    // Module name -> namespace/prefix lookup for feature qualification.
    let module_prefix: BTreeMap<String, String> = ctxs
        .iter()
        .map(|(n, c)| (n.clone(), c.module.prefix.clone().unwrap_or_default()))
        .collect();

    // Resolve each module's top-level children.
    let mut roots: BTreeMap<String, Vec<ResolvedNode>> = BTreeMap::new();
    let mut augments: Vec<AugmentWork> = Vec::new();
    for (name, ctx) in &ctxs {
        let mut resolved = Vec::new();
        for stmt in &ctx.module.children {
            match stmt {
                Stmt::Augment(a) => augments.push(AugmentWork {
                    target: a.target.clone(),
                    children: a.children.clone(),
                    module: name.clone(),
                    last_error: None,
                }),
                Stmt::Deviation(_) => {} // handled in pass below
                Stmt::Feature(_)
                | Stmt::Identity(_)
                | Stmt::Typedef(_)
                | Stmt::Grouping(_)
                | Stmt::Other { .. } => {}
                Stmt::Uses(u) => {
                    // Vendor modules commonly expose their schema through a
                    // top-level `uses <module>-top;` — expand it like a
                    // node-level uses so the grouping's data-defs become roots.
                    for rn in expand_uses(u, ctx, &ctxs, 1, &module_prefix, &[], &mut diagnostics) {
                        resolved.push(rn);
                    }
                }
                _ => {
                    if let Some(node) =
                        resolve_stmt(stmt, ctx, &ctxs, 0, &module_prefix, &[], &mut diagnostics)
                    {
                        resolved.push(node);
                    }
                }
            }
        }
        roots.insert(name.clone(), resolved);
    }

    // Collect deviations (module + nested inside augments handled shallowly).
    let mut deviations: Vec<DeviationWork> = Vec::new();
    for (name, ctx) in &ctxs {
        for stmt in &ctx.module.children {
            if let Stmt::Deviation(d) = stmt {
                deviations.push(DeviationWork {
                    target: d.target.clone(),
                    note: d
                        .description
                        .clone()
                        .or(d.reference.clone())
                        .unwrap_or_default(),
                    status: d.status.clone().unwrap_or_default(),
                    module: name.clone(),
                    deviates: d
                        .deviates
                        .iter()
                        .map(|x| (x.kind.clone(), x.has_block))
                        .collect(),
                });
                collect_nested_deviations(d, name.clone(), &mut deviations);
            }
        }
    }

    // Apply augments iteratively (bounded) — later augments may target
    // nodes introduced by earlier ones.
    for _pass in 0..MAX_RESOLUTION_PASSES {
        let mut applied = false;
        let mut pending = Vec::new();
        for mut aw in augments.drain(..) {
            if apply_augment(&mut roots, &ctxs, &mut aw, &module_prefix, &mut diagnostics) {
                applied = true;
            } else {
                pending.push(aw);
            }
        }
        augments = pending;
        if !applied {
            break;
        }
    }
    for aw in &augments {
        diagnostics.push(match &aw.last_error {
            Some(e) => format!(
                "augment target '{}' in module '{}': {}",
                aw.target, aw.module, e
            ),
            None => format!(
                "augment target '{}' in module '{}' could not be resolved",
                aw.target, aw.module
            ),
        });
    }

    // Apply deviations (target resolution failures are diagnostics, never
    // silent — a deviation that cannot be placed must not let the catalogue
    // pretend full knowledge).
    for dw in &deviations {
        apply_deviation(&mut roots, &ctxs, dw, &module_prefix, &mut diagnostics);
    }

    // Feature-gate evaluation helper data is embedded per node; build index.
    let mut paths = Vec::new();
    let mut modules_out = Vec::new();
    let mut saturated = false;
    for (name, ctx) in &ctxs {
        let nodes = roots.get(name).cloned().unwrap_or_default();
        let mut count = 0usize;
        for node in &nodes {
            emit_paths(
                node,
                "",
                true,
                &ctxs,
                &mut paths,
                &mut count,
                &mut saturated,
            );
        }
        let features: Vec<CompiledFeature> = ctx
            .features
            .iter()
            .map(|(n, f)| CompiledFeature {
                name: n.clone(),
                description: f.description.clone().unwrap_or_default(),
                if_features: f.if_features.clone(),
                status: f.status.clone().unwrap_or_default(),
            })
            .collect();
        let quoted = format!("'{}'", name);
        let partial = diagnostics.iter().any(|d| d.contains(&quoted));
        modules_out.push(CompiledModule {
            name: name.clone(),
            revision: ctx.module.latest_revision(),
            namespace: ctx.module.namespace.clone().unwrap_or_default(),
            prefix: ctx.module.prefix.clone().unwrap_or_default(),
            organization: ctx.module.organization.clone().unwrap_or_default(),
            description: ctx.module.description.clone().unwrap_or_default(),
            yang_version: ctx.module.yang_version.clone(),
            is_submodule: ctx.module.is_submodule,
            belong_to: ctx.module.belong_to.clone(),
            vendor_scope: ctx.provenance.vendor_scope.clone(),
            source_kind: ctx.provenance.source_kind.clone(),
            source_ref: ctx.provenance.source_ref.clone(),
            trust: ctx.provenance.trust.clone(),
            checksum_sha256: ctx.provenance.checksum_sha256.clone(),
            approved: ctx.provenance.approved,
            license: ctx.provenance.license.clone(),
            platforms: ctx.provenance.platforms.clone(),
            pinned_ref: ctx.provenance.pinned_ref.clone(),
            source_id: ctx.provenance.source_id.clone(),
            resolution: if partial { "partial" } else { "complete" }.to_string(),
            features,
            node_count: count,
        });
    }
    modules_out.sort_by(|a, b| a.name.cmp(&b.name));
    paths.sort_by(|a, b| {
        a.module
            .cmp(&b.module)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    paths.dedup_by(|a, b| a.module == b.module && a.path == b.path && a.kind == b.kind);

    if saturated {
        diagnostics.push(format!(
            "schema path index saturated at {} entries; catalogue is truncated and must not be treated as complete",
            MAX_PATH_ENTRIES
        ));
    }

    // Status is decided only after EVERY diagnostic source has spoken —
    // including caller-supplied pre-compilation diagnostics.
    let status = if modules_out.is_empty() {
        "failed"
    } else if diagnostics.is_empty() {
        "complete"
    } else {
        "partial"
    }
    .to_string();
    let schema_id = compute_schema_id(&modules_out, &paths, &status, &diagnostics);

    CompiledCatalogue {
        schema_version: COMPILED_SCHEMA_VERSION,
        schema_id,
        compiled_at_ns: crate::graph::common::now_ns(),
        module_count: modules_out.len(),
        path_count: paths.len(),
        status,
        modules: modules_out,
        paths,
        diagnostics,
        module_selection: options.module_revisions.clone(),
    }
}

// ---------------------------------------------------------------------------
// Resolution helpers
// ---------------------------------------------------------------------------

/// Fold each submodule's definitions into the module named by its
/// `belong-to` statement. Orphan submodules are diagnosed and left empty.
fn merge_submodules(ctxs: &mut BTreeMap<String, ModuleCtx>, diagnostics: &mut Vec<String>) {
    let sub_names: Vec<String> = ctxs
        .iter()
        .filter(|(_, c)| c.module.is_submodule)
        .map(|(n, _)| n.clone())
        .collect();
    for sub_name in sub_names {
        let (belong_to, children, typedefs, groupings, features, identities) = {
            let Some(sub) = ctxs.get(&sub_name) else {
                continue;
            };
            (
                sub.module.belong_to.clone().unwrap_or_default(),
                sub.module.children.clone(),
                sub.typedefs.clone(),
                sub.groupings.clone(),
                sub.features.clone(),
                sub.identities.clone(),
            )
        };
        let parent_name = belong_to
            .rsplit(':')
            .next()
            .unwrap_or(&belong_to)
            .to_string();
        if parent_name.is_empty() || parent_name == sub_name {
            diagnostics.push(format!(
                "submodule '{}' has no resolvable belong-to target",
                sub_name
            ));
            continue;
        }
        let Some(parent) = ctxs.get_mut(&parent_name) else {
            diagnostics.push(format!(
                "submodule '{}' belongs to unloaded module '{}'",
                sub_name, parent_name
            ));
            continue;
        };
        // Parent definitions take precedence on name conflicts.
        for (k, v) in typedefs {
            parent.typedefs.entry(k).or_insert(v);
        }
        for (k, v) in groupings {
            parent.groupings.entry(k).or_insert(v);
        }
        for (k, v) in features {
            parent.features.entry(k).or_insert(v);
        }
        for (k, v) in identities {
            parent.identities.entry(k).or_insert(v);
        }
        parent.module.children.extend(children);
        if let Some(sub) = ctxs.get_mut(&sub_name) {
            sub.module.children.clear();
        }
    }
}

fn build_ctx(input: &ModuleInput, diagnostics: &mut Vec<String>) -> ModuleCtx {
    let mut ctx = ModuleCtx {
        module: input.module.clone(),
        imports: BTreeMap::new(),
        typedefs: BTreeMap::new(),
        groupings: BTreeMap::new(),
        features: BTreeMap::new(),
        identities: BTreeMap::new(),
        provenance: Provenance {
            vendor_scope: input.vendor_scope.clone(),
            source_kind: input.source_kind.clone(),
            source_ref: input.source_ref.clone(),
            trust: input.trust.clone(),
            checksum_sha256: input.checksum_sha256.clone(),
            approved: input.approved,
            license: input.license.clone(),
            platforms: input.platforms.clone(),
            pinned_ref: input.pinned_ref.clone(),
            source_id: input.source_id.clone(),
        },
    };
    for import in &input.module.imports {
        if let Some(prefix) = &import.prefix {
            ctx.imports.insert(prefix.clone(), import.module.clone());
        }
    }
    collect_defs(&input.module.children, &mut ctx, diagnostics, 0);
    ctx
}

fn collect_defs(stmts: &[Stmt], ctx: &mut ModuleCtx, _diagnostics: &mut Vec<String>, depth: usize) {
    if depth > MAX_USES_DEPTH {
        return;
    }
    for stmt in stmts {
        match stmt {
            Stmt::Typedef(t) => {
                ctx.typedefs.insert(t.name.clone(), t.type_spec.clone());
            }
            Stmt::Grouping(g) => {
                ctx.groupings.insert(g.name.clone(), g.children.clone());
                // Groupings may nest (and carry typedefs) — collect
                // recursively so nested definitions resolve.
                collect_defs(&g.children, ctx, _diagnostics, depth + 1);
            }
            Stmt::Feature(f) => {
                ctx.features.insert(f.name.clone(), f.clone());
            }
            Stmt::Identity(i) => {
                if let Some(base) = &i.base {
                    ctx.identities.insert(i.name.clone(), base.clone());
                }
            }
            _ => {}
        }
    }
}

/// Resolve a prefix used in a `uses`/`type`/feature reference to a module
/// name. Bare names resolve within the current module.
fn resolve_module_ref<'a>(
    reference: &'a str,
    ctx: &'a ModuleCtx,
    ctxs: &'a BTreeMap<String, ModuleCtx>,
) -> Option<(&'a str, &'a str)> {
    if let Some((prefix, name)) = reference.split_once(':') {
        // A module may reference its own prefix.
        if ctx.module.prefix.as_deref() == Some(prefix) {
            return Some((ctx.module.name.as_str(), name));
        }
        if let Some(module) = ctx.imports.get(prefix) {
            return Some((module.as_str(), name));
        }
        // prefix may itself be a module name (YANG allows prefix==module name)
        if ctxs.contains_key(prefix) {
            return Some((prefix, name));
        }
        None
    } else {
        Some((ctx.module.name.as_str(), reference))
    }
}

/// Canonicalize one `if-feature` expression for `ctx`'s lexical scope.
/// Feature references are rewritten to the *declaring* module's prefix
/// (`declared_prefix:local`) so `filter_by_features` matches regardless of
/// which import prefix each referencing module happened to use. Boolean
/// `and`/`or`/`not`/parenthesized expressions (YANG 1.1) are preserved as
/// a normalized expression string — they are never treated as feature
/// names. Unresolvable prefixes and malformed expressions produce
/// diagnostics and the raw text is kept (fail-visible, not silent).
fn canonicalize_if_feature_expr(
    expr: &str,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    diagnostics: &mut Vec<String>,
) -> String {
    let tokens = lex_feature_expr(expr);
    if tokens.is_empty() {
        diagnostics.push(format!(
            "module '{}': empty if-feature expression",
            ctx.module.name
        ));
        return expr.trim().to_string();
    }
    let mut out = String::new();
    let mut i = 0usize;
    let mut ok = true;
    while i < tokens.len() {
        match &tokens[i] {
            Ftok::LParen => {
                out.push('(');
                i += 1;
            }
            Ftok::RParen => {
                out.push(')');
                i += 1;
            }
            Ftok::Op(op) => {
                if !out.is_empty() && !out.ends_with('(') {
                    out.push(' ');
                }
                out.push_str(op);
                out.push(' ');
                i += 1;
            }
            Ftok::Feature(name) => {
                if !out.is_empty() && !out.ends_with('(') && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(&canonicalize_feature_atom(name, ctx, ctxs, diagnostics));
                i += 1;
            }
            Ftok::Bad(raw) => {
                diagnostics.push(format!(
                    "module '{}': unsupported if-feature expression token '{}'",
                    ctx.module.name, raw
                ));
                ok = false;
                i += 1;
            }
        }
    }
    let normalized = out.trim().to_string();
    if !ok {
        // Keep the raw expression so the gate is still recorded verbatim.
        return expr.trim().to_string();
    }
    normalized
}

#[derive(Clone, Debug, PartialEq)]
enum Ftok {
    Feature(String),
    Op(&'static str),
    LParen,
    RParen,
    Bad(String),
}

/// Tokenize an if-feature boolean expression. Feature names may contain
/// YANG identifier characters plus ':' for prefix qualification.
fn lex_feature_expr(expr: &str) -> Vec<Ftok> {
    let mut out = Vec::new();
    let mut chars = expr.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                out.push(Ftok::LParen);
            }
            ')' => {
                chars.next();
                out.push(Ftok::RParen);
            }
            _ => {
                let mut word = String::new();
                while let Some(&c2) = chars.peek() {
                    if c2.is_whitespace() || c2 == '(' || c2 == ')' {
                        break;
                    }
                    word.push(c2);
                    chars.next();
                }
                match word.as_str() {
                    "and" => out.push(Ftok::Op("and")),
                    "or" => out.push(Ftok::Op("or")),
                    "not" => out.push(Ftok::Op("not")),
                    _ if word.chars().all(|c3| {
                        c3.is_ascii_alphanumeric() || matches!(c3, '-' | '_' | '.' | ':')
                    }) && !word.is_empty() =>
                    {
                        out.push(Ftok::Feature(word));
                    }
                    _ => out.push(Ftok::Bad(word)),
                }
            }
        }
    }
    out
}

/// Resolve one feature atom to canonical `declared_prefix:local` form using
/// the lexical context of the module that wrote the reference.
fn canonicalize_feature_atom(
    atom: &str,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    diagnostics: &mut Vec<String>,
) -> String {
    let (module_name, local) = if let Some((prefix, local)) = atom.split_once(':') {
        if ctx.module.prefix.as_deref() == Some(prefix) {
            (ctx.module.name.clone(), local.to_string())
        } else if let Some(module) = ctx.imports.get(prefix) {
            (module.clone(), local.to_string())
        } else if ctxs.contains_key(prefix) {
            (prefix.to_string(), local.to_string())
        } else {
            diagnostics.push(format!(
                "module '{}': if-feature prefix '{}' cannot be resolved to a loaded module",
                ctx.module.name, prefix
            ));
            return atom.to_string();
        }
    } else {
        (ctx.module.name.clone(), atom.to_string())
    };
    let Some(target) = ctxs.get(&module_name) else {
        diagnostics.push(format!(
            "module '{}': if-feature module '{}' is not loaded",
            ctx.module.name, module_name
        ));
        return atom.to_string();
    };
    if !target.features.contains_key(&local) {
        diagnostics.push(format!(
            "module '{}': if-feature references undefined feature '{}:{}'",
            ctx.module.name, module_name, local
        ));
        return atom.to_string();
    }
    let declared = target.module.prefix.clone().unwrap_or_default();
    if declared.is_empty() {
        local
    } else {
        format!("{}:{}", declared, local)
    }
}

/// Canonicalize all if-feature expressions of a statement in `ctx`'s scope.
fn canonicalize_if_features(
    raw: &[String],
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    diagnostics: &mut Vec<String>,
) -> Vec<String> {
    raw.iter()
        .map(|e| canonicalize_if_feature_expr(e, ctx, ctxs, diagnostics))
        .collect()
}

/// Merge inherited (already canonical) feature-expression gates with a
/// node's own, preserving order and removing duplicates.
fn merge_feature_gates(inherited: &[String], own: Vec<String>) -> Vec<String> {
    let mut out = inherited.to_vec();
    for f in own {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

fn resolve_stmt(
    stmt: &Stmt,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    depth: usize,
    module_prefix: &BTreeMap<String, String>,
    inherited_features: &[String],
    diagnostics: &mut Vec<String>,
) -> Option<ResolvedNode> {
    if depth > MAX_USES_DEPTH {
        diagnostics.push(format!(
            "uses/grouping expansion depth exceeded in module '{}'",
            ctx.module.name
        ));
        return None;
    }
    let (kind, node) = match stmt {
        Stmt::Container(n) => ("container", n),
        Stmt::List(n) => ("list", n),
        Stmt::Leaf(n) => ("leaf", n),
        Stmt::LeafList(n) => ("leaf-list", n),
        Stmt::Choice(n) => ("choice", n),
        Stmt::Case(n) => ("case", n),
        Stmt::AnyXml(n) => ("anyxml", n),
        Stmt::AnyData(n) => ("anydata", n),
        Stmt::Input(n) => ("input", n),
        Stmt::Output(n) => ("output", n),
        Stmt::Rpc(n) => ("rpc", n),
        Stmt::Notification(n) => ("notification", n),
        _ => return None,
    };

    // This node's own if-feature gates (canonicalized in this module's
    // lexical scope) combine with gates inherited from ancestors — a
    // descendant can never be available when an ancestor is gated.
    let own_features = canonicalize_if_features(&node.if_features, ctx, ctxs, diagnostics);
    let node_features = merge_feature_gates(inherited_features, own_features);

    // Expand children: nested data-defs + uses.
    let mut children = Vec::new();
    for child in &node.children {
        match child {
            Stmt::Uses(u) => {
                children.extend(expand_uses(
                    u,
                    ctx,
                    ctxs,
                    depth + 1,
                    module_prefix,
                    &node_features,
                    diagnostics,
                ));
            }
            Stmt::Augment(_) | Stmt::Deviation(_) => {} // handled globally
            _ => {
                if let Some(rn) = resolve_stmt(
                    child,
                    ctx,
                    ctxs,
                    depth,
                    module_prefix,
                    &node_features,
                    diagnostics,
                ) {
                    children.push(rn);
                }
            }
        }
    }

    if let Some(spec) = &node.type_spec {
        check_type_ref(spec, ctx, ctxs, diagnostics);
    }

    // Key leaf children of a list are implicitly mandatory (RFC 7950).
    if kind == "list" {
        for child in &mut children {
            if node.keys.iter().any(|k| k == &child.name) {
                child.node.mandatory = Some(true);
            }
        }
    }

    let prefix = ctx.module.prefix.clone().unwrap_or_default();
    Some(ResolvedNode {
        name: node.name.clone(),
        kind,
        node: node.clone(),
        children,
        provenance_module: ctx.module.name.clone(),
        provenance_prefix: prefix,
        provenance_revision: ctx.module.latest_revision(),
        provenance_namespace: ctx.module.namespace.clone().unwrap_or_default(),
        required_features: node_features,
        deviated: false,
        deviation_notes: Vec::new(),
        pruned: false,
    })
}

/// Expand a `uses` statement into resolved nodes from the target grouping,
/// applying any `refine` substatements to the expanded children.
fn expand_uses(
    u: &super::parser::Uses,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    depth: usize,
    module_prefix: &BTreeMap<String, String>,
    inherited_features: &[String],
    diagnostics: &mut Vec<String>,
) -> Vec<ResolvedNode> {
    let mut expanded = Vec::new();
    if depth > MAX_USES_DEPTH {
        diagnostics.push(format!(
            "uses expansion depth exceeded in module '{}' (circular grouping reference?)",
            ctx.module.name
        ));
        return expanded;
    }
    let Some((module_name, grouping_name)) = resolve_module_ref(&u.name, ctx, ctxs) else {
        diagnostics.push(format!(
            "uses target '{}' in module '{}' cannot be resolved to a loaded module",
            u.name, ctx.module.name
        ));
        return expanded;
    };
    let Some(target_ctx) = ctxs.get(module_name) else {
        diagnostics.push(format!(
            "uses target module '{}' not loaded (for '{}')",
            module_name, u.name
        ));
        return expanded;
    };
    let Some(grouping) = target_ctx.groupings.get(grouping_name) else {
        diagnostics.push(format!(
            "uses target grouping '{}:{}' not found (module '{}')",
            module_name, grouping_name, ctx.module.name
        ));
        return expanded;
    };
    // `uses ... { if-feature ...; }` gates the whole expanded subtree.
    // The expression is written in the USING module's lexical scope, so it
    // canonicalizes against `ctx`, then propagates as inherited gates into
    // every expanded node (and their descendants).
    let uses_gates = canonicalize_if_features(&u.if_features, ctx, ctxs, diagnostics);
    let inner_inherited = merge_feature_gates(inherited_features, uses_gates);
    for gstmt in grouping {
        match gstmt {
            // `uses` may be nested directly inside a grouping body; its own
            // gates resolve in the grouping's module scope (target_ctx).
            Stmt::Uses(nested) => expanded.extend(expand_uses(
                nested,
                target_ctx,
                ctxs,
                depth + 1,
                module_prefix,
                &inner_inherited,
                diagnostics,
            )),
            _ => {
                if let Some(rn) = resolve_stmt(
                    gstmt,
                    target_ctx,
                    ctxs,
                    depth,
                    module_prefix,
                    &inner_inherited,
                    diagnostics,
                ) {
                    expanded.push(rn);
                }
            }
        }
    }
    apply_refines(&mut expanded, u, diagnostics);
    expanded
}

fn is_builtin_type(name: &str) -> bool {
    let local = name.rsplit(':').next().unwrap_or(name);
    matches!(
        local,
        "string"
            | "boolean"
            | "decimal64"
            | "identityref"
            | "enumeration"
            | "bits"
            | "empty"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "binary"
            | "leafref"
            | "union"
            | "anyxml"
            | "anydata"
    )
}

/// Record a diagnostic when a leaf type reference cannot be resolved to a
/// builtin or a known typedef. Resolution itself is tolerant — the raw type
/// name is preserved on the index entry.
fn check_type_ref(
    spec: &TypeSpec,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    diagnostics: &mut Vec<String>,
) {
    let name = spec.name.trim();
    if name.is_empty() || is_builtin_type(name) {
        return;
    }
    if let Some((prefix, local)) = name.split_once(':') {
        let resolved = if ctx.module.prefix.as_deref() == Some(prefix) {
            Some(ctx.module.name.as_str())
        } else {
            ctx.imports.get(prefix).map(|m| m.as_str())
        };
        match resolved.and_then(|m| ctxs.get(m)) {
            Some(target) if target.typedefs.contains_key(local) => {}
            Some(_) => diagnostics.push(format!(
                "module '{}': type '{}' is not a known typedef",
                ctx.module.name, name
            )),
            None => diagnostics.push(format!(
                "module '{}': type prefix '{}' cannot be resolved to a loaded module",
                ctx.module.name, prefix
            )),
        }
    } else if !ctx.typedefs.contains_key(name) {
        diagnostics.push(format!(
            "module '{}': type '{}' is not a builtin or known typedef",
            ctx.module.name, name
        ));
    }
}

/// Split a refine target into relative path segments. Segments may carry
/// `prefix:` qualifiers (resolved against the grouping's module scope is
/// approximated by local-name matching — prefixes are stripped, which is
/// correct for the overwhelmingly common unprefixed refine targets).
fn refine_segments(target: &str) -> Vec<String> {
    target
        .trim()
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| seg.rsplit(':').next().unwrap_or(seg).to_string())
        .collect()
}

/// Locate the node a refine target addresses within the expanded grouping
/// children. Each segment matches a direct child's local name — choice and
/// case nodes are matchable segments (refine targets name them explicitly).
fn find_refine_node<'a>(
    children: &'a mut [ResolvedNode],
    segments: &[String],
) -> Option<&'a mut ResolvedNode> {
    if segments.is_empty() {
        return None;
    }
    let idx = children.iter().position(|c| c.name == segments[0])?;
    let node = &mut children[idx];
    if segments.len() == 1 {
        return Some(node);
    }
    find_refine_node(&mut node.children, &segments[1..])
}

fn apply_refine_props(node: &mut ResolvedNode, refine: &super::parser::Refine) {
    if let Some(v) = refine.mandatory {
        node.node.mandatory = Some(v);
    }
    if let Some(v) = refine.config {
        node.node.config = Some(v);
    }
    if let Some(v) = &refine.default {
        node.node.default = Some(v.clone());
    }
    if let Some(v) = &refine.description {
        node.node.description = Some(v.clone());
    }
    if let Some(v) = &refine.units {
        node.node.units = Some(v.clone());
    }
    if let Some(v) = &refine.presence {
        node.node.presence = Some(v.clone());
    }
    if let Some(v) = refine.min_elements {
        node.node.min_elements = Some(v);
    }
    if let Some(v) = refine.max_elements {
        node.node.max_elements = Some(v);
    }
}

fn apply_refines(
    children: &mut [ResolvedNode],
    uses: &super::parser::Uses,
    diagnostics: &mut Vec<String>,
) {
    for refine in &uses.refines {
        for kw in &refine.unsupported {
            diagnostics.push(format!(
                "uses '{}': refine of '{}' contains unsupported '{}' substatement (not interpreted)",
                uses.name, refine.target, kw
            ));
        }
        let segments = refine_segments(&refine.target);
        match find_refine_node(children, &segments) {
            Some(node) => apply_refine_props(node, refine),
            None => diagnostics.push(format!(
                "uses '{}': refine target '{}' does not resolve within the expanded grouping",
                uses.name, refine.target
            )),
        }
    }
}

struct AugmentWork {
    target: String,
    children: Vec<Stmt>,
    module: String,
    /// Last resolution error, surfaced once if all passes fail.
    last_error: Option<String>,
}

struct DeviationWork {
    target: String,
    note: String,
    status: String,
    /// Module whose lexical scope resolves the target's prefixes.
    module: String,
    /// (kind, has_block) for each `deviate` statement.
    deviates: Vec<(String, bool)>,
}

fn collect_nested_deviations(
    d: &super::parser::Deviation,
    module: String,
    out: &mut Vec<DeviationWork>,
) {
    for child in &d.children {
        out.push(DeviationWork {
            target: format!("{}/{}", d.target, child.target),
            note: child
                .description
                .clone()
                .or(child.reference.clone())
                .unwrap_or_default(),
            status: child.status.clone().unwrap_or_default(),
            module: module.clone(),
            deviates: child
                .deviates
                .iter()
                .map(|x| (x.kind.clone(), x.has_block))
                .collect(),
        });
        collect_nested_deviations(child, module.clone(), out);
    }
}

/// One segment of an augment/deviation target. `prefix` is the qualifier
/// exactly as written (`prefix:name`); `local` is the node name.
#[derive(Clone, Debug)]
struct TargetSeg {
    prefix: Option<String>,
    local: String,
}

/// Parse an augment/deviation target into (absolute, segments). The
/// returned string keeps prefixes so resolution can bind them through the
/// *declaring* module's import context — never by first local-name match.
fn parse_target(target: &str) -> (bool, Vec<TargetSeg>) {
    let t = target.trim();
    let absolute = t.starts_with('/');
    let segs: Vec<TargetSeg> = t
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| match seg.split_once(':') {
            Some((p, local)) => TargetSeg {
                prefix: Some(p.to_string()),
                local: local.to_string(),
            },
            None => TargetSeg {
                prefix: None,
                local: seg.to_string(),
            },
        })
        .collect();
    (absolute, segs)
}

/// Resolve a written prefix to a module name using `declaring`'s lexical
/// scope: self-prefix, declared imports, or a module name used as prefix.
fn resolve_target_prefix(
    prefix: &str,
    declaring: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
) -> Option<String> {
    if declaring.module.prefix.as_deref() == Some(prefix) {
        return Some(declaring.module.name.clone());
    }
    if let Some(module) = declaring.imports.get(prefix) {
        return Some(module.clone());
    }
    if ctxs.contains_key(prefix) {
        return Some(prefix.to_string());
    }
    None
}

/// Check whether `node` satisfies a target segment's prefix constraint:
/// a prefixed segment only matches a node whose provenance module is the
/// module that prefix resolves to in the declaring module's scope.
fn seg_prefix_matches(
    seg: &TargetSeg,
    node: &ResolvedNode,
    declaring: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
) -> bool {
    match &seg.prefix {
        None => true,
        Some(p) => {
            resolve_target_prefix(p, declaring, ctxs).is_some_and(|m| m == node.provenance_module)
        }
    }
}

/// Find a non-choice/case descendant named `name`, treating choice and case
/// nodes as transparent wrappers (they are not instance-data path segments).
/// Returns child indices relative to `node.children`.
fn transparent_find(node: &ResolvedNode, name: &str) -> Option<Vec<usize>> {
    // Exact name match on any child (choice/case names are valid
    // augment/deviation target segments even though they are not gNMI
    // path segments).
    for (i, c) in node.children.iter().enumerate() {
        if c.name == name {
            return Some(vec![i]);
        }
    }
    // Otherwise descend transparently through choice/case wrappers.
    for (i, c) in node.children.iter().enumerate() {
        if matches!(c.kind, "choice" | "case")
            && let Some(mut sub) = transparent_find(c, name)
        {
            let mut p = vec![i];
            p.append(&mut sub);
            return Some(p);
        }
    }
    None
}

fn node_at<'a>(nodes: &'a [ResolvedNode], path: &[usize]) -> Option<&'a ResolvedNode> {
    let mut cur = nodes.get(*path.first()?)?;
    for &idx in &path[1..] {
        cur = cur.children.get(idx)?;
    }
    Some(cur)
}

fn node_at_mut<'a>(nodes: &'a mut [ResolvedNode], path: &[usize]) -> Option<&'a mut ResolvedNode> {
    fn rec<'a>(node: &'a mut ResolvedNode, idxs: &[usize]) -> Option<&'a mut ResolvedNode> {
        if idxs.is_empty() {
            return Some(node);
        }
        let child = node.children.get_mut(idxs[0])?;
        rec(child, &idxs[1..])
    }
    let first = *path.first()?;
    rec(nodes.get_mut(first)?, &path[1..])
}

/// Locate the node an augment/deviation target addresses.
///
/// Resolution is namespace-aware: the first segment's prefix (when
/// present) binds the target module through the *declaring* module's
/// import context — it is never "first local-name match wins". Deeper
/// prefixed segments must match the matched node's provenance module.
/// Unprefixed first segments search all modules and require a UNIQUE
/// match; ambiguity or non-resolution is an explicit error.
///
/// Returns (module_name, index path) or an explanatory diagnostic string.
fn locate_node(
    roots: &BTreeMap<String, Vec<ResolvedNode>>,
    ctxs: &BTreeMap<String, ModuleCtx>,
    declaring: &ModuleCtx,
    absolute: bool,
    segs: &[TargetSeg],
) -> Result<(String, Vec<usize>), String> {
    if segs.is_empty() {
        return Err("empty target".to_string());
    }
    let first = &segs[0];

    // Candidate modules, in deterministic order.
    let mut candidates: Vec<String> = if let Some(p) = &first.prefix {
        match resolve_target_prefix(p, declaring, ctxs) {
            Some(m) if roots.contains_key(&m) => vec![m],
            Some(m) => return Err(format!("target module '{}' is not loaded", m)),
            None => {
                return Err(format!(
                    "target prefix '{}' cannot be resolved in module '{}'",
                    p, declaring.module.name
                ));
            }
        }
    } else if absolute {
        // Unprefixed absolute target: must be unique across modules.
        roots.keys().cloned().collect()
    } else {
        // Relative target: declaring module first, then others.
        let mut v = vec![declaring.module.name.clone()];
        v.extend(
            roots
                .keys()
                .filter(|k| *k != &declaring.module.name)
                .cloned(),
        );
        v
    };
    candidates.retain(|m| roots.contains_key(m));
    candidates.sort();
    candidates.dedup();

    let mut matches: Vec<(String, Vec<usize>)> = Vec::new();
    'outer: for module_name in &candidates {
        let nodes = &roots[module_name];
        for (i, top) in nodes.iter().enumerate() {
            if top.name != first.local || !seg_prefix_matches(first, top, declaring, ctxs) {
                continue;
            }
            let mut path = vec![i];
            let mut matched = true;
            for seg in &segs[1..] {
                let cur = match node_at(nodes, &path) {
                    Some(n) => n,
                    None => {
                        matched = false;
                        break;
                    }
                };
                match transparent_find(cur, &seg.local) {
                    Some(mut sub) => {
                        // Verify prefix constraint on the resolved node.
                        let found = node_at(nodes, &{
                            let mut p = path.clone();
                            p.append(&mut sub.clone());
                            p
                        });
                        match found {
                            Some(n) if seg_prefix_matches(seg, n, declaring, ctxs) => {
                                path.append(&mut sub);
                            }
                            _ => {
                                matched = false;
                                break;
                            }
                        }
                    }
                    None => {
                        matched = false;
                        break;
                    }
                }
            }
            if matched {
                matches.push((module_name.clone(), path));
                if first.prefix.is_some() {
                    break 'outer; // module already pinned by prefix
                }
            }
        }
    }

    match matches.len() {
        0 => Err(format!(
            "target '{}' does not resolve to any loaded schema node",
            segs.iter()
                .map(|s| {
                    s.prefix
                        .as_ref()
                        .map(|p| format!("{}:{}", p, s.local))
                        .unwrap_or_else(|| s.local.clone())
                })
                .collect::<Vec<_>>()
                .join("/")
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "target is ambiguous: {} modules contain a matching path",
            matches.len()
        )),
    }
}

fn apply_augment(
    roots: &mut BTreeMap<String, Vec<ResolvedNode>>,
    ctxs: &BTreeMap<String, ModuleCtx>,
    aw: &mut AugmentWork,
    module_prefix: &BTreeMap<String, String>,
    diagnostics: &mut Vec<String>,
) -> bool {
    let (absolute, segs) = parse_target(&aw.target);
    let Some(declaring) = ctxs.get(&aw.module) else {
        aw.last_error = Some(format!("declaring module '{}' not loaded", aw.module));
        return false;
    };
    let located = locate_node(roots, ctxs, declaring, absolute, &segs);
    let (module_name, path) = match located {
        Ok(v) => v,
        Err(e) => {
            aw.last_error = Some(e);
            return false;
        }
    };

    // Resolve augment children in the augmenting module's context. They
    // become descendants of the target node, so they inherit the target's
    // feature gates.
    let inherited: Vec<String> = roots
        .get(&module_name)
        .and_then(|nodes| node_at(nodes, &path))
        .map(|n| n.required_features.clone())
        .unwrap_or_default();
    let mut new_children = Vec::new();
    for stmt in &aw.children {
        if let Some(rn) = resolve_stmt(
            stmt,
            declaring,
            ctxs,
            0,
            module_prefix,
            &inherited,
            diagnostics,
        ) {
            new_children.push(rn);
        }
    }
    if new_children.is_empty() {
        return true; // target found; nothing to merge
    }

    let Some(nodes) = roots.get_mut(&module_name) else {
        return false;
    };
    let Some(target) = node_at_mut(nodes, &path) else {
        return false;
    };
    for nc in new_children {
        let exists = target
            .children
            .iter()
            .any(|c| c.name == nc.name && c.kind == nc.kind);
        if !exists {
            target.children.push(nc);
        }
    }
    true
}

fn apply_deviation(
    roots: &mut BTreeMap<String, Vec<ResolvedNode>>,
    ctxs: &BTreeMap<String, ModuleCtx>,
    dw: &DeviationWork,
    module_prefix: &BTreeMap<String, String>,
    diagnostics: &mut Vec<String>,
) {
    let (absolute, segs) = parse_target(&dw.target);
    let _ = module_prefix;
    let Some(declaring) = ctxs.get(&dw.module) else {
        diagnostics.push(format!(
            "deviation target '{}' in module '{}': declaring module not loaded",
            dw.target, dw.module
        ));
        return;
    };
    let located = locate_node(roots, ctxs, declaring, absolute, &segs);
    let (module_name, path) = match located {
        Ok(v) => v,
        Err(e) => {
            diagnostics.push(format!(
                "deviation target '{}' in module '{}': {}",
                dw.target, dw.module, e
            ));
            return;
        }
    };
    // `deviate` kinds drive semantics:
    // - not-supported: the node is declared but NOT part of the effective
    //   schema — prune it (and its subtree) from the path index.
    // - add/delete/replace: real YANG restructuring we do not implement;
    //   the subtree is marked deviated AND an explicit diagnostic keeps the
    //   catalogue partial so it cannot pose as authoritative.
    let has_not_supported = dw.deviates.iter().any(|(k, _)| k == "not-supported");
    let other_kinds: Vec<&str> = dw
        .deviates
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| *k != "not-supported")
        .collect();
    for kind in &other_kinds {
        diagnostics.push(format!(
            "deviation '{}' in module '{}' uses 'deviate {}' — only marking is applied, not full deviation semantics",
            dw.target, dw.module, kind
        ));
    }
    let Some(nodes) = roots.get_mut(&module_name) else {
        return;
    };
    let Some(node) = node_at_mut(nodes, &path) else {
        return;
    };
    if has_not_supported {
        mark_pruned(node, dw);
    } else {
        mark_deviated(node, dw);
    }
}

fn mark_pruned(node: &mut ResolvedNode, dw: &DeviationWork) {
    node.pruned = true;
    if !dw.note.is_empty() {
        node.deviation_notes.push(dw.note.clone());
    }
    for child in &mut node.children {
        mark_pruned(child, dw);
    }
}

fn mark_deviated(node: &mut ResolvedNode, dw: &DeviationWork) {
    node.deviated = true;
    if !dw.note.is_empty() {
        node.deviation_notes.push(dw.note.clone());
    }
    if !dw.status.is_empty() {
        node.node.status = Some(dw.status.clone());
    }
    for child in &mut node.children {
        mark_deviated(child, dw);
    }
}

// ---------------------------------------------------------------------------
// Path index emission
// ---------------------------------------------------------------------------

fn emit_paths(
    node: &ResolvedNode,
    prefix_path: &str,
    inherited_config: bool,
    ctxs: &BTreeMap<String, ModuleCtx>,
    out: &mut Vec<SchemaPathEntry>,
    count: &mut usize,
    saturated: &mut bool,
) {
    if node.pruned {
        // `deviate not-supported`: excluded from the effective schema.
        return;
    }
    if out.len() >= MAX_PATH_ENTRIES {
        *saturated = true;
        return;
    }
    *count += 1;
    let effective_config = node.node.config.unwrap_or(inherited_config);
    let segment = node.segment();
    let path = if prefix_path.is_empty() {
        format!("/{}", segment)
    } else {
        format!("{}/{}", prefix_path, segment)
    };

    // choice/case/input/output/rpc/notification are not gNMI data nodes —
    // do not emit index entries for them, only recurse.
    let indexable = matches!(
        node.kind,
        "container" | "list" | "leaf" | "leaf-list" | "anyxml" | "anydata"
    );

    if indexable {
        // Type references resolve in the lexical scope of the module that
        // DECLARES the node (augment/uses provenance), not the root module
        // being emitted — cross-module typedefs must unwrap correctly.
        let node_ctx = ctxs.get(&node.provenance_module).map(|c| c as &ModuleCtx);
        let fallback = ctxs.values().next();
        let type_ctx = node_ctx.or(fallback);
        let (type_name, resolved_type) = match type_ctx {
            Some(tc) => resolve_type_name(&node.node.type_spec, tc, ctxs),
            None => (None, None),
        };
        out.push(SchemaPathEntry {
            module: node.provenance_module.clone(),
            module_revision: node.provenance_revision.clone(),
            namespace: node.provenance_namespace.clone(),
            module_prefix: node.provenance_prefix.clone(),
            path: path.clone(),
            kind: node.kind.to_string(),
            config: effective_config,
            declared_config: node.node.config,
            mandatory: node.node.mandatory.unwrap_or(false),
            presence: node.node.presence.clone(),
            type_name,
            resolved_type,
            units: node.node.units.clone(),
            default: node.node.default.clone(),
            description: node.node.description.clone(),
            keys: node.node.keys.clone(),
            min_elements: node.node.min_elements,
            max_elements: node.node.max_elements,
            ordered_by: node.node.ordered_by.clone(),
            required_features: node.required_features.clone(),
            deviated: node.deviated,
            deviation_notes: node.deviation_notes.clone(),
            status: node.node.status.clone(),
        });
    }

    // Recurse: choice/case do not extend the path; others do.
    let child_prefix = if matches!(node.kind, "choice" | "case") {
        prefix_path.to_string()
    } else {
        path.clone()
    };
    for child in &node.children {
        emit_paths(
            child,
            &child_prefix,
            effective_config,
            ctxs,
            out,
            count,
            saturated,
        );
    }
}

fn resolve_type_name(
    spec: &Option<TypeSpec>,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
) -> (Option<String>, Option<String>) {
    let Some(spec) = spec else {
        return (None, None);
    };
    let mut current = spec.name.clone();
    // Unwrap typedef chains across module boundaries, bounded.
    for _ in 0..16 {
        let (module_name, local) = if let Some((prefix, name)) = current.split_once(':') {
            if ctx.module.prefix.as_deref() == Some(prefix) {
                (ctx.module.name.clone(), name.to_string())
            } else {
                match ctx.imports.get(prefix) {
                    Some(m) => (m.clone(), name.to_string()),
                    None => break,
                }
            }
        } else {
            (ctx.module.name.clone(), current.clone())
        };
        let Some(target_ctx) = ctxs.get(&module_name) else {
            break;
        };
        let Some(next) = target_ctx.typedefs.get(&local) else {
            break;
        };
        // Guard against typedef cycles.
        if next.name == current {
            break;
        }
        current = if current.contains(':') {
            format!("{}:{}", prefix_of(ctx, &module_name), next.name)
        } else {
            next.name.clone()
        };
    }
    (Some(spec.name.clone()), Some(current))
}

fn prefix_of(ctx: &ModuleCtx, module_name: &str) -> String {
    // Find the import prefix pointing at module_name; fall back to module name.
    for (prefix, module) in &ctx.imports {
        if module == module_name {
            return prefix.clone();
        }
    }
    module_name.to_string()
}

fn compute_schema_id(
    modules: &[CompiledModule],
    paths: &[SchemaPathEntry],
    status: &str,
    diagnostics: &[String],
) -> String {
    // Canonical semantic identity: serialize the deterministic semantic
    // content (compiler version, completeness status, module metadata,
    // features, full path entries, diagnostics) and hash it. Nondeterministic
    // fields (compiled_at_ns, schema_id itself) are excluded by construction.
    // All vectors are already sorted/deduped by the compiler, so the JSON
    // encoding is canonical for a given semantic input.
    #[derive(Serialize)]
    struct Identity<'a> {
        compiler_version: u32,
        status: &'a str,
        modules: &'a [CompiledModule],
        paths: &'a [SchemaPathEntry],
        diagnostics: &'a [String],
    }
    let identity = Identity {
        compiler_version: COMPILED_SCHEMA_VERSION,
        status,
        modules,
        paths,
        diagnostics,
    };
    let canonical = serde_json::to_vec(&identity).unwrap_or_default();
    let digest = Sha256::digest(&canonical);
    format!("ysc-{}", hex::encode(&digest[..16]))
}

/// Recompute the semantic schema identity of a deserialized catalogue.
/// Used by the store layer to detect corrupted or tampered bundles: a
/// bundle whose embedded `schema_id` does not match its content is
/// rejected rather than activated.
pub fn recompute_schema_id(catalogue: &CompiledCatalogue) -> String {
    compute_schema_id(
        &catalogue.modules,
        &catalogue.paths,
        &catalogue.status,
        &catalogue.diagnostics,
    )
}

// ---------------------------------------------------------------------------
// Feature filtering for consumers
// ---------------------------------------------------------------------------

/// Evaluate one canonical if-feature expression against `enabled`.
/// Grammar (YANG 1.1): `or` of `and` of `not` of primary; primary is a
/// feature atom or parenthesized expression. Returns `None` when the
/// expression is malformed (treated as "requirement cannot be proven
/// satisfied" — fail closed).
fn eval_feature_expr(tokens: &[Ftok], enabled: &BTreeSet<String>) -> Option<bool> {
    let mut pos = 0usize;
    fn peek<'a>(t: &'a [Ftok], p: &mut usize) -> Option<&'a Ftok> {
        t.get(*p)
    }
    fn parse_or(t: &[Ftok], p: &mut usize, e: &BTreeSet<String>) -> Option<bool> {
        let mut v = parse_and(t, p, e)?;
        while matches!(peek(t, p), Some(Ftok::Op("or"))) {
            *p += 1;
            let r = parse_and(t, p, e)?;
            v = v || r;
        }
        Some(v)
    }
    fn parse_and(t: &[Ftok], p: &mut usize, e: &BTreeSet<String>) -> Option<bool> {
        let mut v = parse_not(t, p, e)?;
        while matches!(peek(t, p), Some(Ftok::Op("and"))) {
            *p += 1;
            let r = parse_not(t, p, e)?;
            v = v && r;
        }
        Some(v)
    }
    fn parse_not(t: &[Ftok], p: &mut usize, e: &BTreeSet<String>) -> Option<bool> {
        if matches!(peek(t, p), Some(Ftok::Op("not"))) {
            *p += 1;
            return Some(!parse_not(t, p, e)?);
        }
        parse_primary(t, p, e)
    }
    fn parse_primary(t: &[Ftok], p: &mut usize, e: &BTreeSet<String>) -> Option<bool> {
        match t.get(*p)? {
            Ftok::LParen => {
                *p += 1;
                let v = parse_or(t, p, e)?;
                if matches!(t.get(*p), Some(Ftok::RParen)) {
                    *p += 1;
                    Some(v)
                } else {
                    None
                }
            }
            Ftok::Feature(name) => {
                *p += 1;
                Some(feature_atom_enabled(name, e))
            }
            _ => None,
        }
    }
    let v = parse_or(tokens, &mut pos, enabled)?;
    if pos == tokens.len() { Some(v) } else { None }
}

/// A canonical feature atom is `declared_prefix:local`. Callers may also
/// supply bare `local` or `module:prefix:local` forms; match leniently but
/// deterministically.
fn feature_atom_enabled(atom: &str, enabled: &BTreeSet<String>) -> bool {
    if enabled.contains(atom) {
        return true;
    }
    // Accept the local name alone (callers may enable features by name).
    if let Some((_, local)) = atom.rsplit_once(':')
        && enabled.contains(local)
    {
        return true;
    }
    false
}

/// Tri-state truth for feature evidence (M6C-1). `Unknown` is a real
/// outcome: evidence that cannot prove a feature enabled or disabled
/// must never be collapsed into either boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureTruth {
    True,
    False,
    Unknown,
}

impl FeatureTruth {
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }
    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }
    fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

/// Evaluate one canonical if-feature expression with explicit unknown
/// handling. `atom_truth` resolves a single feature atom (already in
/// canonical `prefix:local` or bare form); returning `Unknown` for an
/// atom propagates through the boolean expression per three-valued
/// logic. Malformed expressions evaluate to `Unknown` — a gate we
/// cannot parse must never create positive applicability.
pub fn eval_feature_expr_tristate(
    expr: &str,
    atom_truth: &dyn Fn(&str) -> FeatureTruth,
) -> FeatureTruth {
    let tokens = lex_feature_expr(expr);
    let mut pos = 0usize;
    fn peek<'a>(t: &'a [Ftok], p: &mut usize) -> Option<&'a Ftok> {
        t.get(*p)
    }
    fn parse_or(
        t: &[Ftok],
        p: &mut usize,
        f: &dyn Fn(&str) -> FeatureTruth,
    ) -> Option<FeatureTruth> {
        let mut v = parse_and(t, p, f)?;
        while matches!(peek(t, p), Some(Ftok::Op("or"))) {
            *p += 1;
            let r = parse_and(t, p, f)?;
            v = v.or(r);
        }
        Some(v)
    }
    fn parse_and(
        t: &[Ftok],
        p: &mut usize,
        f: &dyn Fn(&str) -> FeatureTruth,
    ) -> Option<FeatureTruth> {
        let mut v = parse_not(t, p, f)?;
        while matches!(peek(t, p), Some(Ftok::Op("and"))) {
            *p += 1;
            let r = parse_not(t, p, f)?;
            v = v.and(r);
        }
        Some(v)
    }
    fn parse_not(
        t: &[Ftok],
        p: &mut usize,
        f: &dyn Fn(&str) -> FeatureTruth,
    ) -> Option<FeatureTruth> {
        if matches!(peek(t, p), Some(Ftok::Op("not"))) {
            *p += 1;
            return Some(parse_not(t, p, f)?.not());
        }
        parse_primary(t, p, f)
    }
    fn parse_primary(
        t: &[Ftok],
        p: &mut usize,
        f: &dyn Fn(&str) -> FeatureTruth,
    ) -> Option<FeatureTruth> {
        match t.get(*p)? {
            Ftok::LParen => {
                *p += 1;
                let v = parse_or(t, p, f)?;
                if matches!(t.get(*p), Some(Ftok::RParen)) {
                    *p += 1;
                    Some(v)
                } else {
                    None
                }
            }
            Ftok::Feature(name) => {
                *p += 1;
                Some(f(name))
            }
            _ => None,
        }
    }
    match parse_or(&tokens, &mut pos, atom_truth) {
        Some(v) if pos == tokens.len() => v,
        _ => FeatureTruth::Unknown,
    }
}

/// Return path entries whose required feature expressions are all satisfied
/// by `enabled`. Expressions with unparseable content fail closed (entry
/// excluded) — a gate we cannot evaluate must not silently pass.
pub fn filter_by_features<'a>(
    entries: &'a [SchemaPathEntry],
    enabled: &BTreeSet<String>,
) -> Vec<&'a SchemaPathEntry> {
    entries
        .iter()
        .filter(|e| {
            e.required_features.iter().all(|expr| {
                let tokens = lex_feature_expr(expr);
                eval_feature_expr(&tokens, enabled).unwrap_or(false)
            })
        })
        .collect()
}

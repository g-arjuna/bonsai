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
}

pub struct CompileOptions {
    /// When true, modules whose trust != "trusted" are excluded.
    pub trusted_only: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { trusted_only: true }
    }
}

// ---------------------------------------------------------------------------
// Compile driver
// ---------------------------------------------------------------------------

pub fn compile(inputs: Vec<ModuleInput>, options: &CompileOptions) -> CompiledCatalogue {
    let mut diagnostics = Vec::new();

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

    // For duplicate module names keep only the newest revision.
    let mut drop_idx = BTreeSet::new();
    for (_name, idxs) in &by_name {
        if idxs.len() > 1 {
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
                    for rn in expand_uses(u, ctx, &ctxs, 1, &module_prefix, &mut diagnostics) {
                        resolved.push(rn);
                    }
                }
                _ => {
                    if let Some(node) =
                        resolve_stmt(stmt, ctx, &ctxs, 0, &module_prefix, &mut diagnostics)
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
                });
                collect_nested_deviations(d, name.clone(), &mut deviations);
            }
        }
    }

    // Apply augments iteratively (bounded).
    for _pass in 0..MAX_RESOLUTION_PASSES {
        let mut applied = false;
        let mut pending = Vec::new();
        for aw in augments.drain(..) {
            if apply_augment(&mut roots, &ctxs, &aw, &module_prefix, &mut diagnostics) {
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
        diagnostics.push(format!(
            "augment target '{}' in module '{}' could not be resolved",
            aw.target, aw.module
        ));
    }

    // Apply deviations.
    for dw in &deviations {
        apply_deviation(&mut roots, dw);
    }

    // Feature-gate evaluation helper data is embedded per node; build index.
    let mut paths = Vec::new();
    let mut modules_out = Vec::new();
    for (name, ctx) in &ctxs {
        let nodes = roots.get(name).cloned().unwrap_or_default();
        let mut count = 0usize;
        for node in &nodes {
            emit_paths(node, "", true, ctx, &ctxs, &mut paths, &mut count);
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

    let schema_id = compute_schema_id(&modules_out, &paths);
    let status = if modules_out.is_empty() {
        "failed"
    } else if diagnostics.is_empty() {
        "complete"
    } else {
        "partial"
    }
    .to_string();

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

fn resolve_stmt(
    stmt: &Stmt,
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    depth: usize,
    module_prefix: &BTreeMap<String, String>,
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
                    diagnostics,
                ));
            }
            Stmt::Augment(_) | Stmt::Deviation(_) => {} // handled globally
            _ => {
                if let Some(rn) = resolve_stmt(child, ctx, ctxs, depth, module_prefix, diagnostics)
                {
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
        required_features: node.if_features.clone(),
        deviated: false,
        deviation_notes: Vec::new(),
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
    for gstmt in grouping {
        match gstmt {
            // `uses` may be nested directly inside a grouping body.
            Stmt::Uses(nested) => expanded.extend(expand_uses(
                nested,
                target_ctx,
                ctxs,
                depth + 1,
                module_prefix,
                diagnostics,
            )),
            _ => {
                if let Some(rn) =
                    resolve_stmt(gstmt, target_ctx, ctxs, depth, module_prefix, diagnostics)
                {
                    expanded.push(rn);
                }
            }
        }
    }
    // `uses ... { if-feature f; }` gates the whole expanded subtree.
    if !u.if_features.is_empty() {
        for rn in &mut expanded {
            inherit_features(rn, &u.if_features);
        }
    }
    apply_refines(&mut expanded, u, diagnostics);
    expanded
}

/// Append feature requirements to a resolved node and its whole subtree.
fn inherit_features(node: &mut ResolvedNode, features: &[String]) {
    for f in features {
        if !node.required_features.contains(f) {
            node.required_features.push(f.clone());
        }
    }
    for child in &mut node.children {
        inherit_features(child, features);
    }
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

fn apply_refines(
    children: &mut [ResolvedNode],
    uses: &super::parser::Uses,
    diagnostics: &mut Vec<String>,
) {
    for refine in &uses.refines {
        // refine target is relative to the grouping's children; match by name
        // on first segment.
        let first = refine.target.split('/').next().unwrap_or("");
        let first = first.split(':').next_back().unwrap_or(first);
        for child in children.iter_mut() {
            if child.name == first {
                if let Some(v) = refine.mandatory {
                    child.node.mandatory = Some(v);
                }
                if let Some(v) = refine.config {
                    child.node.config = Some(v);
                }
                if let Some(v) = &refine.default {
                    child.node.default = Some(v.clone());
                }
                if let Some(v) = &refine.description {
                    child.node.description = Some(v.clone());
                }
                if let Some(v) = &refine.units {
                    child.node.units = Some(v.clone());
                }
                if let Some(v) = &refine.presence {
                    child.node.presence = Some(v.clone());
                }
                if let Some(v) = refine.min_elements {
                    child.node.min_elements = Some(v);
                }
                if let Some(v) = refine.max_elements {
                    child.node.max_elements = Some(v);
                }
                break;
            }
        }
        let _ = diagnostics;
    }
}

struct AugmentWork {
    target: String,
    children: Vec<Stmt>,
    module: String,
}

struct DeviationWork {
    target: String,
    note: String,
    status: String,
}

fn collect_nested_deviations(
    d: &super::parser::Deviation,
    _module: String,
    out: &mut Vec<DeviationWork>,
) {
    let _ = &_module;
    for child in &d.children {
        out.push(DeviationWork {
            target: format!("{}/{}", d.target, child.target),
            note: child
                .description
                .clone()
                .or(child.reference.clone())
                .unwrap_or_default(),
            status: child.status.clone().unwrap_or_default(),
        });
        collect_nested_deviations(child, _module.clone(), out);
    }
}

/// Parse an augment/deviation target into path segments.
/// "prefix:name" → ["name"] qualified by prefix; "/a/b/c" → [a,b,c];
/// "a/b" → [a,b].
fn target_segments(target: &str) -> Vec<String> {
    let t = target.trim().trim_start_matches('/');
    t.split('/')
        .filter(|s| !s.is_empty())
        .map(|seg| {
            // strip prefix qualifier, keep local name
            seg.rsplit(':').next().unwrap_or(seg).to_string()
        })
        .collect()
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
        if matches!(c.kind, "choice" | "case") {
            if let Some(mut sub) = transparent_find(c, name) {
                let mut p = vec![i];
                p.append(&mut sub);
                return Some(p);
            }
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

/// Locate a node described by `segments` across all module roots.
/// Returns (module_name, index path) where the path's first element indexes
/// the module's top-level node vector.
fn locate_node(
    roots: &BTreeMap<String, Vec<ResolvedNode>>,
    segments: &[String],
    preferred_module: Option<&str>,
) -> Option<(String, Vec<usize>)> {
    if segments.is_empty() {
        return None;
    }
    let mut module_names: Vec<&String> = roots.keys().collect();
    module_names.sort();
    if let Some(pref) = preferred_module {
        module_names.sort_by(|a, b| {
            let ra = if a.as_str() == pref { 0 } else { 1 };
            let rb = if b.as_str() == pref { 0 } else { 1 };
            ra.cmp(&rb).then_with(|| a.cmp(b))
        });
    }
    'outer: for module_name in module_names {
        let nodes = &roots[module_name];
        for (i, top) in nodes.iter().enumerate() {
            if top.name != segments[0] {
                continue;
            }
            let mut path = vec![i];
            let mut matched = true;
            for seg in &segments[1..] {
                let cur = match node_at(nodes, &path) {
                    Some(n) => n,
                    None => {
                        matched = false;
                        break;
                    }
                };
                match transparent_find(cur, seg) {
                    Some(mut sub) => path.append(&mut sub),
                    None => {
                        matched = false;
                        break;
                    }
                }
            }
            if matched {
                return Some((module_name.clone(), path));
            }
        }
        continue 'outer;
    }
    None
}

/// Resolve the module a target prefix refers to, for disambiguation.
fn target_preferred_module(
    target: &str,
    segments: &[String],
    ctxs: &BTreeMap<String, ModuleCtx>,
    roots: &BTreeMap<String, Vec<ResolvedNode>>,
) -> Option<String> {
    let qualifier = target
        .split('/')
        .next()
        .and_then(|s| s.split(':').next())
        .filter(|p| !segments.is_empty() && *p != segments[0])?;
    // qualifier may be a module prefix or a module name.
    for (name, ctx) in ctxs {
        if ctx.module.prefix.as_deref() == Some(qualifier) || name == qualifier {
            return Some(name.clone());
        }
    }
    if roots.contains_key(qualifier) {
        return Some(qualifier.to_string());
    }
    None
}

fn apply_augment(
    roots: &mut BTreeMap<String, Vec<ResolvedNode>>,
    ctxs: &BTreeMap<String, ModuleCtx>,
    aw: &AugmentWork,
    module_prefix: &BTreeMap<String, String>,
    diagnostics: &mut Vec<String>,
) -> bool {
    let segments = target_segments(&aw.target);
    if segments.is_empty() {
        return false;
    }
    let preferred = target_preferred_module(&aw.target, &segments, ctxs, roots);
    let Some((module_name, path)) = locate_node(roots, &segments, preferred.as_deref()) else {
        return false;
    };

    // Resolve augment children in the augmenting module's context.
    let Some(ctx) = ctxs.get(&aw.module) else {
        return false;
    };
    let mut new_children = Vec::new();
    for stmt in &aw.children {
        if let Some(rn) = resolve_stmt(stmt, ctx, ctxs, 0, module_prefix, diagnostics) {
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

fn apply_deviation(roots: &mut BTreeMap<String, Vec<ResolvedNode>>, dw: &DeviationWork) {
    let segments = target_segments(&dw.target);
    if segments.is_empty() {
        return;
    }
    // Deviations don't carry ctxs here; prefer a module whose name matches
    // the target qualifier, else search all modules.
    let preferred = dw
        .target
        .split('/')
        .next()
        .and_then(|s| s.split(':').next())
        .filter(|p| !segments.is_empty() && *p != segments[0])
        .map(str::to_string)
        .filter(|p| roots.contains_key(p));
    let Some((module_name, path)) = locate_node(roots, &segments, preferred.as_deref()) else {
        return;
    };
    let Some(nodes) = roots.get_mut(&module_name) else {
        return;
    };
    let Some(node) = node_at_mut(nodes, &path) else {
        return;
    };
    // A deviation applies to the targeted node and its whole subtree.
    mark_deviated(node, dw);
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
    ctx: &ModuleCtx,
    ctxs: &BTreeMap<String, ModuleCtx>,
    out: &mut Vec<SchemaPathEntry>,
    count: &mut usize,
) {
    if out.len() >= MAX_PATH_ENTRIES {
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
        let (type_name, resolved_type) = resolve_type_name(&node.node.type_spec, ctx, ctxs);
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
            ctx,
            ctxs,
            out,
            count,
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

fn compute_schema_id(modules: &[CompiledModule], paths: &[SchemaPathEntry]) -> String {
    let mut h = Sha256::new();
    for m in modules {
        h.update(m.name.as_bytes());
        h.update(b"@");
        h.update(m.revision.as_bytes());
        h.update(b"@");
        h.update(m.checksum_sha256.as_bytes());
        h.update(b"\n");
    }
    for p in paths.iter().take(1_000_000) {
        h.update(p.module.as_bytes());
        h.update(p.path.as_bytes());
        h.update(p.kind.as_bytes());
        h.update(b"\n");
    }
    let digest = h.finalize();
    format!("ysc-{}", hex::encode(&digest[..12]))
}

// ---------------------------------------------------------------------------
// Feature filtering for consumers
// ---------------------------------------------------------------------------

/// Return path entries whose required features are all satisfied by
/// `enabled` (entries with no requirements always pass). Feature references
/// are matched both as written and qualified with the module prefix.
pub fn filter_by_features<'a>(
    entries: &'a [SchemaPathEntry],
    enabled: &BTreeSet<String>,
) -> Vec<&'a SchemaPathEntry> {
    entries
        .iter()
        .filter(|e| {
            e.required_features.iter().all(|f| {
                enabled.contains(f) || enabled.contains(&format!("{}:{}", e.module_prefix, f))
            })
        })
        .collect()
}

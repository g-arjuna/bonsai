//! Schema-verified module-prefix checks for gNMI coverage matching
//! (M6F-C).
//!
//! gNMI `PathElem` names may carry a YANG module qualifier
//! (`srl_nokia-interfaces:interface`). Structural path matching
//! ([`crate::gnmi_path::observation_covers`]) compares LOCAL names only —
//! a matching local name is NOT matching schema identity: two different
//! modules can declare the same local name, and an arbitrary qualifier
//! must never establish positive coverage.
//!
//! This module binds observed qualifiers to the VERIFIED compiled
//! catalogue identity: for every qualified observed element, the schema
//! node at that path position (resolved through the catalogue's canonical
//! schema-path templates) must be declared by the module the qualifier
//! names — via the module's declared YANG prefix or its module name.
//! Unqualified observed elements carry no module claim and remain
//! governed by structural matching alone.
//!
//! Authority rules (fail closed):
//! - No verified catalogue for the plan's schema identity → every
//!   qualified element is unverifiable → no coverage.
//! - The observed template prefix does not resolve to exactly one
//!   declaring module (absent from catalogue, or ambiguous duplicate
//!   local identity) → unverifiable → no coverage.
//! - A qualifier that resolves to a DIFFERENT module than the declaring
//!   module → rejected.
//! - Uncertainty is preserved as non-coverage; identity is never
//!   invented.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::proto::gnmi::Path;
use crate::yang::YangLibrary;
use crate::yang_schema::{CompiledCatalogue, SchemaPathEntry};

/// Verified schema identity for one compiled catalogue: canonical
/// schema-template prefix → declaring module identity.
#[derive(Debug)]
pub struct CoverageSchemaIndex {
    /// Keyed by the observed path's LOCAL template prefix
    /// (`segments[0..=i]`): the schema node denoted by that prefix.
    nodes: HashMap<Vec<TemplateSeg>, NodeIdentity>,
}

/// Declaring-module identity of one schema node.
#[derive(Clone, Debug)]
enum NodeIdentity {
    /// Exactly one module declares this node.
    Unique { module: String, prefix: String },
    /// Multiple modules declare an identical template — the node's
    /// identity is genuinely ambiguous; no qualifier can verify it.
    Ambiguous,
}

/// One schema-template segment: local node name plus the set of list-key
/// names (values are instance data, never part of schema identity).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct TemplateSeg {
    name: String,
    keys: Vec<String>,
}

/// Split a gNMI element name into (qualifier, local name).
/// `None` = unqualified. Malformed qualifiers (empty parts, embedded
/// colons) are reported distinctly so callers fail closed.
enum ElemName<'a> {
    Unqualified(&'a str),
    Qualified { qualifier: &'a str, local: &'a str },
    Malformed,
}

fn split_elem_name(name: &str) -> ElemName<'_> {
    if !name.contains(':') {
        return ElemName::Unqualified(name);
    }
    match name.split_once(':') {
        Some((q, local)) if !q.is_empty() && !local.is_empty() && !local.contains(':') => {
            ElemName::Qualified {
                qualifier: q,
                local,
            }
        }
        _ => ElemName::Malformed,
    }
}

/// Parse a compiled-catalogue canonical schema path
/// (`/seg[k1][k2]/leaf`) into template segments. Key placeholders carry
/// names only; ordering is normalized by sorting.
fn parse_template(path: &str) -> Option<Vec<TemplateSeg>> {
    let mut out = Vec::new();
    for raw in path.split('/').filter(|s| !s.is_empty()) {
        let mut name_end = raw.len();
        for (idx, ch) in raw.char_indices() {
            if ch == '[' {
                name_end = idx;
                break;
            }
        }
        let name = &raw[..name_end];
        if name.is_empty() || name.contains(']') || name.contains('=') {
            return None;
        }
        let mut keys = Vec::new();
        let mut rest = &raw[name_end..];
        while !rest.is_empty() {
            let inner = rest.strip_prefix('[')?;
            let close = inner.find(']')?;
            let key = &inner[..close];
            // `name[]` marks a keyless list — it contributes no key names.
            if key.is_empty() {
                rest = &inner[close + 1..];
                continue;
            }
            if key.contains('=') || key.contains('[') || key.contains(']') {
                return None;
            }
            keys.push(key.to_string());
            rest = &inner[close + 1..];
        }
        keys.sort();
        out.push(TemplateSeg {
            name: name.to_string(),
            keys,
        });
    }
    (!out.is_empty()).then_some(out)
}

fn template_seg(elem: &crate::proto::gnmi::PathElem) -> Option<TemplateSeg> {
    let (ElemName::Unqualified(local) | ElemName::Qualified { local, .. }) =
        split_elem_name(&elem.name)
    else {
        return None;
    };
    if local.is_empty() {
        return None;
    }
    let mut keys: Vec<String> = elem.key.keys().cloned().collect();
    keys.sort();
    Some(TemplateSeg {
        name: local.to_string(),
        keys,
    })
}

impl CoverageSchemaIndex {
    /// Build the index over a compiled catalogue's canonical path
    /// entries. Malformed catalogue rows are skipped — they simply
    /// provide no verifiable identity (fail closed downstream).
    pub fn new(catalogue: &CompiledCatalogue) -> Self {
        let mut nodes: HashMap<Vec<TemplateSeg>, NodeIdentity> = HashMap::new();
        for entry in &catalogue.paths {
            insert_entry(&mut nodes, entry);
        }
        Self { nodes }
    }

    /// Verify every QUALIFIED element name in `path` against the
    /// declaring module at that schema position. Unqualified elements
    /// make no module claim and always pass this layer (structural
    /// matching already bound their local identity).
    ///
    /// Returns false — unverifiable, therefore non-covering — when any
    /// qualifier cannot be proven to name the declaring module.
    pub fn verify_path(&self, path: &Path) -> bool {
        let mut prefix: Vec<TemplateSeg> = Vec::with_capacity(path.elem.len());
        for elem in &path.elem {
            let Some(seg) = template_seg(elem) else {
                return false;
            };
            prefix.push(seg);
            let qualifier = match split_elem_name(&elem.name) {
                ElemName::Unqualified(_) => continue,
                ElemName::Qualified { qualifier, .. } => qualifier,
                ElemName::Malformed => return false,
            };
            match self.nodes.get(&prefix) {
                Some(NodeIdentity::Unique { module, prefix }) => {
                    if !qualifier_matches(qualifier, module, prefix) {
                        return false;
                    }
                }
                // Absent or ambiguous schema identity: the qualifier's
                // claim cannot be verified — preserve uncertainty.
                _ => return false,
            }
        }
        true
    }
}

fn insert_entry(nodes: &mut HashMap<Vec<TemplateSeg>, NodeIdentity>, entry: &SchemaPathEntry) {
    let Some(segs) = parse_template(&entry.path) else {
        return;
    };
    match nodes.entry(segs) {
        std::collections::hash_map::Entry::Vacant(v) => {
            v.insert(NodeIdentity::Unique {
                module: entry.module.clone(),
                prefix: entry.module_prefix.clone(),
            });
        }
        std::collections::hash_map::Entry::Occupied(mut o) => {
            let same = match o.get() {
                NodeIdentity::Unique { module, prefix } => {
                    *module == entry.module && *prefix == entry.module_prefix
                }
                NodeIdentity::Ambiguous => false,
            };
            if !same {
                o.insert(NodeIdentity::Ambiguous);
            }
        }
    }
}

/// A gNMI qualifier is verified when it names the declaring module —
/// either by the module's declared YANG prefix or by the module name
/// itself (devices variously qualify with either). Anything else is a
/// different module's claim and cannot establish coverage.
fn qualifier_matches(qualifier: &str, module: &str, module_prefix: &str) -> bool {
    (!module_prefix.is_empty() && qualifier == module_prefix)
        || (!module.is_empty() && qualifier == module)
}

/// True when the path carries at least one qualified element name —
/// the only case requiring schema verification.
pub fn path_has_qualifiers(path: &Path) -> bool {
    path.elem.iter().any(|e| e.name.contains(':'))
}

/// Source of verified compiled catalogues for coverage verification.
/// `schema_id = Some(id)` resolves the exact approved binding bundle;
/// `None` resolves the active catalogue (unbound/legacy plans).
pub trait CoverageSchemaSource: Send + Sync {
    fn catalogue_for(
        &self,
        schema_id: Option<&str>,
    ) -> anyhow::Result<Option<Arc<CompiledCatalogue>>>;
}

/// Production source backed by the configured YANG library. The
/// library handle is opened lazily on first use so the verifier never
/// creates catalogue directories for deployments that never compile.
pub struct YangLibraryCoverageSource {
    roots: (String, String, String),
    library: OnceLock<YangLibrary>,
}

impl YangLibraryCoverageSource {
    pub fn new(library_root: String, cache_root: String, bundle_key_env: String) -> Self {
        Self {
            roots: (library_root, cache_root, bundle_key_env),
            library: OnceLock::new(),
        }
    }

    fn library(&self) -> anyhow::Result<&YangLibrary> {
        if let Some(lib) = self.library.get() {
            return Ok(lib);
        }
        let (root, cache, key) = &self.roots;
        let lib = YangLibrary::open(
            std::path::Path::new(root),
            std::path::Path::new(cache),
            key.as_str(),
        )?;
        Ok(self.library.get_or_init(|| lib))
    }
}

impl CoverageSchemaSource for YangLibraryCoverageSource {
    fn catalogue_for(
        &self,
        schema_id: Option<&str>,
    ) -> anyhow::Result<Option<Arc<CompiledCatalogue>>> {
        let lib = self.library()?;
        match schema_id {
            // Approved binding identity: load exactly that verified
            // bundle — never silently substitute the active catalogue.
            Some(id) => crate::yang_schema::load_compiled(lib, id).map(|c| Some(Arc::new(c))),
            // Unbound plan: the active verified catalogue is the best
            // available schema identity. Absent/corrupt/partial-active
            // states yield no catalogue → qualifiers stay unverifiable.
            None => Ok(crate::yang_schema::active_catalogue_state(lib)
                .catalogue()
                .map(|c| Arc::new(c.clone()))),
        }
    }
}

/// Test fixture helpers shared with `subscription_status` coverage tests.
#[cfg(test)]
pub(crate) fn fixture_entry(path: &str, module: &str, prefix: &str) -> SchemaPathEntry {
    SchemaPathEntry {
        module: module.to_string(),
        module_revision: "2024-01-01".to_string(),
        namespace: "urn:test".to_string(),
        module_prefix: prefix.to_string(),
        path: path.to_string(),
        kind: "container".to_string(),
        config: false,
        declared_config: Some(false),
        mandatory: false,
        presence: None,
        type_name: None,
        resolved_type: None,
        units: None,
        default: None,
        description: None,
        keys: Vec::new(),
        min_elements: None,
        max_elements: None,
        ordered_by: None,
        required_features: Vec::new(),
        deviated: false,
        deviation_notes: Vec::new(),
        status: None,
    }
}

/// Two modules declare same-named top-level lists — the duplicate
/// local-name case the coverage fix must distinguish.
#[cfg(test)]
pub(crate) fn fixture_catalogue() -> CompiledCatalogue {
    CompiledCatalogue {
        schema_version: 1,
        schema_id: "ysc-test".to_string(),
        compiled_at_ns: 0,
        module_count: 2,
        path_count: 0,
        status: "complete".to_string(),
        modules: Vec::new(),
        paths: vec![
            fixture_entry(
                "/interface[name]",
                "srl_nokia-interfaces",
                "srl_nokia-interfaces",
            ),
            fixture_entry(
                "/interface[name]/statistics",
                "srl_nokia-interfaces",
                "srl_nokia-interfaces",
            ),
            fixture_entry(
                "/interface[name]/statistics/in-packets",
                "srl_nokia-interfaces",
                "srl_nokia-interfaces",
            ),
            // Same local name `interface` under a different parent —
            // a real second-module declaration that must NOT verify
            // qualifiers on the root-level `interface` node.
            fixture_entry(
                "/interfaces/interface[name]",
                "openconfig-interfaces",
                "oc-if",
            ),
        ],
        diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::gnmi::PathElem;
    use std::collections::HashMap;

    fn elem(name: &str, keys: &[(&str, &str)]) -> crate::proto::gnmi::PathElem {
        PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn path(elems: Vec<crate::proto::gnmi::PathElem>) -> Path {
        Path {
            elem: elems,
            ..Default::default()
        }
    }

    #[test]
    fn verified_prefix_accepted_unqualified_and_wrong_rejected() {
        let index = CoverageSchemaIndex::new(&fixture_catalogue());
        // SR Linux convention: qualifier on the list element, bare
        // container child.
        let srl = path(vec![
            elem("srl_nokia-interfaces:interface", &[("name", "mgmt0")]),
            elem("statistics", &[]),
        ]);
        assert!(index.verify_path(&srl));
        // Module name used as qualifier also verifies.
        let by_name = path(vec![elem(
            "srl_nokia-interfaces:interface",
            &[("name", "mgmt0")],
        )]);
        assert!(index.verify_path(&by_name));
        // Fully unqualified path: no module claim — passes this layer.
        let bare = path(vec![
            elem("interface", &[("name", "mgmt0")]),
            elem("statistics", &[]),
        ]);
        assert!(index.verify_path(&bare));
        // Wrong module qualifier on a duplicate local name: rejected —
        // `oc-if` is a real catalogue prefix but does not declare the
        // root-level `interface` node.
        let wrong = path(vec![elem("oc-if:interface", &[("name", "mgmt0")])]);
        assert!(!index.verify_path(&wrong));
        // Unknown qualifier: rejected.
        let unknown = path(vec![elem("other-module:interface", &[("name", "mgmt0")])]);
        assert!(!index.verify_path(&unknown));
        // Malformed double qualifier: rejected.
        let malformed = path(vec![elem("a:b:interface", &[("name", "mgmt0")])]);
        assert!(!index.verify_path(&malformed));
    }

    #[test]
    fn deeper_qualified_elements_verify_per_position() {
        let index = CoverageSchemaIndex::new(&fixture_catalogue());
        // Qualified leaf under verified parents.
        let deep = path(vec![
            elem("srl_nokia-interfaces:interface", &[("name", "mgmt0")]),
            elem("statistics", &[]),
            elem("srl_nokia-interfaces:in-packets", &[]),
        ]);
        assert!(index.verify_path(&deep));
        // A qualifier appearing only at a deeper position still resolves
        // through the full local template prefix.
        let deep_wrong = path(vec![
            elem("interface", &[("name", "mgmt0")]),
            elem("statistics", &[]),
            elem("oc-if:in-packets", &[]),
        ]);
        assert!(!index.verify_path(&deep_wrong));
    }

    #[test]
    fn absent_and_ambiguous_templates_fail_closed() {
        let mut cat = fixture_catalogue();
        // Duplicate local identity declared by a second module.
        cat.paths
            .push(fixture_entry("/interface[name]", "other-if", "other-if"));
        let index = CoverageSchemaIndex::new(&cat);
        // Even the correct qualifier cannot disambiguate an ambiguous
        // node — uncertainty is preserved, not resolved by guesswork.
        let qualified = path(vec![elem(
            "srl_nokia-interfaces:interface",
            &[("name", "mgmt0")],
        )]);
        assert!(!index.verify_path(&qualified));
        // A node absent from the catalogue cannot verify a qualifier.
        let missing = path(vec![elem("srl_nokia-interfaces:missing", &[])]);
        assert!(!index.verify_path(&missing));
    }

    #[test]
    fn multi_key_lists_verify_key_names_not_values() {
        let mut cat = fixture_catalogue();
        cat.paths.push(fixture_entry("/foo[name][slot]", "m", "m"));
        cat.paths
            .push(fixture_entry("/foo[name][slot]/bar", "m", "m"));
        let index = CoverageSchemaIndex::new(&cat);
        let ok = path(vec![
            elem("m:foo", &[("name", "a"), ("slot", "1")]),
            elem("bar", &[]),
        ]);
        assert!(index.verify_path(&ok));
        // Extra observed key changes the template — unverifiable.
        let extra_key = path(vec![
            elem("m:foo", &[("name", "a"), ("slot", "1"), ("extra", "x")]),
            elem("bar", &[]),
        ]);
        assert!(!index.verify_path(&extra_key));
        // Missing key likewise fails the template lookup.
        let missing_key = path(vec![elem("m:foo", &[("name", "a")]), elem("bar", &[])]);
        assert!(!index.verify_path(&missing_key));
    }
}

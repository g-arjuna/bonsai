# Effective YANG catalogue (M6) — scope and semantics

## Layers

1. **Source manifest** (`config/yang_sources.toml`): pinned, HTTPS-only
   upstream repositories (OpenConfig, YangModels/IETF/IEEE, Nokia SR
   Linux/SR OS, Cisco, Juniper, Arista). No moving-HEAD promotion.
2. **Module store**: immutable content-addressed blobs (SHA-256) with
   name+revision+content identity; same-name/revision conflicts are kept
   as distinct records, never overwritten.
3. **Parser** (`src/yang_schema/parser.rs`): RFC 7950/7951 lexer +
   recursive-descent parser producing a typed AST. Bounded input size,
   nesting depth and statement count; malformed input fails closed.
4. **Compiler** (`src/yang_schema/compiler.rs`): resolves imports,
   includes/submodules, uses/groupings (including top-level `uses`,
   recursively nested uses and cross-module groupings), augments,
   deviations, refines, features and typedef chains into a deterministic
   compiled catalogue plus canonical schema-path index.
5. **Compiled bundles** (`<library>/compiled/`): content-addressed
   (`ysc-*`) JSON with an atomic `active.json` pointer; activation and
   rollback are pointer switches. A failed compile never disturbs the
   active catalogue.

## Catalogue status

`CompiledCatalogue.status`:

- `complete` — zero diagnostics; every parsed construct resolved.
- `partial` — usable schema emitted; diagnostics record unresolved
  imports, uses targets, types, unsupported statements or revision
  mismatches. Partial catalogues are explicit and must not be treated as
  fully validated effective schemas.
- `failed` — no modules compiled.
- `unknown` — persisted bundle predating the status field.

Per-module `resolution` is `complete` or `partial` on the same basis.

## Supported YANG subset

Supported: modules/submodules, imports/includes, belong-to merging,
top-level and nested `uses` (bounded, cycle-safe), `refine`,
`augment`/`deviation` (subtree marking with provenance), `feature` /
`if-feature` gating (recorded per node; `uses`-level gates propagate to
the expanded subtree), typedef chains (bounded unwrap), built-in and
custom type names, identityref/leafref/union/enum/bits type specs,
units, defaults, descriptions, mandatory/optional, presence, min/max
elements, ordered-by, list keys (including `key "a b"` multi-key form),
config true/false inheritance, choice/case transparency in canonical
paths, status current/deprecated/obsolete, quoted/bare/single-quoted
strings with RFC 7950 §6.1.3 escape rules and `+` concatenation.

Explicitly unsupported or approximate (diagnosed or documented, never
silent where detectable):

- `deviate add/delete/not-supported` detail semantics — deviations mark
  the target subtree `deviated` with notes/status only.
- `when`/`must` expressions are captured as strings, not evaluated.
- Feature implication (`feature a { if-feature b; }`) is recorded but
  not transitively evaluated by `filter_by_features`; cross-module
  feature references match by written form.
- Import `revision` pins are diagnosed when they differ from the
  selected (newest) module revision; per-revision module selection is
  not implemented — compile keeps the newest revision per module name.
- `anyxml`/`anydata` are indexed; rpc/action/notification/input/output
  are parsed but not emitted as data-path entries (they are not
  instance-data nodes).
- Submodules merge by `belong-to` even without an explicit `include`
  (lenient vs RFC 7950; diagnosed only when belong-to is unresolvable).
- `extension` statements and extension instances are parsed/skipped;
  extension semantics are not interpreted.

## Path semantics

`SchemaPathEntry.path` is a **schema template**, not a gNMI instance
path: list keys appear as `[keyname]` placeholders
(`/interfaces/interface[name]/config/name`). Instance paths use
`elem[key=value]` syntax (`src/gnmi_path.rs`). M6C must instantiate keys
before a template can become a subscribable path. Entries carry module
name, revision, namespace and module prefix so identical unqualified
paths from different modules never collapse.

## Determinism

Compilation output is independent of input ordering: modules are keyed
by name in sorted maps, duplicate name+revision ties break on content
checksum then source ref, and paths are sorted before identity hashing.
`schema_id` is a SHA-256 over the sorted module set and path index.

## Resource bounds

Module input size, nesting depth, statement count, uses-expansion depth
(cycle-safe), resolution passes, path entries and compiled bundle size
are all bounded. Parser/compiler run as administrative operations only;
nothing in the live ingestion path compiles YANG.

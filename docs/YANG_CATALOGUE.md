# Effective YANG catalogue (M6) — scope and semantics

## Layers

1. **Source manifest** (`config/yang_sources.toml`): pinned, HTTPS-only
   upstream repositories (OpenConfig, YangModels/IETF/IEEE, Nokia SR
   Linux/SR OS, Cisco, Juniper, Arista). `pinned_ref` MUST be a full
   40-hex commit SHA — branches and tags are moving references and are
   rejected at manifest validation. No moving-HEAD promotion.
2. **Module store**: immutable content-addressed blobs (SHA-256) with
   name+revision+content identity; same-name/revision conflicts are kept
   as distinct records, never overwritten. Effective selection is an
   explicit, auditable policy: operator approval (`approve_module`)
   first, then trust rank, then earliest import (incumbent stability),
   then lowest checksum as a deterministic tiebreak — a newly imported
   equal-trust conflict can never silently displace the incumbent;
   promotion between equal-trust variants requires explicit checksum-
   specific approval. `set_module_trust` fails closed when a name/
   revision matches multiple conflicting records.
3. **Parser** (`src/yang_schema/parser.rs`): RFC 7950/7951 lexer +
   recursive-descent parser producing a typed AST. Bounded input size,
   nesting depth (enforced on every recursive path: node bodies, uses
   bodies, type bodies, feature/identity/typedef/grouping/augment/
   deviation/rpc/notification/statement lists) and statement count;
   malformed input — including unterminated modules — fails closed.
4. **Compiler** (`src/yang_schema/compiler.rs`): resolves imports,
   includes/submodules, uses/groupings (including top-level `uses`,
   recursively nested uses and cross-module groupings), augments,
   deviations, refines, features and typedef chains into a deterministic
   compiled catalogue plus canonical schema-path index.
5. **Compiled bundles** (`<library>/compiled/`): content-addressed
   (`ysc-<32 hex>`) JSON with an atomic `active.json` pointer. Bundle
   files are immutable: activation and load verify the embedded
   `schema_id` AND recompute the semantic identity from content — a
   corrupt, truncated, tampered or identity-mismatched bundle fails
   closed and can never become active. Rollback is a pointer switch to
   another verified retained bundle. A failed compile or activation
   never disturbs the active catalogue.

## Catalogue status

`CompiledCatalogue.status`:

- `complete` — zero diagnostics; every parsed construct resolved AND no
  pre-compilation failures (unreadable records, parse failures, record/
  content mismatches, checksum mismatches) were recorded.
- `partial` — usable schema emitted; diagnostics record unresolved
  imports, uses targets, types, unsupported statements, deviation or
  refine semantics not fully interpreted, or pre-compilation failures.
  Partial catalogues are explicit diagnostic information and must not be
  treated as fully validated effective schemas.
- `failed` — no modules compiled.
- `unknown` — persisted bundle predating the status field.

Per-module `resolution` is `complete` or `partial` on the same basis.
The HTTP compile endpoint only auto-activates `complete` catalogues;
`partial`/`failed` results remain browsable but require an explicit
`POST /api/yang/activate` (admin-only) to become the active pointer.

## Schema identity (`schema_id`)

`schema_id` is a SHA-256 over a canonical serialization of the
compiler version, catalogue status, all diagnostics, full module
metadata (name, revision, namespace, prefix, provenance, license,
platforms, pinned ref, source id, features) and every schema-path
entry (path, kind, config/state, mandatory, presence, type names,
resolved types, units, defaults, keys, element bounds, required
feature expressions, deviation markers). Nondeterministic metadata
(`compiled_at_ns`) is excluded. Any change that alters the meaning of
the compiled schema changes the identity; identical inputs — including
reordered module sets — produce identical identities.

## Supported YANG subset

Supported: modules/submodules, imports/includes, belong-to merging,
top-level and nested `uses` (bounded, cycle-safe), `refine` of the
supported property set (mandatory/config/default/description/units/
presence/min/max-elements) with full relative-path resolution and
diagnostics for unsupported substatements, `augment`/`deviation` with
namespace-aware target resolution, `feature` / `if-feature` gating
(including YANG 1.1 boolean `and`/`or`/`not` expressions,
canonicalized to the declaring module's prefix and inherited through
ancestor nodes and `uses` expansion), typedef chains (bounded unwrap),
built-in and custom type names, identityref/leafref/union/enum/bits
type specs, units, defaults, descriptions, mandatory/optional,
presence, min/max elements, ordered-by, list keys (unquoted `key a b;`
and quoted multi-key forms), config true/false inheritance,
choice/case transparency in canonical paths, status
current/deprecated/obsolete, quoted/bare/single-quoted strings with
RFC 7950 §6.1.3 escape rules and `+` concatenation.

Explicitly unsupported or approximate (diagnosed or documented, never
silent where detectable):

- `deviate add/delete/replace` semantics — only `deviate
  not-supported` is interpreted (the target subtree is pruned from the
  effective schema). Other `deviate` kinds mark the subtree `deviated`
  with notes/status AND emit an explicit diagnostic that keeps the
  catalogue `partial` — affected definitions must not be treated as
  authoritative.
- `when`/`must` expressions are captured as strings, not evaluated.
- Feature implication (`feature a { if-feature b; }`) is recorded on
  `CompiledFeature.if_features` but not transitively evaluated by
  `filter_by_features`; cross-module feature references are canonicalized
  to `declared_prefix:local` form.
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
- Deviations on augmented nodes resolve against the augmented tree;
  deviation targets that cannot be resolved unambiguously produce
  diagnostics (never a silent first-match).

## Path semantics

`SchemaPathEntry.path` is a **schema template**, not a gNMI instance
path: list keys appear as `[keyname]` placeholders
(`/interfaces/interface[name]/config/name`). Instance paths use
`elem[key=value]` syntax (`src/gnmi_path.rs`). M6C must instantiate keys
before a template can become a subscribable path. Entries carry module
name, revision, namespace and module prefix so identical unqualified
paths from different modules never collapse.

Even a `complete` catalogue describes the *source* schema only. It does
not prove any device implements a path — device applicability requires
M6C (YANG Library discovery, gNMI Capabilities correlation, feature and
deviation evaluation against the device).

## Determinism

Compilation output is independent of input ordering: modules are keyed
by name in sorted maps, duplicate name+revision ties break on content
checksum then source ref, feature expressions are canonicalized, and
paths are sorted before identity hashing.

## Resource bounds

Module input size, nesting depth, statement count, uses-expansion depth
and resolution passes, schema-path entries (`MAX_PATH_ENTRIES` —
saturation emits an explicit diagnostic and yields `partial`, never a
silently truncated `complete`), compiled bundle size and retained-bundle
count are all bounded. Git subprocesses during sync carry a 300-second
timeout. YANG compilation runs as administrative operations only
(`spawn_blocking` in HTTP handlers); nothing in the live ingestion path
compiles YANG.

## Authorization

`GET /api/yang/*` reads require Viewer (or scoped ApiReadonly where
applicable). `POST /api/yang/compile` and `POST /api/yang/activate`
require the Admin role — they mutate the authoritative catalogue.

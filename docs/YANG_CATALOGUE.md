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
compiler version (`COMPILED_SCHEMA_VERSION`, currently **4** — bumped
from 3 by the M6 final-review deviation-semantics correction),
catalogue status, all diagnostics, full module
metadata (name, revision, namespace, prefix, provenance, license,
platforms, pinned ref, source id, features) and every schema-path
entry (path, kind, config/state, mandatory, presence, type names,
resolved types, units, defaults, keys, element bounds, required
feature expressions, deviation markers). Nondeterministic metadata
(`compiled_at_ns`) is excluded, as are the request-provenance fields
`module_selection`, `module_scope` and `notices` — they record *how* a
bundle was requested, not *what* it contains, so identical effective
content deduplicates to one content-addressed bundle. Any change that
alters the meaning of the compiled schema changes the identity;
identical inputs — including reordered module sets — produce
identical identities.

`notices` are deterministic provenance records that are NOT schema
defects: routine revision/content dedup decisions (e.g. "module
'openconfig-interfaces' has multiple revisions; pinned '2022-10-25',
dropped '2026-10-05'"). They never affect `status` and never enter
`schema_id` — a pinned compile inside a shared multi-revision library
is `complete` and selectable exactly like a single-revision compile.
Real conflicts (unresolvable pins, same-authority distinct content)
remain in `diagnostics` and keep the catalogue non-authoritative.

### Compiler-version migration (v3 → v4)

Compiled bundles embed the compiler version; a bundle compiled under a
different `COMPILED_SCHEMA_VERSION` fails verification closed and is
never silently reinterpreted. After upgrading Core:

1. Every pre-upgrade compiled bundle (v3 and earlier) is rejected on
   load — `active.json` selection, device schema selections and
   schema-bound approvals referencing it fail closed, never grant
   authority.
2. Recompile the intended module set under the new compiler; the new
   content produces a new `schema_id`.
3. Explicitly reactivate the new bundle and reselect it per device;
   schema-bound approvals must be re-approved against the new schema —
   no old approval is treated as approval of the new schema.
4. Rollback is a binary rollback plus the same recompile/reselect
   steps; old bundle files are never edited to fake compatibility.

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

Vendor dialects observed in the wild are also accepted where they are
unambiguous supersets of the standard grammar (M6F-B, Nokia SR Linux
26.3.1 — the full 11-module `srl_nokia-*` closure compiles to 419
paths with zero diagnostics, schema `ysc-d7254b6e840b0f0041c4b2458bb509a1`):

- Unquoted `revision 2026-03-31 { ... }` dates.
- Prefixed extension statements and instances
  (`srl_nokia-ext:delivery-node-override "true";`).
- `presence "description";` and bare `presence;` forms.
- Bare `enum name;`, enum names containing `/`, and optional integer
  values inside enum blocks.
- Type arguments carrying `{ error-message "..."; }` sub-blocks
  (e.g. Nokia `pattern '...' + '...' { error-message ...; }`).
- Block-form statements skipped as unsupported now terminate at the
  matching `}` instead of swallowing following sibling statements
  (regression fixture `tests/fixtures/yang/test-nokia.yang`).

Explicitly unsupported or approximate (diagnosed or documented, never
silent where detectable):

- `deviate not-supported` removes the target node and its whole subtree
  from the effective schema (path index, feature gates and all).
- `deviate delete` removes only the listed PROPERTIES of the target
  node — the node itself stays (RFC 7950 §7.20.3.2; `delete` and
  `not-supported` are distinct operations). Per the RFC the deviable
  property set is exactly `config`, `default`, `mandatory`,
  `max-elements`, `min-elements`, `must`, `type`, `unique` and `units`;
  `delete` requires the keyword AND argument to match and removes only
  the matching occurrence of repeated properties (`must`, `when`,
  `unique`). Deleting a property outside the RFC set, a property whose
  argument does not match, or a property that is not present preserves
  the node, emits a diagnostic and keeps the catalogue `partial` — the
  deviation is NOT counted as applied. Both block (`deviate delete {
  ... }`, no trailing `;` — the form real-world vendor deviation
  modules such as `cisco-xr-openconfig-interfaces-deviations` use) and
  semicolon forms parse correctly.
- `deviate replace` writes the replacement VALUE into the effective
  node for the represented RFC-deviable scalar properties, and only
  when the property already exists on the target — replacing an absent
  property, an unrepresented property (`type`), or a sub-block form is
  diagnosed and keeps the catalogue `partial`.
- `deviate add` applies RFC-deviable scalar properties plus `must`/
  `when`/`unique` entries, but only when the property is absent —
  adding an existing singleton property or an unimplemented property
  is diagnosed and keeps the catalogue `partial`. Data-definition
  additions (`leaf`, `container`, ...) are never synthesized.
- Malformed deviation bodies are diagnosed, never silently applied:
  `deviate delete`/`add`/`replace` without a non-empty block,
  `deviate not-supported` carrying a block, and unknown `deviate`
  kinds all keep the catalogue `partial`.
- Applied deviation coverage is recorded per target module in
  `CompiledModule.deviation_modules` (deviation-module names whose
  `deviate` statements were fully honored). Applicability treats a
  device-reported deviation as covered ONLY when its module appears
  there; reported-but-uncompiled deviations remain
  `deviation_semantics_unsupported` and fail closed.
- `when`/`must` expressions are captured as strings, not evaluated.
- Feature implication (`feature a { if-feature b; }`) IS evaluated
  device-side in applicability (M6G): `if-feature` expressions use
  three-valued logic — a device-reported feature is effectively enabled
  only when every dependency is provably satisfied; unproven
  dependencies cap applicability at `unknown`, never positive.
  Circular dependency graphs (direct, multi-node, cross-module) are
  invalid per RFC 7950 §7.20.1 and are diagnosed at compile time,
  marking the module `partial` — they can never authorize paths.
  Feature atoms resolve against ALL modules carrying the lexical
  prefix; ambiguous or disagreeing resolution is `unknown`.
  `filter_by_features` (compile-time pruning) still does not expand
  implications transitively; cross-module feature references are
  canonicalized to `declared_prefix:local` form.
- Import `revision` pins are diagnosed when they differ from the
  selected revision. Per-revision module selection IS implemented
  (M6F-F): `CompileOptions.module_revisions` pins a module name to an
  exact revision date — unresolvable pins fail closed, and pins never
  substitute a different revision. `CompileOptions.module_scope`
  restricts compilation to named modules; out-of-scope library records
  are skipped before read/parse so unrelated malformed modules cannot
  poison a device-scoped bundle, while in-scope failures still
  diagnose. Both are exposed on `POST /api/yang/compile` via the
  optional `{module_revisions, module_scope}` body and recorded as
  bundle provenance; scoped compiles are never auto-activated — they
  become device authority only through the admin schema-selection API.
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

## Device applicability (M6C-1)

The compiled catalogue describes the *source* schema. M6C-1 adds a
deterministic, evidence-backed layer answering "which catalogue paths
can Bonsai justify as applicable **for this particular device**, on
which evidence, and with which uncertainty?"

### Evidence sources (kept distinct)

- **gNMI Capabilities** — complete `ModelData` entries
  (name/organization/version), gNMI service version, encodings,
  security mode and observation provenance are preserved in
  `CapabilityEvidence`. Capabilities establish *candidates only*: a
  gNMI `ModelData.version` is **not** a YANG module revision and is
  never compared as one.
- **RFC 8525 YANG Library** — bounded, read-only gNMI `Get` probes of
  `/yang-library` (NMDA module-set/schema/datastore model) with a
  legacy RFC 7895 `/modules-state` fallback. Both modern and legacy
  trees are reconstructed from arbitrary gNMI update shapes (prefixes,
  keyed elements, leaf-list encodings) under strict size/depth/count
  bounds. Outcomes are explicit: `complete`, `legacy`, `unsupported`,
  `permission_denied`, `incomplete`, `malformed`, `timeout`,
  `transport_failure`, `conflicting_module_definitions`,
  `unsupported_encoding`, `datastore_schema_missing`,
  `datastore_schema_ambiguous`. None of the negative outcomes is ever
  treated as an empty-but-complete module set.
- **Compiled catalogue** — only integrity-verified active bundles are
  used; `partial`/`failed`/`unknown` catalogues cap every conclusion
  below `applicable`.

### Resolution rules (`src/yang_schema/applicability.rs`)

- Module identity is matched on name + independently reported
  revision + namespace + organization. Absent evidence stays absent;
  a revision that cannot be resolved exactly yields `unknown`, never a
  newest-match guess. Conflicting namespace/organization/revision
  evidence yields `conflicted`.
- Feature gates reuse the compiler's expression machinery extended to
  tri-state evaluation: `if-feature` requirements resolve to
  `applicable` (all true), `not_applicable` (a required feature is
  provably disabled), or `unknown`/`feature_state_unknown` — unknown
  feature state is never interpreted as enabled.
- `deviate not-supported` pruning and property-level `deviate
  delete`/`add`/`replace` are honoured for the implemented property
  subset; a device-advertised deviation module caps affected paths at
  `unknown`
  with `deviation_semantics_unsupported` ONLY when that deviation
  module was not compiled into and fully applied against the target
  module (`CompiledModule.deviation_modules`). Compiled-and-applied
  deviations do not block applicability — their semantics were already
  evaluated at compile time.
- Import-only modules supply referenced definitions but classify as
  `not_applicable` for data-tree applicability.
- Datastore linkage is explicit: the evaluated datastore's schema →
  module-set mapping is selected (default `ietf-datastores:running`
  when unambiguous) and config/state classification is checked against
  it. Multiple datastore schemas are never silently merged.
  Per RFC 8342 §5 the axes are `running` = configuration only,
  `operational` = config+state superset, `candidate`/`startup`/
  `intended` = configuration only; an explicit `requested_datastore`
  is evaluated strictly, while default resolution unions coverage
  across all declared datastores. Unrecognized datastore identities
  (e.g. XRd's misspelled `ietf-datastores:candiate`) yield `unknown`
  axes — module membership may verify but config/state applicability
  stays unproven (`datastore_semantics_unverified`), never positive.
  Consequence: schema-bound acquisition of config-false state paths
  requires the device to declare a state-bearing datastore; devices
  advertising only `running` get `not_applicable` on state paths.
- RFC 8525 revision conflicts are scoped to each schema's union of
  referenced module-sets — different datastores may legitimately pin
  different revisions; conflicts within one effective schema are
  still rejected. `import-only-module` entries never conflict (RFC
  8525 permits multiple import-only revisions of one module name);
  implemented-module identity is (name, revision, namespace).
- Device identity comes from the registry binding (canonical address /
  external id / collector), never hostname or transport IP alone;
  ambiguous identity yields `conflicted` with `identity_ambiguity`.

### Status model

`applicable` requires: verified complete catalogue + authoritative
YANG-Library-backed module evidence + exact revision/namespace match +
feature gates satisfied + datastore linkage + no unsupported deviation
semantics. `candidate` covers capabilities-only or datastore-unlinked
evidence. `unknown`, `not_applicable`, `conflicted` and `stale`
(freshness) cover the remaining honest states. Every result carries
deterministic reason codes, `paths_total`/`paths_truncated` truth and
a content fingerprint over catalogue + device evidence.

### Persistence and API

Snapshots persist in `bonsai_config.db` (schema v9,
`yang_applicability_snapshots`) with bounded payloads, device-identity
binding, schema/content-id binding, evidence fingerprint, freshness
policy (24h evidence age + catalogue `schema_id` match) and bounded
history — historical snapshots are never silently erased. Freshness is
computed at read time; a stale snapshot reports `stale` with explicit
reasons rather than mutating history.

- `GET /api/devices/{address}/yang-applicability` — Viewer; reads the
  persisted snapshot only (never connects to the device); bounded
  `module`/`status`/`kind`/`limit`/`offset` filtering with truthful
  `paths_matching` counts.
- `POST /api/devices/{address}/yang-applicability/refresh` — Admin;
  performs the bounded capabilities + YANG Library probe through the
  existing credential/transport-security path and persists a new
  snapshot. Optional `{"datastore": "..."}` body selects the datastore.
  It never issues gNMI `Set`, never creates subscriptions and never
  changes acquisition intent.

### What this does not claim

Applicability is **schema-level**: it says a path is justified by
catalogue + device module-set evidence — it is not transport-probed,
not subscribed, not received and not projected. Remote-collector-only
devices are not discoverable from Core (explicit
`collector_discovery_required`-class limitation recorded for M6C-2).
Enforcing applicability into acquisition planning is M6C-2; the UI
schema browser and plan preview is M6D.

## Schema-bound acquisition workspace (M6D)

The M6D workspace (`ui/src/lib/SchemaCollectionWorkspace.svelte`,
mounted as the device drawer's schema tab and reused from Devices,
Profiles and Onboarding) consumes the M6C-1/M6C-2 surfaces above; it
introduces no second planner or schema authority.

- **Selection identity** is structured end to end: gNMI wire `origin`
  is a model origin (simple identifier), selection `provenance`
  records how the path was chosen (`manual_override`, profile
  recommendation, approved binding). Backend dedup uses
  `gnmi_path::canonical_key` — origin + ordered elements + sorted
  key/value pairs + mode + interval — so distinct list instances
  (`interface[name=Ethernet1]` vs `Ethernet2`) never collapse while
  exact duplicates and same-path parameter conflicts still reject.
- **Preview → activation concurrency**: preview returns
  `config_revision` (the device `updated_at_ns` the evaluation ran
  against); activation requires `expected_config_revision` and the
  registry commit atomically rejects any intervening device update
  with `config_revision_mismatch` (HTTP 409).
- **RBAC**: preview is Viewer-readable (read-only evaluation, no
  mutation, no device RPC); activate, revoke and applicability
  refresh require Admin plus an authenticated subject. `/api/auth/me`
  exposes the caller role to the UI, which disables — never merely
  hides — unauthorized controls.
- **Applicability filters**: `GET /api/devices/{address}/yang-applicability`
  accepts `status`, `module`, `kind`, `config` (`true`/`false`),
  `feature`, `q` (substring over path/module/namespace/type),
  `limit` and `offset`; `paths_matching` reports the truthful filtered
  total and the embedded snapshot never serializes the unpaginated
  path list.
- **Execution truth**: the UI renders desired → approved → assigned →
  submitted → synchronized → observed as separate stages from
  assignment/subscription-status data; approval alone never displays
  as observed telemetry.

## Authenticated remote-collector discovery (M6E)

When Core cannot reach a device directly, an Admin may request
applicability refresh through the device's **assigned authenticated
collector** (`executor=assigned_collector`, or `auto` when the
collector's live session advertises the contract). The flow:

1. `POST /api/devices/{address}/yang-applicability/refresh` validates
   the device and queues a **durable, bounded** discovery job
   (`yang_discovery_jobs`, schema v12) — returning `202 Accepted` with
   a job id, never blocking on device I/O.
2. The collector polls `PollYangDiscoveryJobs` on the existing
   authenticated registration channel (sessions advertising
   `yang_discovery_v1` at contract version ≥ 6). Jobs are dispatched
   stamped with the current session generation; pre-contract sessions
   receive `discovery_supported=false` and zero work.
3. The collector executes **read-only** gNMI Capabilities + RFC 8525
   YANG Library Get in an isolated bounded worker (never inside the
   assignment/heartbeat loop), then submits bounded evidence through
   `SubmitYangDiscoveryResult`.
4. Core independently validates the transport-attested collector
   principal, job identity/state/deadline, collector binding, session
   generation, authority epoch, assignment generation, canonical
   device identity, datastore, payload bounds and observation times —
   then resolves applicability through the **same** M6C-1 resolver and
   persists through the **same** snapshot path as Core-direct refresh.

Trust boundary: the collector gathers evidence only. It cannot approve
plans, declare authoritative paths, request arbitrary targets, or
submit for another collector. Device credentials travel only through
the existing secure assignment-delivery path — never inside job rows,
evidence or logs.

Job lifecycle states: `queued`, `dispatched`, `completed`,
`unsupported`, `denied`, `timed_out`, `failed`, `cancelled`,
`stale_authority`, `superseded`. Reassignment, deletion, reconnect,
deadline expiry, replay and out-of-order completion are all fenced;
restart reconciliation expires dead jobs and stale authority epochs.
Per-collector/per-device/global queue bounds apply.

**Evidence provenance (M6E-G):** snapshots record `provenance.executor`
(`core_direct` | `authenticated_collector` | `legacy_unknown`),
`observing_collector_id`, `discovery_job_id`,
`collector_session_generation`, `authority_epoch` and
`evidence_received_at_ns`. `collector_id` on a snapshot remains the
device's *assigned* collector — assignment alone never proves who
observed the device. Pre-M6E rows read as `legacy_unknown` and are
never silently upgraded.

**Authority invalidation (M6E-A1):** after any new snapshot persists,
`revoke_binding_superseded_by_evidence` re-reads the CURRENT binding at
the registry mutation boundary: an approval derived from the new
evidence is kept (`Current`), an approval derived from newer evidence
than the triggering snapshot is preserved (`NewerApproval` — stale
completions cannot revoke newer approvals), an older incompatible
approval is revoked through the fingerprint-guarded registry path, and
a concurrent binding change yields `ConcurrentlyReplaced`.

UI: the schema workspace exposes the executor selector, live job card
(state/collector/reason/snapshot), bounded status polling and the
resulting snapshot's provenance; refresh on a schema-bound device
requires explicit confirmation because it can invalidate the approval.

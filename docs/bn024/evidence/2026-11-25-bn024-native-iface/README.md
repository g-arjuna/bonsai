# BN-024 native interface-state normalizer — first native intelligence slice

Branch: `feat/bn024-native-interface-state` (worktree `bonsai-bn024-g`),
based on merged PR #85 (`1f15930`).

## What this is

`src/interface_normalizer.rs` — a native Rust normalizer for the
Cumulus Linux / SONiC FRR observation pair
(`vtysh -c "show int vrf all"` + `ip -d address show`, the exact command
pair SuzieQ `interfaces.yml` declares for `cumulus`/`sonic`,
`version: all`). It emits typed `interface.state` `TypedFact`s on the
BN-024-02 envelope (`trust: device_asserted`, `authority: none`,
namespace-aware `subject.network_namespace`).

## Source basis

- Pinned upstream: SuzieQ OSS @ `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`
  (crosswalk `suzieq.services.interfaces`, `suzieq.services.service`,
  `suzieq.config.textfsm_templates/cl_interfaces.tfsm`).
- License class: `study_and_reimplement` — independent Rust
  implementation; no upstream code copied.
- Fixture: genuine upstream capture
  `tests/integration/texttfsm/input/cl_interfaces.txt` @ same revision
  (sha256 `f1384d1b…ca28e`), split into the two command outputs under
  `tests/fixtures/bn024/interface-cumulus/` with `PROVENANCE.md`
  (Apache-2.0 `test_fixture_redistribution` class).
- Parser oracle: `scripts/bn024/interface_oracle.py` runs the pinned
  textfsm template plus the *unmodified* pinned
  `_entry_cumulus_sonic_cleaner` (stubbed imports only) and diffs
  per-interface fields against the Rust output.

## Differential oracle result

`compare` on the Cumulus fixture pair: **28/28 interfaces, all compared
fields match** (ifname set, adminState, state, type, mtu, speed,
macaddr, master/member_of, ipAddressList, ip6AddressList). First run
exposed and fixed four real divergences:

1. iproute2 `name@link` ifnames — upstream `([^:@ ]+)` strips the
   `@` suffix; stanzas merge into the base interface.
2. ip `-d` linkinfo `${type}` lines (`bond_slave state…`, `vxlan id…`,
   `vlan protocol…`, `macvlan mode…`) carry the type upstream; last
   match wins.
3. `Link ifindex` is not in the upstream grammar — must not set
   `member_of`.
4. Upstream `${type}` is a case-sensitive lowercase vocabulary —
   `Interface Type Other/Vxlan/VRF` never match upstream.

Documented deliberate deviations (typed payload is stricter than the
upstream string record):

- `speed` is `ObservedField<SpeedObservation>` with explicit
  `SpeedUnit`; the FRR speed field's scale is unproven for this format,
  so magnitudes are kept with `unit: unknown`. The `0xFFFFFFFF` FRR
  sentinel and `0` are explicit unavailability, not values.
- `_linux_data`'s `state "unknown" → "up"` loopback assumption is NOT
  ported: unknown stays unknown.
- Identity is namespace-aware from day one
  (`subject.network_namespace`); upstream collapses SONiC host/ASIC
  namespaces.
- Per-interface `vrf` is retained as a subject axis (upstream deletes
  it in the cleaner).
- `ip_addresses`/`ipv6_addresses` are typed `ObservedField<Vec<String>>`
  with upstream merge semantics: IPv4 first-source-wins, IPv6 prefers
  the ip-addr (`-_2nd`) list, `::1/128` removed.

## Coverage / diagnostics

`NormalizationReport` carries `interfaces_observed`, `facts_emitted`,
`duplicate_ifnames`, `merge_conflicts`, `unparsed_lines`, per-field
`field_coverage` tallies, and explicit `notes` (untyped extracted
fields: vlan/vni/link counters/status timestamps; speed-unit caveat;
unknown-release note). `NormalizationOutput.interfaces` provides
per-interface raw diagnostics (`InterfaceDiagnostic`) used by the
oracle. Empty observations emit one `PayloadState::Empty` fact;
oversized input (>8 MiB) is refused, not truncated.

## Operator surface (BN-030)

This slice is an offline ABI/normalization capability only — no UI,
no authority claim, no graph write. Operator presentation is deferred
to the owning backlog item for the interface-state feature; the typed
facts, coverage report and diagnostics defined here are the contract
that surface will consume. `trust` remains `device_asserted`; nothing
here is authority-eligible.

## Verification executed

- `cargo test -p bonsai --lib --profile ci -- interface_normalizer
  interface_facts typed_facts`: 45+ tests pass (incl. fixture
  normalize+validate, sentinel/zero speed, sonic internal marking,
  empty-observation, malformed-line accounting, namespace-bound fact
  identity, oversized refusal).
- Differential oracle: `interface_oracle.py compare` → all fields
  match (see above).
- Perf: `interface_normalizer::perf::fixture_normalization_throughput`
  (ignored by default; run explicitly) — see run record below.

## Review-fix pass (Stages A–E, post-`1130ef1`)

Confirmed review findings A1–A7 addressed in the normalizer/ABI:

- **A1 capture completeness**: `CaptureOutcome` per source command
  (Complete/Partial/Unavailable/Failed/TimedOut/Truncated/Unsupported/
  Unknown). Unusable captures are never parsed; `Unknown` parses but
  caps every fact at `Completeness::Unknown`; degraded outcomes cap at
  `Partial`. Entity/field conflicts and omissions also downgrade
  completeness — `Complete` is claimed only when nothing is missing.
- **A2 resource limits**: entity/address/unparsed/conflict bounds are
  counted in `NormalizationReport` (`entities_omitted`,
  `addresses_omitted`, `invalid_entities`, `facts_rejected`), never
  silent; oversized input (>8 MiB) is refused via an `InvalidInput`
  status fact.
- **A3 per-source conflicts**: cross-source disagreements are preserved
  as `SourceFieldConflict` evidence on the payload
  (`source_conflicts`) with effective/alternate source+value and
  resolution — never collapsed by first-wins.
- **A4 identity/relationships**: `member_of` is populated ONLY for
  independently-established LAG aggregates; `parent_interface` for
  physical parents; `master_relationship` records the classified
  target kind (Lag/Bridge/Vrf/Parent/Unresolved) for every reported
  master.
- **A5 admin vs operational**: admin state and operational state are
  separate `ObservedField`s; iproute2 header `state` is operational
  evidence (upstream ignores it — see oracle deviations), vtysh
  "line protocol" remains preferred; admin-down never implies
  oper-down.
- **A6 address validation**: IPv4/IPv6 entries validated per family;
  any invalid entry marks the whole address field `Invalid` (never a
  typed fact containing it); MAC normalized/validated.
- **A7 pre-return validation**: every emitted fact is validated
  against the canonical ABI; failures are counted in
  `report.facts_rejected`, never emitted.

Stage B seam: `NormalizationReport.applicability` records the
qualified profile scope (`platform=cumulus|sonic version=all`) —
provenance for profile-registry routing, explicitly NOT release
qualification (`version: all` remains an upstream selector claim;
unknown device release is noted, `release_raw: None` on fixtures).

Stage C: manifest entry added for the committed fixture split
(`suzieq-oss:tests/integration/texttfsm/input/cl_interfaces.txt`,
`test_fixture_redistribution`, hash-verified against the pinned
checkout); `interface_oracle.py compare` now understands the typed
payload (`member_of`/`parent_interface`/`master_relationship.target`,
iproute2 operstate evidence, `::1/128` retention) and prints
documented deviations separately from mismatches. Result on the
genuine fixture: **28/28 interfaces, all compared fields match**,
16 documented deviations (15 iproute2-operstate + 1 ::1/128).

Stage D: evidence digests are computed once per observation (not per
fact); the perf harness borrows fixture strings (no `Box::leak`).

Stage E: `AuthorityGrant::for_fact` hardened — requires a
resolver-attested resolution with `may_update_authoritative_state`,
device/scope match, and binds every subject/source axis; attestation
is never serialized. `PayloadState::CaptureFailed` added and treated
as degraded.

Fixture release honesty: `tests/fixtures/bn024/interface-cumulus/`
remains the only committed upstream-derived content; `PROVENANCE.md`
records origin/revision/sha256/split boundary and the manifest entry
above is its machine-checkable counterpart.

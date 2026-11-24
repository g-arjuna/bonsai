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

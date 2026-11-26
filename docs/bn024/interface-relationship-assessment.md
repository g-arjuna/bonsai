# BN-024-06 interface relationship assessment — first capability-led slice

Status: REVIEW_READY implementation candidate, focused offline analysis only.
Canonical L0–L3 gates: NOT_EXECUTED — deferred until independent review under
the approved review-first validation policy.

## Selected enterprise use case

The first capability-led increment is **LAG aggregate/member operational
consistency, with bridge, parent/master and evidence-based network-instance
relationship reporting**. The immediate operational questions are:

- Is an observed LAG member operationally down?
- Does the aggregate have reduced capacity because only part of its observed
  membership is up?
- Does the aggregate have no active members?
- Is an aggregate present and independently typed as a LAG in the same
  observation?
- Which interfaces have evidence-based bridge, parent/master or VRF/network
  instance relationships, and which relationships remain unresolved?

This was selected from the SuzieQ capability ledger rather than by parser
convenience. The ledger ranks structured interface hierarchy/LAG completeness
among the highest-value gaps. LLDP is ranked first, but Bonsai currently has
no genuine LLDP fixture, acquisition path or identity-reconciliation input;
starting LLDP without that evidence would produce another parser-shaped
artifact rather than a qualified enterprise capability. The committed genuine
Cumulus Linux interface fixture already contains independently inspectable
bond, bridge, macvlan/parent and VRF-master evidence, so the first useful
implementation can be verified against real captured output.

## Implementation boundary

Implementation: `src/interface_relationships.rs`.

The module consumes caller-selected typed `interface.state` facts and returns
a serializable report. It is intentionally pure analysis:

- no graph writer or second topology engine;
- no live device access or collector integration;
- no configuration inference;
- no API/UI exposure yet;
- no claim of SuzieQ `interfaces.assert` parity;
- no cross-NOS qualification claim.

Facts are grouped only by explicit typed subject axes: `device_id`, `scope`
and `network_namespace`. Interface names are not treated as global truth, and
a missing relationship target is reported rather than silently assumed.


## Important evidence-boundary clarification

`bond01` **is present in the genuine vtysh half** of the Cumulus capture,
but the source vocabulary extraction does not independently classify that
presence as a bond/LAG in the effective type evidence used for relationship
classification. `swp1` therefore remains an unresolved master relationship
rather than silently becoming LAG membership. This is a useful negative
fixture for fail-closed evidence handling, not a claim that the device lacks
`bond01` or that an iproute2 absence alone proves nonexistence.

## Deterministic verdicts

Per-scope verdicts use the feature-family contract required for operational
assertions:

- `pass` — at least one LAG assessment exists and no fail/unknown condition
  is present;
- `fail` — one or more LAG members are down, including explicit findings for
  `lag_member_down`, `lag_reduced_capacity` or `lag_no_active_members`;
- `unknown` — required evidence is missing or unresolved, for example the
  aggregate is absent/not independently typed as a LAG or member state is
  unavailable;
- `not_applicable` — no LAG membership evidence exists in the scope.

The implementation deliberately does not treat missing evidence as health.
The current initial consistency rules are conservative and narrow; they are
not yet a complete SuzieQ-style interface assertion family.

## Genuine versus synthetic evidence

Genuine evidence:

- `tests/fixtures/bn024/interface-cumulus/vtysh-show-int-vrf-all.txt`
- `tests/fixtures/bn024/interface-cumulus/ip-d-address-show.txt`

The genuine Cumulus fixture independently shows `swp49`/`swp50` as members of
`peerlink`, bridge membership for `bond02`/VNI interfaces, parent/macvlan
forms and `evpn-vrf` master evidence. It also contains a deliberately useful
negative case: `swp1` reports raw master target `bond01`. `bond01` is present
in the vtysh half, but the qualified source extraction does not independently
establish it as a bond/LAG in the effective type evidence used for
classification. The
existing normalizer preserves that as an unresolved relationship rather than
inventing LAG membership. The regressions verify both the established
`peerlink` aggregate/member relationship and the unresolved `bond01` case
without fabricating missing evidence.

Synthetic evidence:

- one focused regression mutates the already-normalized genuine `swp49` fact
  to `operational_state: down` to test reduced-capacity behavior. This is
  a synthetic negative control and is not represented as device evidence.
- structured non-zero subinterface identity regressions in
  `src/typed_facts.rs` are synthetic structural ABI tests because neither
  the current genuine FRR fixture nor the genuine XRd fixture contains a
  non-zero subinterface.

## Relationship to the shared identity contract

The additive `subject.interface_identity` contract is validated separately
in `src/typed_facts.rs` and `tests/test_bn024_typed_facts.py`. Existing FRR,
Cumulus and XRd producers currently leave the refinement absent because their
genuine inputs do not establish non-zero subinterface identity. This is an
intentional fail-closed choice: dotted interface names are never split to
manufacture parent/subinterface axes. The analyzer nevertheless honors the
typed subject axes that already exist — device, scope and network namespace —
and consumes only validated typed facts.

## Remaining limitations

1. Only offline Cumulus relationship evidence is exercised in this slice.
2. Standalone FRR and XRd are not qualified for LAG consistency because the
   current genuine evidence does not establish equivalent aggregate/member
   relationships.
3. There is no production acquisition integration beyond the existing FRR
   interface acquisition path, and that path does not yet expose this report.
4. There is no operator/API surface. Under BN-030, this backend-only slice is
   not operator-ready; primary exposure should later be the existing interface
   workspace/DeviceDrawer evidence surface with coverage, unknown, stale and
   unsupported states.
5. Bridge/VLAN reporting is structural evidence, not forwarding correctness
   proof. It does not prove VLAN consistency, bridge forwarding, MLAG peer
   state or hardware offload.
6. VRF attachments are reported only when present in typed subject/master
   evidence; no naming-convention inference is performed.
7. SuzieQ's full interface assertion semantics, LLDP reconciliation, ARP/ND,
   MAC movement and VLAN mismatch scenarios remain missing.

## Next implementation dependency

The next useful increment should add genuine LLDP evidence for at least one
materially different NOS and reconcile LLDP neighbor identity against these
typed interface identities. Until such evidence exists, implementing an LLDP
parser alone would not satisfy the enterprise use case.


## Focused verification evidence

Executed through the trusted shared ci build lane with prebuilt lbug, one
heavyweight Cargo owner:

```text
cargo test -p bonsai --lib --profile ci --offline typed_facts
cargo test -p bonsai --lib --profile ci --offline interface_relationships
cargo test -p bonsai --lib --profile ci --offline interface_normalizer::tests
cargo test -p bonsai --lib --profile ci --offline frr_interface_json::tests
cargo test -p bonsai --lib --profile ci --offline xrd_openconfig_interfaces::tests
```

Results: typed-facts 42 passed / 2 ignored artifact generators;
interface-relationships 2 passed; interface-normalizer 21 passed;
FRR structured JSON 20 passed; XRd OpenConfig 34 passed. Total focused
execution: **119 passed, 0 failed, 2 ignored** in 1086 s wall. Compilation
dominated wall time because Cargo unexpectedly rebuilt the same test unit for
each filter invocation in this lane; this is logged in
`docs/build-times/runs.csv` as an efficiency anomaly, not hidden.

Additional schema compatibility checks:

```text
/home/arjuna/Desktop/bonsai/.venv/bin/python -m pytest -q tests/test_bn024_typed_facts.py
```

Result: **9 passed** in 2.88 s final recheck. JSON artifacts parse; the
capability ledger's disposition and domain summary counts recompute exactly
from its entries; `git diff --check` passes. Canonical L0–L3 remain
NOT_EXECUTED under the review-first policy.

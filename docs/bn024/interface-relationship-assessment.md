# BN-024-06 interface relationship assessment — first capability-led slice

Status: MERGE_READY implementation candidate, focused offline analysis only.
Canonical L0–L3 gates: PASS on frozen implementation candidate
`89382018f3c4e14886c7a55fd28fb236c62ac9cd` against base
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`. L4 remains NOT_EXECUTED; this slice
has no live acquisition or operator-surface acceptance boundary.

## Selected enterprise use case

The first capability-led increment is **evidence-consistency assessment for
LAG aggregate/member relationships, with bridge, parent/master and
evidence-based network-instance relationship reporting**. The immediate
questions are deliberately evidence questions, not forwarding-health claims:

- Is an aggregate present and independently typed as a LAG in the same
  coherent observation batch?
- Do aggregate-reported (`lag_members`) and member-reported (`member_of`)
  membership axes agree, disagree, or remain incomplete?
- Are the operational states of resolved member interfaces known, up, down,
  lower-layer-down, dormant, testing or unknown?
- Are interface entity identities distinct across physical interfaces,
  subinterface zero, numbered subinterfaces and network namespaces?
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

Facts are first grouped by explicit typed subject axes: `device_id`, `scope`
and `network_namespace`. Within each scope they are additionally bounded by
the coherent observation batch axes collector, source kind, exporter, capture
id and session epoch. Multiple unreconciled batches for the same scope are
preserved separately and can never merge into a passing verdict. Withdrawn,
superseded, non-operational-plane and expired-before-observation facts are
excluded from current evidence and reported as input issues. Interface names
are not treated as global truth; entity keys include the structured interface
identity refinement when present, and a missing/ambiguous/conflicting
relationship target is reported rather than silently assumed.


## Important evidence-boundary clarification

`bond01` **is present in the genuine vtysh half** of the Cumulus capture,
but the source vocabulary extraction does not independently classify that
presence as a bond/LAG in the effective type evidence used for relationship
classification. `swp1` therefore remains an unresolved master relationship
rather than silently becoming LAG membership. This is a useful negative
fixture for fail-closed evidence handling, not a claim that the device lacks
`bond01` or that an iproute2 absence alone proves nonexistence.

## Deterministic verdicts

Per-scope verdicts use the corrected deterministic contract:

- `pass` — a narrow bounded consistency verdict: required aggregate,
  membership and member-state evidence is complete enough, internally
  consistent, and free of unresolved/coverage/input-contract defects. It does
  **not** mean LACP selection, collecting/distributing state, forwarding
  participation, intended member count, active membership or usable capacity.
- `fail` — a proven evidence inconsistency, such as an incompatible LAG
  target type, a numbered subinterface selected as the aggregate, or
  contradictory forward/reverse membership evidence. An operationally down
  member is observed state (`observed_member_oper_down`); it is not by itself
  a consistency failure, and administrative down is not treated as a fault.
- `unknown` — required evidence is missing, ambiguous, stale/lifecycle
  excluded, incomplete, conflicting or otherwise unresolved. Unknown aggregate
  state, unknown membership, mixed observation batches, degraded completeness
  and unresolved relationship evidence all remain `unknown`.
- `not_applicable` — only a coherent scope with no LAG/aggregate evidence and
  no unresolved evidence suggesting applicability. Unresolved evidence never
  becomes `not_applicable`.

The implementation deliberately does not treat missing evidence as health or
success. Positive results are evidence-consistency results only; the analyzer
does not claim reduced capacity, no active members or active LAG forwarding
state. The current rules are conservative and narrow; they are not yet a
complete SuzieQ-style interface assertion family.

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

- typed synthetic regressions cover physical/subinterface-zero/numbered
  subinterface coexistence, namespace separation, duplicate replay,
  permutation determinism, mixed capture/session epochs, lifecycle exclusion,
  unknown aggregate/member evidence, forward/reverse membership agreement,
  conflicting same-generation observations, unresolved evidence and
  administrative/operational down terminology. These are labeled structural
  controls and are not represented as device evidence.
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

Corrective-review execution through the trusted shared ci build lane with
prebuilt lbug, one heavyweight Cargo owner:

```text
cargo test -p bonsai --lib --profile ci --offline interface_relationships
```

Result after the semantic rewrite: **17 passed, 0 failed** in 0.01 s test
execution following a 3m49s compile
(`docs/bn024/evidence/2026-11-26-pr91-corrective/interface-relationships-final-focused.log`).
A repeated identical invocation then reused the Cargo fingerprint and completed
in 7.13 s total wall time with no recompilation
(`interface-relationships-cache-probe.log`), confirming the measured build.rs
worktree fingerprint correction. Earlier focused evidence from the superseded
review candidate is historical and does not validate the corrected analyzer.

Capability-ledger structural validation passes with 66 entries; implementation
completion is separated into 1 completed existing equivalent, 1 completed
narrow native port, 0 completed improvements, 1 planned-but-incomplete
improvement, 20 partial and 43 not-implemented port-required capabilities
(`capability-ledger-validation.log`).

Canonical L0–L3 gates were executed through the trusted shared ci lane with
prebuilt lbug on frozen implementation candidate
`89382018f3c4e14886c7a55fd28fb236c62ac9cd` against base
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`:

```text
scripts/ci/build-lane.sh --lbug-native "$HOME/.cache/bonsai-ci/lbug-native" -- \
  env CARGO_BUILD_JOBS=2 \
  PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome \
  GATE_BASE=4c100ba56a06dfe57a5aed65c3920eb5c894757e \
  scripts/ci/gates.sh l0 l1 l2 l3
```

Result: **L0 PASS, L1 PASS, L2 PASS, L3 PASS** in 28m21.78s wall time, maximum
RSS 6,589,888 KiB, exit 0. L2 executed 45 Rust suites with 1362 passed, 0
failed and 10 ignored; this includes the bonsai lib result of 724 passed, 0
failed and 6 ignored. L3 executed 13 Rust integration/browser targets with 71
passed and 0 failed, followed by root Python 66 passed and SDK Python 318
passed plus 1 explicitly skipped and 1 deselected test. L4 remains
**NOT_EXECUTED** because this offline analyzer slice has no live lab or
operator-surface acceptance boundary. Evidence:
`docs/bn024/evidence/2026-11-26-pr91-corrective/canonical-l0-l3-8938201.log`.

The first canonical attempt on corrected candidate `d7796c3` failed L2 in
`typed_facts::tests::structured_interface_identity_is_bound_into_authority`:
semantic payload validation ran before the authority-binding check and masked
the required authority error. The authority integrity check was moved ahead of
semantic payload consistency validation, the focused regression passed, and
the full canonical run above passed on the frozen descendant. The superseded
`d7796c3`/`a2360e0` logs are retained as failure/interrupted-candidate evidence
only; they do not validate the final candidate.

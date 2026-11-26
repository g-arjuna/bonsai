# SuzieQ functional capability delivery assessment

Pinned upstream: `netenglabs/suzieq` `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` (Apache-2.0). Machine ledger: `suzieq-capability-delivery.v1.json`; structural inventory: `suzieq-upstream-inventory.v1.json`.

## Counting method

- Capability-family denominator: **27 upstream object families** — 19 polled service YAML families plus 8 derived/virtual sqobject families.
- Individual functional capability denominator: **66 capabilities**. Of these, **64** are implemented upstream and **2** are enterprise/Bonsai-required improvements absent upstream.
- Capabilities were generated from actual upstream service YAML, Avro schemas, cleaner functions, sqobject methods and pandas engine methods; they are not one-per-file or one-per-schema-field counts.
- No single overall parity percentage is meaningful across unlike domains. Use the explicit numerators/denominators in this table.

## Domain summary

| Functional domain | Upstream capabilities | Bonsai equivalent | Partial | Missing | Unassessed |
|---|---:|---:|---:|---:|---:|
| Neighbor intelligence | 4 | 0 | 0 | 4 | 0 |
| Routing | 10 | 1 | 3 | 6 | 0 |
| Assertions | 4 | 1 | 1 | 3 | 0 |
| Configuration | 3 | 0 | 3 | 1 | 0 |
| Device intelligence | 10 | 0 | 2 | 8 | 0 |
| Overlay | 3 | 0 | 0 | 3 | 0 |
| Interface intelligence | 4 | 1 | 2 | 1 | 0 |
| Layer 2 | 11 | 0 | 0 | 11 | 0 |
| Network reasoning | 4 | 0 | 2 | 2 | 0 |
| Path reasoning | 3 | 0 | 1 | 2 | 0 |
| Collection assurance | 4 | 0 | 4 | 0 | 0 |
| Historical analysis | 4 | 0 | 2 | 2 | 0 |

“Bonsai equivalent” includes `EXISTING_EQUIVALENT`, narrow `NATIVE_PORT_IMPLEMENTED` slices and intentional improvements. It does not imply production parity across all NOS releases. `DELIBERATELY_EXCLUDED` count is zero in this ledger.

## Disposition counts

| Disposition | Count |
|---|---:|
| `EXISTING_EQUIVALENT` | 1 |
| `EXISTING_PARTIAL` | 20 |
| `IMPROVE_UPON_UPSTREAM` | 1 |
| `NATIVE_PORT_IMPLEMENTED` | 1 |
| `PORT_REQUIRED` | 43 |

## High-value missing enterprise capabilities

1. **LLDP adjacency normalization and interface reconciliation** (`lldp.acquisition`).
2. **Structured interface hierarchy/LAG/subinterface/VRF completeness** (`interfaces.acquisition`).
3. **ARP/IPv6 ND neighbor intelligence** (`arpnd.acquisition`).
4. **VLAN/bridge/MAC forwarding relationships** (`vlan.acquisition`, `macs.acquisition`).
5. **Cross-NOS device-polled BGP operational state** complementing BMP (`bgp.acquisition`).
6. **Route-aware recursive LPM path resolution** (`routes.lpm`, `path.l3`).
7. **EVPN/VNI/VTEP operational normalization** (`evpnVni.acquisition`).
8. **Complete deterministic interface/BGP/OSPF/EVPN assertions** beyond the initial offline LAG-consistency slice, with explicit unknown and coverage.
9. **Unified collection-assurance ledger** across all acquisition sources.
10. **Configuration semantic interpretation**, absent upstream and still required for Bonsai.

## Defined-progress subset examples

These narrow ratios are useful because their numerators and denominators are
explicit; they are not overall parity percentages:

- Interface semantic acquisition requirements represented by this ledger:
  **1 of 4** interface-intelligence capabilities at narrow implemented/equivalent
  level, **2 of 4** partial and **1 of 4** missing.
- Interface assertion requirements: **0 of 1** complete; the initial offline
  LAG-consistency slice is partial and not operator-exposed.
- Qualified interface acquisition sources in the cross-NOS matrix: **2 of 8**
  originally assessed mature NOS families have any genuine interface
  implementation evidence (FRR and offline XRd); this is evidence presence,
  not production qualification. The newer SR OS/RouterOS/OcNOS roadmap targets
  are excluded because they have no assessed genuine source evidence in this
  session.

## Post-ledger implementation movement

`interfaces.assert` moved from `PORT_REQUIRED` to `EXISTING_PARTIAL` in this
same review candidate: `src/interface_relationships.rs` implements a narrow
offline LAG aggregate/member consistency slice using genuine Cumulus evidence.
This does not satisfy the broader interface assertion family and has no
operator/API exposure yet.

## Interpretation

- Bonsai is ahead in streaming BMP/RIB history, exporter/session identity, typed evidence digests, explicit unknown/completeness states, authenticated ingress and deterministic incident evidence foundations. Those advantages do not replace missing operational normalizers.
- SuzieQ is materially broader in polled operational tables: interfaces, LLDP, VLAN, MAC, ARP/ND, OSPF, routes, EVPN, MLAG, inventory, filesystem/process/time and assertions. Most remain `PORT_REQUIRED`.
- Important upstream limitations are preserved in the machine ledger: silent normalize defaults, first-match/version-all selection, questionable SONiC/Linux/Cumulus cleaner reuse, lossy route protocol mapping, whole-config hashing, and poll-miss lifecycle ambiguity.
- `nos_service_depth` distinguishes upstream selector existence, copy aliases, version selectors, command count, extraction mechanism and vendor cleaner existence. Bonsai implementation/acquisition/query booleans are conservative and exact-release scoped. It intentionally records XRd's genuine offline fixture/test evidence without marking generic IOS XR implementation or production acquisition true; XRd is a specific virtual implementation family and `iosxr` production qualification remains false.

## Recommended implementation waves

**Wave 1 — interface and adjacency truth:** structured subinterface identity, parent/LAG membership, VRF/network-instance attachment, LLDP, VLAN/bridge relationships and address ownership.
**Wave 2 — neighbor and routing evidence:** ARP/ND, MAC, route-aware LPM, recursive next-hop resolution, BGP operational state complementing BMP and explicit collection coverage.
**Wave 3 — assertions and operator scenarios:** interface/MTU/LAG/VLAN/LLDP/address assertions with deterministic verdicts and BN-030 evidence surfaces.
**Wave 4 — overlay and advanced routing:** EVPN/VNI/VTEP, underlay recursion, OSPF/IS-IS/LSDB topology using BN-026 contracts.
**Wave 5 — configuration semantics:** lossless AST, effective IR, policy/VRF semantics and change-impact explanation.



## Artifact structural validation

Focused validation in this review candidate: both JSON artifacts parse;
domain/disposition summary counts recompute exactly from the capability
entries; the capability ledger contains 66 entries with exactly the
disposition counts shown above (43 `PORT_REQUIRED`, 20 `EXISTING_PARTIAL`, 1
`NATIVE_PORT_IMPLEMENTED`, 1 `EXISTING_EQUIVALENT`, 1
`IMPROVE_UPON_UPSTREAM`), every capability has the required mapping fields and
non-empty acceptance scenarios. This is structural consistency validation,
not proof of semantic correctness or production qualification.

## Verification for this artifact

Focused artifact validation: JSON parse passes for the inventory and delivery
ledger; 66 entries present; disposition counts recompute exactly as 43
`PORT_REQUIRED`, 20 `EXISTING_PARTIAL`, 1 `NATIVE_PORT_IMPLEMENTED`, 1
`EXISTING_EQUIVALENT` and 1 `IMPROVE_UPON_UPSTREAM`; per-domain summary counts
recompute exactly from the entries; every capability has required mapping
fields and non-empty acceptance scenarios; the top-missing list contains
exactly 10 entries. This validates structural consistency only.

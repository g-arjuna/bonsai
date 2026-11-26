# Cross-NOS interface intelligence qualification matrix

Date: 2026-11-25/26 UTC; PR #90 closeout corrected 2026-11-26 UTC. This matrix records actual evidence and implementation
state; empty cells are not aspirational claims. Qualification terminology uses
the repository D/I/N/S/P/V/O convention: design, implementation, normalization,
scale, production, vendor-release and operational qualification.

| NOS family | Genuine source evidence currently available | Native interface implementation state | Exact release qualification | Acquisition status | Semantic coverage established | Remaining blocker |
|---|---|---|---|---|---|---|
| FRR (standalone) | Genuine authorized FRR 10.6.1 `show interface json` fixture and pre-correction live SSH evidence from PR #89 | Production shadow acquisition, bounded persistence, typed facts and first operator panel merged in PR #89 | Exact lab artifact profile for FRR `10.6.1_git` structured JSON | Core-managed authorized SSH slice; post-correction live re-execution not performed because credentials were rotated; L4 remains not executed | Base interface identity, admin/oper state, ifindex, type, MTU, speed Mbps, MAC, IPv4/IPv6, partial parent/master relationships; degraded outcome semantics | Live corrected-candidate browser/API journey and L4 matrix |
| Cisco IOS XR / XRd | Genuine XRd 24.4.2 gNMI capabilities and subscribe-once evidence; immutable image `ios-xr/xrd-control-plane:24.4.2` id `sha256:b3bd8e…a487f` | **Merged PR #90:** offline Rust OpenConfig normalizer plus registry/profile wiring; experimental shadow only (production head `be277f9`, merge commit `4c100ba`) | Fixture/image provenance exact, but no device-reported release/build identity in capture; qualified registry dispatch intentionally fails closed | Read-only live evidence acquisition only; canonical L0–L3 PASS on `be277f9`, L4 NOT_EXECUTED; no production Core-managed acquisition/persistence | Base interface identity, admin/oper, ifindex, type, interface MTU, forced/negotiated speed semantics, MAC, subinterface-0 IPv4; unknown population; non-zero subinterfaces explicitly unsupported | Production acquisition integration, structured subinterface identity, network-instance/VRF/LAG extraction, UI exposure |
| Nokia SR Linux | Historical genuine gNMI/YANG evidence (`2026-11-21-m6f-srlinux`, `2026-11-23-m6fc-srlinux-r8`); simple CLI fixture exists but is weak | No native interface normalizer implemented | No exact interface-feature release qualification | Existing lab state was broken in this session (`clab-m6f-srlinux-srlinux` exited; netns present but services refused connections) | None in common typed interface model | Repair/deploy authorized lab, choose native versus OpenConfig schema scope, implement Rust adapter |
| Arista EOS | cEOS 4.37.0F explicit-mode live counter/withdrawal evidence; CLI fixture exists but is not sufficient live qualification | No native interface normalizer implemented | No exact interface-feature release qualification | Explicit acquisition evidence only; cEOS lacks YANG Library in documented evidence, so not schema-bound authority | None in common typed interface model | Decide structured EOS JSON/OpenConfig source, establish authority semantics, implement adapter |
| Cisco IOS XE | No current genuine versioned live evidence located in this session; generic CLI parser references are not interface-feature qualification | No native interface normalizer implemented | None | None | None | Authorized image/lab or genuine versioned capture |
| Juniper Junos | Text CLI fixture exists; no current genuine versioned live evidence located in this session | No native interface normalizer implemented | None | None | None | Authorized device/image or genuine structured XML/JSON evidence |
| SONiC | Upstream SuzieQ selector evidence and architectural requirements; no qualified modern STATE_DB/COUNTERS_DB/gNMI interface evidence in this slice | No native interface normalizer implemented; must not inherit FRR qualification | None | None | None | Genuine namespace-aware SONiC evidence and lab; ASIC/host namespace model |
| NVIDIA Cumulus | Historical genuine Cumulus SuzieQ CLI capture used by the original offline normalizer; no modern NVUE/kernel/FRR combined qualification | Legacy FRR-style CLI offline normalizer plus first offline LAG/bridge/parent/VRF relationship assessment in this validated implementation candidate (`src/interface_relationships.rs`); not modern Cumulus native state | Fixture-only upstream capture; not current Cumulus release qualification | No live modern Cumulus acquisition | Partial offline CLI semantics plus narrow offline LAG aggregate/member consistency over genuine fixture; bridge/parent/VRF structural reporting | Modern authorized image/evidence for NVUE/kernel/FRR relationship; production/API/operator exposure |
| Nokia SR OS | Roadmap target added through BN-024-08D research; no genuine interface evidence located in this session | No native interface normalizer implemented | None | None | None | Approved evidence/lab and source contract; do not infer from Nokia SR Linux |
| MikroTik RouterOS | Roadmap target added through BN-024-08B research; no genuine interface evidence located in this session | No native interface normalizer implemented | None | None | None | Approved evidence/lab/API contract; do not assume OpenConfig/gNMI |
| IP Infusion OcNOS | Roadmap target added through BN-024-08C research; no genuine interface evidence located in this session | No native interface normalizer implemented | None | None | None | Approved evidence/lab/source contract |

## Capability-level outcome after the first non-FRR adapter

| Semantic capability | FRR structured JSON | XRd OpenConfig gNMI | Cross-NOS finding |
|---|---|---|---|
| Canonical device identity | Qualified shadow slice: canonical inventory identity, not management address | Typed input requires canonical device context; no production persistence yet | Shared model is sound, but production identity lifecycle must be common acquisition infrastructure rather than per-NOS copies |
| Base interface identity | Implemented | Implemented from OpenConfig list key | Compatible |
| Subinterface identity | FRR dotted/relationship evidence remains limited | Non-zero subinterfaces explicitly unsupported; index 0 addressing folded only as base-interface addressing | Additive typed-fact contract exists in this validated implementation candidate, but current genuine FRR/XRd evidence does not contain non-zero subinterfaces and producers intentionally emit no refinement; producer integration remains open |
| Admin/oper state vocabulary | Implemented | Implemented | OpenConfig token names differ but map cleanly without collapsing unknown |
| Interface type | Implemented | Implemented for observed ethernetCsmacd/other scope | Classification depends on both IANA type and `logical`; vendor-neutral mapping needs more scenarios |
| MTU | Implemented | Implemented as interface-level value | Units are bytes in both qualified sources, but XRd exposes distinct subinterface IPv4 MTU; layering must remain explicit |
| Speed | Implemented Mbps | Implemented Mbps from OpenConfig `SPEED_*` | Shared numeric+unit representation validated across CLI structured JSON and model-driven source |
| MAC | Implemented | Implemented when ethernet subtree exists | Unsupported/absent behavior validated on `Null0` |
| IPv4/IPv6 | Implemented | IPv4 implemented for subinterface index 0; IPv6 extraction implemented but unknown in genuine capture | Address ownership depends on subinterface modeling; cannot mature without structured subinterface identity |
| Namespace | FRR default namespace slice | Context supports namespace but genuine lab evidence is default only | SONiC/Cumulus will stress this axis; not yet cross-NOS qualified |
| VRF/network instance | FRR subject binding where source reports it | Not extracted; OpenConfig interfaces root does not establish network-instance attachment | Requires separate network-instance source and explicit conflict retention |
| LAG relationships | Partial FRR only | Not implemented | First narrow offline Cumulus fixture assessment implemented in this candidate; no XRd/FRR/cross-NOS qualification |
| Capture completeness | CLI command response plus degraded outcomes | gNMI initial sync plus non-atomic subtree merge; population denominator unknown | Completeness semantics are source-specific and must not be forced into one command-response envelope |

## PR #90 canonical closeout

PR #90 merged as `4c100ba56a06dfe57a5aed65c3920eb5c894757e`; final production
head `be277f97be8500d43fa724e0352d207ea26746ff`. Canonical L0–L3 passed on that
exact head; L4 remains NOT_EXECUTED. Durable evidence:
`docs/merger/evidence/2026-11-25-bn025-xrd-native-interfaces/canonical-l0-l3-be277f9.log`.
This does not change the qualification boundary: the XRd implementation is an
experimental offline/shadow normalizer, not production acquisition or an
operator-ready Cisco interface capability.

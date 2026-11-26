# XRd residual model-driven findings for the next shared increment

Date: 2026-11-26 UTC. Applies to merged PR #90 production head
`be277f97be8500d43fa724e0352d207ea26746ff` (merge commit
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`). These are **not** reasons to reopen
the merged offline XRd qualification by themselves. They are recorded here so
the next shared model-driven implementation resolves them with focused
regressions instead of creating another correction-only XRd cycle.

The existing boundary remains: experimental offline/shadow normalization only;
no production acquisition, authoritative graph projection or operator-ready
Cisco interface workspace claim.

| ID | Disposition | Finding and required correction |
|---|---|---|
| XRD-R1 | **Reproduced by inspection; assigned to next shared model-driven increment** | `canonical_json_key()` in `src/xrd_openconfig_interfaces.rs` strips the module qualifier for all keys in `openconfig-interfaces`, `openconfig-if-ethernet` and `openconfig-if-ip` without constraining module ownership to the schema path at which the leaf/augmentation is valid. Recognized module membership is therefore necessary but not sufficient. Reuse the effective YANG schema architecture for path-aware module qualification, or add a narrowly reviewed explicit module/path map if the offline contract cannot yet support schema-bound decoding. Required tests: valid augmentation at its applicable location, recognized module at an invalid location, foreign module with matching local leaf name, and conflicting module ownership across notifications. Do not build a second YANG compiler. |
| XRD-R2 | **Reproduced by inspection; assigned to next shared model-driven increment** | `decode_capture()` accepts a response containing `sync_response` after checking only its boolean value, without visibly rejecting contradictory update/delete content in the same response object. `atomic` and `duplicates` are read through optional typed extraction, so malformed non-boolean/non-integer metadata can be treated as absent. Required correction: enforce protocol response-variant shape; reject sync plus notification content, non-boolean atomicity metadata, malformed duplicate-count metadata and unexpected response-type combinations. Production processing should reuse typed protobuf representations rather than handwritten JSON protocol validation. |
| XRD-R3 | **Reproduced by inspection; assigned to structured-interface identity increment** | `payload_identity_errors()` corroborates `state.name` and `config.name` but does not visibly check the interface root-level list identity. Optional string extraction can also treat malformed identity-bearing values as absent. Required regression: path key `Ethernet1` with payload root name `Ethernet2` must not become unambiguous evidence for `Ethernet1`; invalid identity types and conflicting identity leaves across notifications must fail closed or remain conflicted. This belongs with the structured subinterface identity contract because root and subinterface identity must be modeled coherently. |
| XRD-R4 | **Confirmed semantic mismatch by inspection; assigned to typed-fact temporal-contract increment** | `TypedFact.valid_from`/`valid_until` are documented as a state validity window, while the XRd adapter derives them from earliest/latest notification timestamps. The period over which notifications were received is acquisition/observation evidence, not proof that merged interface state was valid throughout that interval. Required correction: preserve capture/source observation interval separately; do not imply state validity across the merge interval unless the source semantics establish it. This matters for historical queries and deterministic path analysis. |
| XRD-R5 | **Partially corrected; residual consumer-risk assigned to shared capture-completeness contract** | Missing synchronization no longer becomes ordinary empty success. However, a synchronized envelope with a partial/truncated outer capture outcome can still select `PayloadState::Empty` with degraded completeness. Required correction: where emptiness is not established by both capture and synchronization contracts, use explicit unknown/degraded output rather than a state consumers could read as confirmed absence. Add consumer-level regression coverage. |

## Focused regression matrix required in the implementing PR

- Valid module-qualified augmentation at its correct schema path.
- Recognized module-qualified leaf at an invalid schema path.
- Foreign module using a recognized local leaf name.
- Sync response plus update/delete content.
- Non-boolean `atomic` and malformed/non-integer `duplicates` metadata.
- Root path/payload interface-name mismatch, invalid identity types and conflicts across notifications.
- Separate observation/acquisition interval from fact validity semantics.
- Partial/truncated outer capture with terminal sync must not be represented as confirmed empty absence.

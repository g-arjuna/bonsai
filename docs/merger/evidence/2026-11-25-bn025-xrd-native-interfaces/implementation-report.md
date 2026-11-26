# XRd OpenConfig interface normalization implementation report

Date: 2026-11-25 UTC; closeout corrected 2026-11-26 UTC.
Branch: `feat/bn025-xrd-native-interfaces` — **merged as PR #90** in merge commit
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`. Final production head:
`be277f97be8500d43fa724e0352d207ea26746ff`.

## Live evidence acquisition

A fresh dedicated authorized XRd lab container `clab-bn025-xrd-xrd` was probed
read-only at `172.100.106.21:9339` using plaintext lab-only gNMI. Image evidence:
`ios-xr/xrd-control-plane:24.4.2`, image id
`sha256:b3bd8e8ea4892d4dd9b0a32c223990965f6f874b6b7ebf732b27dd5ab3ca487f`.
Capabilities succeeded: gNMI 0.10.0, JSON_IETF/ASCII/PROTO, 1064 supported
models. Relevant model revisions: openconfig-interfaces 3.0.0,
openconfig-if-ethernet 2.12.1, openconfig-if-ip 3.5.0 and Cisco deviations
2024-02-21.

The root subscribe-once capture is byte-identical to the committed fixture:
sha256 `eca6f4efaf0c54d46679a36ea0c92b02de2af02cc8ba3a60623e60c269259486`.

## Implemented slice

- Rust semantic extraction in `src/xrd_openconfig_interfaces.rs`.
- Exact revisioned registry/profile wiring in `src/normalizer_registry.rs` and
  `src/nos_features.rs`.
- Genuine fixture and provenance under `tests/fixtures/bn025/` and this evidence
  directory.
- Human/machine cross-NOS matrices in `docs/bn024/`.

The adapter intentionally remains offline/shadow-only. Production acquisition,
persistent shadow lifecycle, shared operator API/UI, graph projection,
non-zero subinterface identity, network-instance/VRF attachment and LAG
relations are not implemented in this slice.

## Verification executed

### Independent owner-review reproduction (pre-correction)

The focused regression suite was first run against the pre-correction adapter
and failed as expected: **20 failed / 5 passed** in 245 s through the trusted
prebuilt ci lane. The failures independently reproduced the review findings:
wrong model origin acceptance, namespace injection/collision first-wins,
false-empty success for invalid identities, omitted address entries disappearing
from accounting, unobserved Ethernet state misclassified as unsupported,
missing/invalid gNMI timestamp handling, terminal-sync and delete lifecycle
gaps, insufficient update bounds, and prerequisite conflicts not invalidating
dependent fields.

### Corrected adapter

`cargo check -p bonsai --lib --profile ci --offline` through the trusted
prebuilt build lane: **PASS** in 56 s.

`cargo test -p bonsai --lib --profile ci --offline xrd_openconfig_interfaces::tests`
through the trusted prebuilt build lane: **PASS**, 31/31 tests in 264 s wall
(compilation dominated; test execution 0.18 s). The broader risk-linked filter
`cargo test -p bonsai --lib --profile ci --offline xrd_openconfig` also passed
**32/32**, adding the registry experimental-dispatch/fail-closed qualification
regression. Coverage includes the genuine
XRd expected values plus negative regressions for wrong/conflicting origins,
unbound path targets, atomic/delete lifecycle, terminal sync ordering,
malformed/missing timestamps, duplicate JSON object identity keys,
module-qualified namespace collisions, foreign identityref namespaces, invalid
interface identities, path/payload identity mismatches, actual update-count
bounds, YANG uint16/uint32 bounds, exact enum case, non-boolean prerequisites,
subinterface key/state mismatches, address-entry omission accounting, conflict
prerequisites, and missing Ethernet state (`Unknown`, not `Unsupported`).

The parser/mapping/requirement revisions for the corrected semantics are
`xrd-oc-iface-2026.11-r2`, `bn025-iface-map-2`, and
`bn025-iface-requirements-r2`.

## Canonical validation closeout (2026-11-26)

Canonical L0–L3 were subsequently executed through the trusted shared build lane
against the exact corrected production head
`be277f97be8500d43fa724e0352d207ea26746ff`:

```text
env CARGO_BUILD_JOBS=2 \
  PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome \
  GATE_BASE=origin/main scripts/ci/gates.sh l0 l1 l2 l3
```

Results: **L0 PASS, L1 PASS, L2 PASS, L3 PASS**. The branch's implementation
merge-base was `54d4cb65140ffbf202190f67b5c3b736345a5572`; PR #90 was then
merged onto main after the documentation-only roadmap/policy commits
`237b707d535e2d067f51f636f1ec5fd7bf4cdd75` and
`2e569cbab294ce6ed21dce107ad3baaba19aa7ee`, producing merge commit
`4c100ba56a06dfe57a5aed65c3920eb5c894757e`. L4 was **NOT_EXECUTED** and remains
unexecuted.

Retained durable evidence: `canonical-l0-l3-be277f9.log` in this directory
(sha256 `8b69493d713187693e4e9d47732dda26f1b5cf73503c0e440802a6c0cc42ff06`).
The log was copied byte-identically from `/tmp/bn025-gates-l0-l3.log` after a
secret-pattern scan found only test names/constants, not credential material.
Counting each executed test target in that log, including targets repeated by
separate canonical tiers, there were **1,409 Rust test executions passed,
0 failed and 10 ignored**, plus **382 Python tests passed** (64 + 318),
**1 skipped** and **1 deselected**; these are execution counts, not unique-test
counts. The focused corrected XRd suite remained **32/32** under the broader
`xrd_openconfig` filter.

No browser/API operator-ready claim is made. This remains an experimental
offline/shadow normalization slice. Production acquisition, durable shadow
lifecycle integration, structured non-zero subinterface identity,
network-instance/VRF attachment, LAG relationships, cross-NOS relationship
coverage and shared operator/API integration remain outstanding. Residual
model-driven validation findings are recorded for the next shared increment in
`docs/bn024/xrd-residual-model-driven-findings.md`.

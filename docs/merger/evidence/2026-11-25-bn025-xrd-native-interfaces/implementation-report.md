# XRd OpenConfig interface normalization implementation report

Date: 2026-11-25 UTC. Branch: `feat/bn025-xrd-native-interfaces`.

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

`cargo check -p bonsai --lib --offline` through the trusted prebuilt build lane:
PASS before registry/test finalization.

`cargo test -p bonsai --lib xrd_openconfig --profile ci --offline` through the
trusted prebuilt build lane: PASS, 5 tests:

- genuine XRd capture expected values;
- missing sync marks completeness unknown;
- malformed envelope is invalid input, not empty success;
- wrong acquisition contract rejected;
- registry experimental dispatch is shadow-only and qualified dispatch fails
  closed without exact device-reported release qualification.

An earlier implementation iteration failed the genuine fixture test before the
recursive namespace-canonical merge correction; that failure was fixed and the
final source passes. Canonical L0-L3 and L4 were **not** executed for this
branch in this bounded session. No browser/API operator claim is made.

# BN-024 LLDP fixture provenance

Pinned upstream: `https://github.com/netenglabs/suzieq` at immutable revision
`75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`, Apache-2.0. A byte-identical copy
of the upstream `LICENSE` is preserved here as `LICENSE.apache-2.0`.

These are genuine historical upstream capture slices, not synthetic device
emulation and not evidence of current production acquisition.

| local file | upstream container | upstream container sha256 | extracted slice sha256 | capture metadata |
|---|---|---:|---:|---|
| `cumulus-leaf02-net-show-lldp-json.txt` | `tests/integration/sqcmds/cumulus-input/ospf-single/lldp.output` | `52155e19834d7f198789136f2043483249c3b9ad8e5d1a4ac065f6e5134ee12c` | `2370a6e4119487393fd50976956baf7ef3bbc75636cd275b6e6172103912336b` | Cumulus Linux 4.3.0, host `leaf02`, namespace `ospf-single`, command `net show lldp json`, timestamp `1616352403891`, status `0` |
| `eos-leaf02-show-lldp-neighbors-detail.json` | `tests/integration/sqcmds/eos-input/lldp.output` | `dc8d43de9c592fe2d729bfb80d866993b212db39592311871e0a0584ec8a85ba` | `0cc3d0b79e8cf2a2757dc8b261c0402d95ba324c85658962fef5e76a5464113b` | EOS host `leaf02`, namespace `eos`, command `show lldp neighbors detail`, timestamp `1623025176214`, status `200`; upstream metadata records `version: null`, so the local EOS release is explicitly unknown and must not be fabricated |
| `capture-metadata.json` | extracted sidecar generated during BN-024 fixture preparation | n/a | `3ed2e74f15da9b6d9669de078ac62fe9c19252cf809b47e6b9755ef2ee083976` | exact capture metadata copied from the upstream record wrappers |

Extraction/modification status: the two device-output files are exact output
payload slices copied without content alteration from larger upstream capture
containers. The containers themselves are not vendored. The sidecar is derived
metadata. The license file is unmodified.

Reuse review: recorded as `approved_reuse` / `test_fixture_redistribution` in
`docs/bn024/provenance/upstream-sources.json` for offline regression evidence
only. This approval does not qualify live collection, any current NOS release,
SONiC, generic EOS, or generic Cumulus behavior.

Independent expected values used by tests are read directly from the pinned
fixture bytes: Cumulus contains exactly two neighbor-bearing interfaces
(`swp1`, `swp2`); EOS contains seven local interface keys, four neighbor
records and three positive empty-interface arrays (`Ethernet3`, `Ethernet4`,
`Management1`). Ethernet5/Ethernet6 are parallel leaf02-leaf01 observations and
must remain distinct.

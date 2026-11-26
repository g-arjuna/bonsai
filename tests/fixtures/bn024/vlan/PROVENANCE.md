# BN-024 VLAN/bridge fixture provenance

All command payloads in this directory are extracted slices from the pinned upstream SuzieQ OSS integration inputs at revision `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` (Apache-2.0). The upstream files are concatenated per-device capture arrays; extraction selected the exact device/command slice named below and did not alter record semantics. JSON slices are serialized with sorted keys for deterministic offline use. NXOS `data` is the upstream JSON-native string decoded once into JSON because the command output itself is JSON. The license text is redistributed byte-identically beside the fixtures.

| Fixture | Upstream input | Device/release | Command | Capture ms | Extracted SHA-256 |
|---|---|---|---|---:|---|
| `eos-leaf02-show-vlan-brief.json` | `tests/integration/sqcmds/eos-input/vlan.output` | `eos/leaf02`, release unknown in fixture | `show vlan brief` | 1623025174532 | `af73f4b4c16c467acf45c1b7f7b7d5a035281aff9316c2ba0434dc836e378d78` |
| `nxos-leaf03-show-vlan-brief-json-native.json` | `tests/integration/sqcmds/nxos-input/vlan.output` | `nxos/leaf03`, upstream wrapper release `9.3(4)` | `show vlan brief \| json native` | 1619275257446 | `21afcf20a44ba59c1a7b19606434151a7cf4ba521834c096a958612f7021e9ef` |
| `cumulus-leaf01-bridge-c-vlan-show.txt` | `tests/integration/sqcmds/cumulus-input/dual-bgp/vlan.output` | `cumulus/leaf01`, upstream wrapper release `4.3.0` | `/sbin/bridge -c vlan show` | 1616638469693 | `2d930d12f8686f17d1ad8014b62267d3acf04860db526d689da09183e65dc10f` |

`capture-metadata.json` preserves the wrapper timestamps, status, namespace and release metadata for these slices. These fixtures are offline regression evidence only. They are not live-device proof, do not establish support for other NOS versions, and do not authorize graph projection. `LICENSE.apache-2.0` is unmodified upstream license content (sha256 `c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`).

# BN-024 ARP/ND and MAC fixture provenance

All command payloads in this directory are extracted slices from the pinned upstream SuzieQ OSS integration inputs at revision `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` (Apache-2.0). The upstream files are concatenated per-device capture arrays; extraction selected the exact device/command slice named below and did not alter record semantics. JSON slices are serialized with sorted keys for deterministic offline use. The license text is redistributed byte-identically beside the fixtures.

| Fixture | Upstream input | Device | Command | Capture ms | Extracted SHA-256 |
|---|---|---|---|---:|---|
| `eos-leaf02-show-arp-vrf-all.json` | `tests/integration/sqcmds/eos-input/arpnd.output` | `eos/leaf02` | `show arp vrf all` | 1623025177307 | `b11bfd397473cfc19ff133ddb947dbf49a2f498d944031bdfbcbdce92de9f1c2` |
| `eos-leaf02-show-mac-address-table-unicast.json` | `tests/integration/sqcmds/eos-input/macs.output` | `eos/leaf02` | `show mac-address-table unicast` | 1623025175378 | `bcacd44a6ed2969f864b3d5b9c52e1e2b2db1e739fbb76765c7b443e7f1ac1ef` |
| `nxos-leaf03-show-ip-arp-detail-vrf-all.json` | `tests/integration/sqcmds/nxos-input/arpnd.output` | `nxos/leaf03` | `show ip arp detail vrf all \| json native` | 1619275263495 | `b67f8019d2f2b45c4eab3d15a43bb3423e741281d9824bf1d17b2ca4d43d92a0` |
| `nxos-leaf03-show-mac-address-table.json` | `tests/integration/sqcmds/nxos-input/macs.output` | `nxos/leaf03` | `show mac address-table \| json native` | 1619275257018 | `dee438f6392641c4b964041268f5f1f50209cc16f7ede75350464ab6f2ce3931` |
| `cumulus-spine02-ip-neigh-show.txt` | `tests/integration/sqcmds/cumulus-input/dual-bgp/arpnd.output` | `cumulus/spine02` | `ip neigh show` | 1616638468510 | `75e373f07b50aa08bff691f2645d47b53466b34bd92ec8268746ef667f4d053c` |
| `cumulus-leaf01-bridge-fdb-show.txt` | `tests/integration/sqcmds/cumulus-input/dual-bgp/macs.output` | `cumulus/leaf01` | `/sbin/bridge fdb show` | 1616638470427 | `a3063aefb5662e63c17a56eb7934d18e3f2e1ede313ff41d09917c727441b415` |

These fixtures are offline regression evidence only. They are not live-device proof, do not establish support for other NOS versions, and do not authorize graph projection.

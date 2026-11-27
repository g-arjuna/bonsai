# BN-006 FRR genuine fixture provenance

## Source identity

- Device/container: `clab-bonsai-bn003-vrf-frr1` (container ID at inspection: `db97140ef7064ff5170fa22171158abc2885f7e75b466641601412c04cdbf677`).
- Image reference: `quay.io/frrouting/frr:10.6.1`.
- Image digest: `quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b`.
- Image ID: `sha256:3b5561b2cf344dbb92f22ef124fbb7a7787ff9bd199828a27597d81c32aa1653`.
- Reported software release: FRRouting `10.6.1_git` (`version.json`).
- Reported build configuration: see the complete `configureLine` value in `version.json`; it is preserved verbatim as source evidence.

## Read-only acquisition commands

The structured artifacts were acquired from the existing local lab container with read-only commands equivalent to:

```sh
docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show bgp vrf all summary json'
docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show version json'
docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show ip route vrf all json'
```

No device configuration, credential, BMP injection or lab lifecycle mutation was performed for this fixture.

## Artifact identity and timestamps

The original vtysh completion timestamp was not independently emitted by FRR and is therefore not fabricated here. The exact workspace artifact write/retention timestamps observed when the fixture was preserved were:

| artifact | workspace write timestamp | SHA-256 |
|---|---|---|
| `bgp-vrf-all-summary.json` | 2026-11-27 20:51:55.496475935 +0530 | `1a368cc2b95976269d44c6a5e4f8f2b1cf949090b49131cd2f9bbd40d47a351b` |
| `version.json` | 2026-11-27 20:51:55.543474949 +0530 | `d47b1405f58c0d135d533e23c3f995575ab4b411a018ac130a8bf8dd715908ba` |
| `ip-route-vrf-all.json` | 2026-11-27 20:58:42.390684000 +0530 | `8ef50939156e46a329bb27f96c891aaf565cad635f8b4338b9d2064695956238` |

These are artifact retention timestamps, not independently proven device observation times.

## Qualification boundary

This is genuine structured FRR source evidence captured on 2026-11-27 during the existing local FRR/BMP lab operation. The captured BGP sessions were `Active`/`Idle`; therefore this fixture validates multi-VRF identity, parser normalization and non-established session semantics only. It does not establish established-session corroboration, verified-SSH acquisition through Bonsai, route forwarding correctness, or live operational qualification. Established-session reconciliation controls must remain separately labelled synthetic evidence.

# Genuine FRR structured-interface fixture provenance

Source classification: genuine authorized lab observation, read-only.
This is standalone FRR evidence, not SONiC or NVIDIA Cumulus compatibility
evidence.

- Capture time: 2026-11-25 (UTC, host session)
- Container: `clab-bonsai-bn003-vrf-frr2` (pre-existing Bonsai containerlab workload)
- Image reported by prior inventory: `quay.io/frrouting/frr:10.6.1`
- Immutable image identity: `quay.io/frrouting/frr@sha256:813497af103e972600f6161012b3bf2a2a1c5ea10d09cd1eacd74f1dc510a57b`
  (container image id `sha256:3b5561b2cf344dbb92f22ef124fbb7a7787ff9bd199828a27597d81c32aa1653`).
  `10.6.1_git` is a development-build version string; the digest above is
  the immutable qualification identity. A different image reporting the
  same version string is NOT assumed to have identical JSON semantics.
- Capture execution status: success (exit 0, complete single response);
  re-verified byte-identical against the live container on 2026-11-25
  (`docker exec ... vtysh -c 'show interface json' | sha256sum` =
  `b61b12df…`, matching the committed fixture).
- Committed fixture sha256: `b61b12df7142a9fcfacb4718eb9fc2b3210cd91a68122a87254dd093855c489a` (`show-interface-json-frr-10.6.1.json`, 2,796 bytes)
- Companion release evidence sha256: `7467578f71aa34a12db3a5803fab5a976944c89fa3fe087a310328cbe77371ef` (`show-version-json-frr-10.6.1.json`)
- Observed VRF/namespace scope: the container's single default network
  namespace / VRF `default`; the capture is not multi-namespace evidence.
- Sanitization/transformation: none — the committed fixture is the raw
  command response byte-for-byte. No redaction was required (lab-only
  addressing). Fixture redistribution classification: lab-operational
  data, no credentials or customer identifiers.
- Command: `docker exec clab-bonsai-bn003-vrf-frr2 vtysh -c 'show interface json'`
- Companion release evidence command: `docker exec clab-bonsai-bn003-vrf-frr2 vtysh -c 'show version json'`
- Release reported by companion output: FRRouting `10.6.1_git`, hostname `frr2`
- Capture file: `show-interface-json-frr-10.6.1.json`
- Release file: `show-version-json-frr-10.6.1.json`
- Mutation: none; the commands are read-only operational queries.
- Atomicity: single command response; no cross-command snapshot claim.
- Caveat: `eth1` and `eth2` are pseudo-interface records with partial operational
  detail. Their missing values must remain unknown, not fabricated.

## Format semantics checked

FRR 10.6.1 `zebra/interface.c` (`if_dump_vty_json`) was inspected for the
field contract at upstream tag `frr-10.6.1` (commit prefix `71da51b`).
`index` is the kernel ifindex, `mtu` is bytes, `speed` is
FRR's interface speed in Mbps with zero meaning no established speed, and
`parentInterface`/`masterInterface` are explicit relationship names when
present. Numeric `parentIfindex`/`masterIfindex` values are resolved only
when the target interface is present in the same observation. The fixture
is genuine evidence; expected values in tests were independently read from
this capture rather than copied from normalizer output.

Upstream coverage note: SuzieQ pinned at
`75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` does not provide a standalone
FRR `show interface json` normalizer. Cumulus/SONiC interface support uses
the text command pair instead. No upstream differential comparison is
therefore claimed for the structured adapter.

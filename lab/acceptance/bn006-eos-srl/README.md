# BN-006 EOS and SR Linux acceptance

This dedicated lab uses cEOS 4.36.0F, SR Linux 26.3.1-410 and an independent
Python BGP source. The EOS startup configuration exports default and BLUE VRF
post-policy and Loc-RIB views to the local routing service. The source sends
ordinary BGP updates; it never injects BMP frames. The packet probe verifies
cEOS software forwarding separately from BMP and does not claim physical ASIC
behavior. All NOS configuration is staged before startup; no live NOS
configuration commands are used.

The topology fixes `172.31.218.0/24`, ports 19019/19029 and lab name
`bonsai-bn006-live-20261107`. Before deploying, verify that the lab name, subnet
and ports are unused; do not reuse or destroy another lab. Copy the topology,
startup files and `tests/lab/bn006_bgp_speaker.py` into a dedicated directory
named `/tmp/bonsai-bn006-live-*`. Use a separate journal directory with the same
prefix. Start the routing service with its BMP listener bound to the lab bridge
address, HTTP bound to localhost, both journal paths set, and a lab-only token.
The acceptance scripts use `bn006-lab-internal` and port 19029.

After deploying this topology with Containerlab, start the source with
`docker exec -d clab-<lab-name>-source python3 /bn006_bgp_speaker.py` and wait
for all three sessions to establish. The main gate checks actual announcements,
policy attribute effects, withdrawal, gap preservation and a full dedicated
lab restart. It recreates **only** the lab named by its `--lab-root`:

```sh
python3 tests/bn006_bmp_live_acceptance.py \
  --lab-root /tmp/bonsai-bn006-live-20261107 \
  --journal-root /tmp/bonsai-bn006-live-journal-20261107c \
  --evidence-root /tmp/bonsai-bn006-live-20261107/second-run \
  --source-url http://172.31.218.12:19000 --speaker 172.31.218.11
```

For forwarding, run `tests/bn006_fib_live_acceptance.py` with the same lab,
source URL and speaker plus `--instance default` or `--instance blue` and a
fresh `--evidence-name`. It captures the exact sent and returned Ethernet
frames, checks TTL and payload independently, and requires no matching forward
after withdrawal while the other VRF remains present. It reannounces the route
in a `finally` block.

The policy candidate differs only in the configured local preference. Preserve
the baseline evidence, stop the dedicated lab, stage `eos-policy-candidate.cfg`
as the stopped EOS node's startup configuration, then deploy the same dedicated
topology and start the source. Run `tests/bn006_policy_live_acceptance.py` with
the lab, source URL, speaker and the expected minimum new session epoch. It
compares EOS CLI and BMP values for the same MED and checks BLUE isolation.
Never apply this configuration to an operational device.

The native SR Linux test is opt-in: obtain its SSH public host key directly from
the dedicated container, pin it in a lab-specific known-hosts file, and set
`BONSAI_BN006_LIVE_ROOT`, `BONSAI_BN006_SRL_ADDRESS`,
`BONSAI_BN006_KNOWN_HOSTS`, and a fresh `BONSAI_BN006_NATIVE_EVIDENCE` filename.
Run the named ignored test in `bn006_collection_wiring` explicitly. Do not
accept an unverified network-supplied key. Capture exact image IDs, source and
fixture hashes, commands, exit codes and raw data before cleaning up the owned
lab; do not use `containerlab destroy --all` or `--cleanup`.

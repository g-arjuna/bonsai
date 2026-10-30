# BN-006 native SR Linux capture slice

Two disposable SR Linux nodes, pinned to the image digest observed on 30 October
2026 (26.3.1-410). This slice establishes eBGP and reads native CLI JSON. It does
not configure BMP, export a test route, mutate live device configuration, or
prove FIB, routing-service ingestion, browser parity or VRF/RD isolation.

Check that the lab name `bonsai-bn006-srl` and subnet `172.31.216.0/24` are unused.
Use a temporary workspace so generated TLS/SSH credentials stay outside Git:

```sh
bn006_workspace=$(mktemp -d /tmp/bonsai-bn006.XXXXXXXX)
cp lab/acceptance/bn006-srl/*.cfg lab/acceptance/bn006-srl/topology.clab.yml "$bn006_workspace/"
containerlab deploy -t "$bn006_workspace/topology.clab.yml"
```

Allow startup and BGP convergence; the observed startup took about three minutes.
Read each node's version and neighbor state (replace `srl1` with `srl2` as needed):

```sh
docker exec clab-bonsai-bn006-srl-srl1 sr_cli -d --timeout 10000 --output-format json 'info from state system information'
docker exec clab-bonsai-bn006-srl-srl1 sr_cli -d --timeout 10000 --output-format json 'info from state with-context network-instance default protocols bgp neighbor *'
docker exec clab-bonsai-bn006-srl-srl1 sr_cli -d --timeout 10000 'show network-instance default protocols bgp neighbor'
```

Require established sessions on both nodes. Route counters are genuinely zero
in this slice; these counts do not demonstrate route lifecycle acceptance.
Preserve exact raw bytes, command/exit status, image identity, and capture hashes.
Keep JSON native: do not flatten it into the old A13 synthetic schema.

Always destroy only this dedicated topology when finished:

```sh
containerlab destroy -t "$bn006_workspace/topology.clab.yml"
```

Run deterministic replay/negative cases without a live lab:

```sh
cargo test --locked --test bn006_native_nos_adapter
```

The recorded run used a unique temporary lab name `bonsai-bn006-d821abca`; startup
configuration and the pinned image are equivalent to this reusable template.
Native fixtures and capture provenance are in
`docs/merger/evidence/2026-10-30-bn006/`. The `blue` VRF and policy-list test mutations
are synthetic regressions and must not be presented as live VRF evidence.

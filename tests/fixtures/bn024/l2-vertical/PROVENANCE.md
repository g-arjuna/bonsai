# BN-024 L2 vertical-slice fixture provenance

Captured read-only on 2026-11-27 from the pre-existing dedicated containerlab
workload `clab-bonsai-bn003-vrf-frr2` (FRRouting 10.6.1 image family). These
commands were executed with `docker exec` only to preserve genuine local output
shape for offline tests; production acquisition remains verified SSH and no
device configuration/write command was executed.

Commands:

- `vtysh -c 'show interface json'` -> `frr2-show-interface.json`
- `ip neigh show` -> `frr2-ip-neigh-show.txt` (genuine empty neighbor population)
- `bridge fdb show` -> `frr2-bridge-fdb-show.txt`
- `bridge -c vlan show` -> `frr2-bridge-c-vlan-show.txt` (header-only positive empty VLAN population)

SHA-256 at capture:

- `frr2-show-interface.json`: `2909f0cbc8247d0ca94a2d7b844c8d5f3676f51371a9583b1314dc98a358f44f`
- `frr2-ip-neigh-show.txt`: `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`
- `frr2-bridge-fdb-show.txt`: `38d98c92ca2c70e1a33b3afe8f46cb93384f3cabca00b462f1b0db07c2ae3330`
- `frr2-bridge-c-vlan-show.txt`: `d299cee85dd273e3589117253c00a89f353cbf6e8a5f3c880b4a4940cb5c81ce`

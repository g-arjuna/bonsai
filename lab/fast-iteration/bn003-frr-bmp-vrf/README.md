# BN-003: real FRRouting BMP VRF, no synthetic frames

**Version:** upstream FRR 10.6.2 official release image `quay.io/frrouting/frr:10.6.2`.

**Verification status:** FRR 10.6.2 source, upstream regression tests, and containerlab documentation were examined; YAML and bootstrap shell syntax checked. This harness has **not** been deployed or run against Docker/FRR/Bonsai in the authoring environment, because Docker/containerlab and the user's Bonsai repository are not available here. A green test must be observed on the actual Ubuntu machine.

## Lab design

- `frr1 eth1` (global BGP): AS65001 `10.100.1.0/31` toward `frr2 eth1` AS65002 `10.100.1.1/31`.
- `frr1 eth2` **inside kernel VRF BLUE**: AS65001 `10.100.2.0/31` toward `frr2 eth2` AS65002 `10.100.2.1/31`.
- `frr2` originates **real connected** `10.99.1.0/24` inside BLUE using a kernel dummy interface `bn003pfx`; `frr1` receives the update inside its BLUE BGP instance.
- `frr1` directly exports BMP for `router bgp 65001 vrf BLUE`, with `rd vpn export 65000:100` under IPv4 unicast. Raw BMP peer type `1` (RD instance) and distinguishers `0000fde800000064` expected; LOC-RIB peer type `3` is not a substitute.
- `frr1` BMP output TCP to host's **management bridge gateway** `172.100.208.1:11029`. A listener on host `127.0.0.1:11029` **will not work**; bind `0.0.0.0` or bridge's address.

## Files

- `frr-bmp-vrf.clab.yml`: network topology.
- `bootstrap.sh`: root container **pre-FRR** VRF provisioning. Waits for links, creates VRF, attaches eth2, creates advertisement interface on frr2, then starts unmodified official FRR script.
- `frr1/frr.conf` and `frr2/frr.conf`: BGP and BMP configuration.
- `frr1/daemons`, `frr2/daemons`: enable bgpd, `-M bmp` **on frr1**, ordinary default daemon users.
- `verify_bmp.py`: optional **real TCP BMP listener**, checks actual bytes and actual IPv4-unicast UPDATE NLRI; captures native BMP stream. This is **not** synthetic input and is **not** part of Bonsai.

## Preparatory checks

```sh
cd /home/arjuna/Desktop/bonsai/lab/fast-iteration
# Copy the files from this package here, in a dedicated folder, as desired.
# Change directory into the extracted package directory before running commands below.

docker version
containerlab version
docker pull quay.io/frrouting/frr:10.6.2
# Confirm module location and iproute2 (inside official image):
docker run --rm --entrypoint /bin/sh quay.io/frrouting/frr:10.6.2 \
  -c 'command -v ip; find /usr/lib/frr -name "*bmp*"; id frr'
# Confirm the host management subnet is not overlapping an existing Docker network:
docker network ls
ip route show
```

## Start a wire-only proof (instead of Bonsai listener)

In **terminal 1**, run from package directory:

```sh
python3 verify_bmp.py --seconds 120
```

In **terminal 2**:

```sh
sudo containerlab deploy -t frr-bmp-vrf.clab.yml
sudo docker logs clab-bonsai-bn003-vrf-frr1
sudo docker logs clab-bonsai-bn003-vrf-frr2
```

Check VRFs, interface membership, routes and BGP sessions:

```sh
sudo docker exec clab-bonsai-bn003-vrf-frr1 ip -d link show BLUE
sudo docker exec clab-bonsai-bn003-vrf-frr1 ip -d link show eth2
sudo docker exec clab-bonsai-bn003-vrf-frr1 ip route show table 1001
sudo docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show vrf'
sudo docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show bgp ipv4 unicast summary'
sudo docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show bgp vrf BLUE ipv4 unicast summary'
sudo docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show bgp vrf BLUE ipv4 unicast 10.99.1.0/24'
sudo docker exec clab-bonsai-bn003-vrf-frr2 vtysh -c 'show bgp vrf BLUE ipv4 unicast 10.99.1.0/24'
sudo docker exec clab-bonsai-bn003-vrf-frr1 vtysh -c 'show running-config'
```

Expected: BLUE active; eth2 master BLUE; frr1 BLUE neighbor 10.100.2.1 Established; 10.99.1.0/24 received; `verify_bmp.py` reports `WIRE PROOF PASSED`. **Only then** redirect into Bonsai and confirm RIB state through its own acceptance API/journey.

## Bonsai integration

Do **not** run the test BMP listener simultaneously with Bonsai on TCP 11029. Stop probe, start Bonsai bound on `0.0.0.0:11029` (or 172.100.208.1:11029), then restart/redeploy lab or reconnect BMP. No synthetically built frames are injected by this lab. Compare your Bonsai routing-service RIB fields against actual BMP Peer Up / Route Monitoring evidence.

If the destination is actually a collector container instead of the host, replace `bmp connect 172.100.208.1 port 11029` with the reachable collector's management IP. The host bridge gateway is valid only when collector listens on host.

## Failure checkpoints

- Missing `BLUE` interface: inspect bootstrap `docker logs`; wait for eth2 and kernel capability. This is **before** FRR.
- `show vrf BLUE inactive`: `ip -d link show BLUE`; check VRF name, table and `eth2` master.
- Global Established, BLUE Idle: check `show bgp vrf BLUE ... summary`, `show vrf`, `ip route show table 1001`; verify link addresses set by Zebra.
- BMP no connection: ensure collector bound to host address, inspect host firewall, test `docker exec ... ping -c 1 172.100.208.1` if ping is installed.
- Only type=3 LOC-RIB: accept that as Loc-RIB only, **do not** call it RD-instance success. Ensure frr2 genuinely advertises the network and frr1 receives eBGP updates.
- Type=1 but wrong distinguisher: confirm `rd vpn export 65000:100` **inside frr1 `router bgp 65001 vrf BLUE`, address-family ipv4 unicast**; inspect returned config.

## Source evidence

- FRR 10.6.2 upstream BMP VRF test: https://github.com/FRRouting/frr/blob/frr-10.6.2/tests/topotests/bgp_bmp/test_bgp_bmp_2.py
- FRR 10.6.2 upstream reference router: https://github.com/FRRouting/frr/blob/frr-10.6.2/tests/topotests/bgp_bmp/r1vrf/frr.conf
- FRR 10.6.2 peer-type and RD code: https://github.com/FRRouting/frr/blob/frr-10.6.2/bgpd/bgp_bmp.c
- FRR BMP loadable-module docs: https://docs.frrouting.org/en/stable-10.6/bmp.html
- FRR Linux VRF docs: https://docs.frrouting.org/en/stable-10.6/zebra.html
- Linux VRF device setup: https://docs.kernel.org/networking/vrf.html
- Containerlab node `cmd` vs `exec`: https://containerlab.dev/manual/nodes/
- BMP Peer Type/Per-peer header wire format: https://www.rfc-editor.org/rfc/rfc7854

## What remains outside the scope of this proof

The user's Bonsai source files `/home/arjuna/Desktop/bonsai/crates/rbmp-core/src/bgp/nlri.rs` and `crates/rbmp-rib/src/manager.rs` could not be accessed here, so actual parser and RIB-manager integration is not verified. A vpnv4 NLRI RD does not automatically make a default-VRF BMP peer-header a type=1 RD-instance peer; do not substitute that for BN-003 acceptance.

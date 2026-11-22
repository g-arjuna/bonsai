# M6F Junos (cJunosEVO) probe evidence — 2026-11-22

Real Juniper cJunosEvolved container booted under containerlab
`juniper_cjunosevolved` kind. NOT the production path — image/service
readiness probe only.

## Device / image

- Archive: `cJunosEvolved-26.2R1.7-EVO.tar.gz` (docker-save archive,
  `docker load` — archive-declared ref `docker.io/library/
  cjunosevolved:26.2R1.7-EVO`).
- Image ID: `sha256:3b253da2d8cfebac81e62939b934de26c492cb06e0ebcad898f66d835e108c11`
  (~2.25 GB). EVOVM is a QEMU/KVM guest inside the container
  (qemu-system-x86_64, 6.3 GB, 4 vCPU) — boot took ~10 min.
- NOS: Junos 26.2R1.7-EVO, model `ptx10001-36mr` (cPTX)
  (`junos-version.txt`).
- Lab: `/tmp/bonsai-m6f-junos/topology.clab.yml`, mgmt net
  `m6f-junos-mgmt` 172.100.105.0/24, node `172.100.105.11`.
  Junos mgmt reachable inside container at 176.1.1.1 via the
  image's `/usr/local/sbin/cli` ssh helper.

## Findings — gNMI UNSUPPORTED on this image

- `set system services ?` completions contain NO `grpc`/`gnmi`
  stanza (only grpc-tunnel for outbound TCP-over-gRPC)
  (`services-completions.txt`). No gNMI/grpc/openconfig stanza found
  anywhere under `set system`, `set protocols`, or `set chassis`.
- `show system connections` shows NO TCP listener on 57400/830/443;
  only SSH :22 (`listening-sockets.txt`). Internal `_gnmi` unix
  sockets exist (OpenConfig render infra) but no gNMI service is
  exposed.
- Conclusion: this cPTX-EVO image does not offer a configurable gNMI
  server in this train. Junos gNMI typically requires a platform/
  release that supports `system services grpc` (e.g. vMX/vSRX or
  different PTX trains) — owner may supply another image. Recorded
  as UNSUPPORTED, not FAIL: the image boots and runs Junos, it just
  lacks the gNMI service surface.

## Not executed

- Any gNMI RPC (no endpoint exists).
- NETCONF was not enabled (out of scope for gNMI acceptance).

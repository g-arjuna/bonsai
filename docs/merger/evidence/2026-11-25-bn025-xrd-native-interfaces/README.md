# BN-025 XRd native interface evidence

Genuine read-only evidence from dedicated authorized lab container
`clab-bn025-xrd-xrd` running `ios-xr/xrd-control-plane:24.4.2`
(`sha256:b3bd8e8ea4892d4dd9b0a32c223990965f6f874b6b7ebf732b27dd5ab3ca487f`)
at `172.100.106.21` on 2026-11-25 UTC.

Advertised model evidence: gNMI 0.10.0, JSON_IETF/ASCII/PROTO,
openconfig-interfaces 3.0.0, openconfig-if-ethernet 2.12.1, openconfig-if-ip
3.5.0, cisco-xr-openconfig-interfaces-deviations 2024-02-21.

Files:

- `capabilities.json` — gNMI capability response; plaintext lab-only warning is
  in the adjacent stderr file.
- `openconfig-interface-state-subscribe.json` — read-only subscribe-once for
  `/openconfig-interfaces:interfaces/interface[name=*]/state`.
- `openconfig-interface-root-subscribe.json` — read-only subscribe-once for
  `/openconfig-interfaces:interfaces/interface[name=*]`; this exact stdout is
  the committed normalization fixture.
- `container-image-identity.json` — Docker image id/tag evidence.

Credentials were supplied through environment variables (`M6F_XRD_USER`,
`M6F_XRD_PASS`) and are not recorded here. Plaintext gNMI was explicit lab-only
mode because containerlab first-boot configured `grpc port 9339 no-tls`; this
is not production transport qualification.

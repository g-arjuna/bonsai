# M6F — SR Linux live gNMI/YANG interoperability evidence

Date: 2026-11-21. Vendor: Nokia SR Linux (first live M6F slice).

## Environment

- Topology: `m6f-srlinux` (single node, generated from
  `lab/acceptance/m6f/manifests/vendors.yaml` +
  `lab/acceptance/m6f/configs/srlinux.cfg`).
- Image: `ghcr.io/nokia/srlinux@sha256:ad3d4a533674cf2e2e7321fef38806a6c66e3d56a3e8693c45fc2b4206078dd6`
  (same pinned digest family as `lab/acceptance/bn006-srl`; observed
  version `SRLinux-v26.3.1-410-g98ed16c774c`, platform 7220 IXR-D2L).
- Endpoint: `172.100.102.11:57400`, TLS via clab CA
  (`clab-m6f-srlinux/.tls/ca/ca.pem`, server name `srlinux`).
- Auth: HTTP/2 `username`/`password` metadata (lab account `admin`;
  password supplied via `M6F_SRL_PASS` env var, never recorded).
- Harness: `scripts/lab/m6f-gnmi-interop.sh --vendor srlinux`
  with `lab/acceptance/m6f/tools/m6f_gnmi.py` (read-only; no Set RPC).

## Executed checks (live device)

| Check | Result | Evidence |
|---|---|---|
| Capabilities | PASS | `capabilities.json` — gNMI 0.10.0, JSON_IETF/PROTO/ASCII + Nokia-proprietary encoding enum values (42–53), full model list incl. `ietf-yang-library` 2019-01-04 |
| YANG Library RFC 8525 | PASS | `yang-library-ietf.json` — `/ietf-yang-library:yang-library` Get returned module-set (base64 `json_ietf_val`) |
| Legacy YANG Library | PASS | `yang-library-legacy.json` — `/ietf-yang-library:modules-state` with JSON_IETF |
| Subscribe once (initial sync) | PASS | `subscribe-once.json` — `/interface[name=*]/statistics`, `srl_nokia-interfaces` origin, per-interface updates + sync |

## Real interoperability findings (defects in the *probe*, not the NOS)

1. SR Linux rejects prefix alias `yang-library:yang-library`; the
   canonical module name `ietf-yang-library:yang-library` is required.
   The probe now tries the canonical name (recorded in tool history).
2. SR Linux does not accept `Encoding.JSON` (0); supported encodings
   are ASCII, PROTO/TYPED_VAL, JSON_IETF, JSON_IETF_FROM_ROOT,
   PROTO_ANY, PROTO_JSON, PROTO_BYTES, CLI_FLAT.
3. `Subscription.path` is a single `Path`, not repeated — client bug
   fixed in `m6f_gnmi.py`.
4. Bidi-streaming Subscribe requires an iterator request, not a bare
   message — client bug fixed.
5. `sync_response` is a field of `SubscribeResponse`, not `Update` —
   client bug fixed.

No Bonsai production-code defect was identified by this slice; the
findings above are harness/probe corrections validated against the
live device.

## Not executed (explicit)

- Remote-collector discovery path (B6): requires a Bonsai core +
  collector deployment against this lab; NOT_EXECUTED in this slice.
- Controlled schema-bound Subscribe approval/revocation workflow:
  requires Core-side job orchestration; NOT_EXECUTED.
- XRd / Junos cJunos Evolved / cEOS: owner images present under
  `~/Desktop/clab-images/` but manifest entries remain `image: null`
  pending operator import — NOT_EXECUTED, not green.
- ON_CHANGE/SAMPLE streaming duration, delete semantics, coverage
  projection: partial — single-shot subscribe only.

## Evidence hygiene

- No credentials, private keys, tokens or certificates-with-keys in
  this directory; CA cert is public material but is not copied here.
- `SHA256SUMS` covers all captured JSON/log/topology artifacts.
- Raw `json_ietf_val` payloads are base64-encoded YANG data as returned
  by the device (RFC 7951); decode with `base64 -d` for inspection.

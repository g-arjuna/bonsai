# M6F cEOS probe evidence — 2026-11-22

Standalone read-only gNMI probe (`tools/m6f_gnmi.py`) against a real
Arista cEOS-lab container. NOT the production Core/collector path —
see MATRIX.md for the distinction.

## Device / image

- Image: `ceos:4.37.0F`, imported via `docker import
  cEOS64-lab-4.37.0F.tar.xz` (archive is a rootfs tar.xz, NOT a
  docker-save archive — `docker load` does not apply).
- Image ID: see `image-id.txt`.
- NOS: Arista cEOSLab 4.37.0F-51574443.4370F (see `eos-version.txt`).
- Lab: containerlab `arista_ceos` kind, mgmt `172.100.104.11`,
  topology `/tmp/bonsai-m6f-ceos/topology.clab.yml` (task-owned,
  `m6f-ceos-mgmt` network).
- gNMI: `show management api gnmi` → enabled, server on port **6030**,
  transport default, SSL profile none, authorization not required
  (`gnmi-service.txt`). Probe used explicit `--tls-mode plaintext`
  (lab-only); EOS issues no TLS on 6030 without an SSL profile.

## Results

- `01-capabilities.json` — PASS. gNMI 0.7.0, 303 supported models
  (openconfig-* + arista-*), encodings JSON/JSON_IETF/ASCII.
  Auth: `admin:admin` (clab default) via gRPC `username`/`password`
  metadata headers.
- `02-yanglib-ietf.json`, `03-yanglib-legacy.json` — UNSUPPORTED.
  Every RFC 8525 / legacy YANG Library Get variant is rejected by EOS
  (unknown origin / node not found). EOS does not advertise
  `ietf-yang-library` or `yang-library` in Capabilities either.
  Consequence: Core-side remote discovery cannot obtain RFC 8525
  library evidence from EOS; applicability resolution would have to
  rely on Capabilities `supported_models` alone (not implemented).
- `04-subscribe-sample.json` — PASS (probe). SAMPLE mode, 5 s
  interval, origin `openconfig`, path
  `interfaces/interface[name=Management0]/state/counters/in-octets`:
  real update (`uint_val`) + `sync_response=true`.
- `05-subscribe-onchange.json` — PARTIAL. ON_CHANGE initial
  synchronization observed (`admin-status=UP` + sync_response); no
  genuine change event was generated (no disruptive changes made).

## Conventions observed (differs from SR Linux)

- gNMI `origin` selects the model set: `openconfig`, `arista` are
  valid request-prefix origins; per-path `module:` prefixes are
  rejected as unknown origins. Path element names are UNPREFIXED
  (`interfaces/interface[name=Management0]/...`).
- Only interface present on the clab node is `Management0`
  (eth0/mgmt); there is no `Ethernet1`.
- EOS gNMI username is taken from gRPC metadata per its
  username-extraction priority (spiffe, metadata, commonName).

## Not executed

- Production Core/collector e2e for cEOS: blocked pending
  applicability evidence — EOS provides no YANG Library, so the
  schema-bound path cannot reach `applicable` under current Core
  resolution (fail-closed by design).
- TLS-verified gNMI (requires EOS `ssl profile` configuration —
  lab-only change, not applied).
- ON_CHANGE genuine change event, deletes, other interfaces/VRFs.

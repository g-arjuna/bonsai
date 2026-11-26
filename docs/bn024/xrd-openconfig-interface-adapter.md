# BN-025 — Cisco XRd OpenConfig interface normalization

Status: offline/shadow native normalization slice. This is genuine live-lab
source evidence plus a Rust semantic adapter, not production acquisition
authority and not operator-ready UI exposure.

## Genuine source evidence

Lab source:

- container: `clab-bn025-xrd-xrd`
- image: `ios-xr/xrd-control-plane:24.4.2`
- image id: `sha256:b3bd8e8ea4892d4dd9b0a32c223990965f6f874b6b7ebf732b27dd5ab3ca487f`
- address: `172.100.106.21`
- acquisition: read-only gNMI subscribe-once, plaintext lab-only port 9339
- subscribed path: `/openconfig-interfaces:interfaces/interface[name=*]`
- capture date: 2026-11-25 UTC
- gNMI version: 0.10.0; encodings JSON_IETF/ASCII/PROTO
- advertised models: openconfig-interfaces 3.0.0, openconfig-if-ethernet 2.12.1,
  openconfig-if-ip 3.5.0, cisco-xr-openconfig-interfaces-deviations 2024-02-21

Evidence directory:
`docs/merger/evidence/2026-11-25-bn025-xrd-native-interfaces/`.

The exact raw root subscribe response is copied to
`tests/fixtures/bn025/xrd-openconfig-interfaces/openconfig-interface-root-subscribe-xrd-24.4.2.json`
for independent expected-value tests. No credentials are present in the fixture.

## Source semantics established

The genuine XRd update sequence contains independent JSON-IETF subtree
notifications followed by `sync_response`. It is not an atomic snapshot.
`sync_response` means initial synchronization for the subscribed path scope; it
does not establish a device-wide expected interface denominator.

Observed base-interface state includes `name`, `type`, `mtu`, `enabled`,
`ifindex`, `admin-status`, `oper-status`, `logical`, `management`, `cpu` and
counters. Ethernet detail is namespaced under `openconfig-if-ethernet` and
includes MAC, auto-negotiation, forced port speed and negotiated speed.
Addressing is under subinterface index 0 in this capture.

Important semantic distinctions implemented in Rust:

- interface MTU is the interface-level OpenConfig value (1514 on the XRd
  management Ethernet interface); subinterface IPv4 MTU (1500) is not merged
  into that field;
- `negotiated-port-speed` is preferred, but `SPEED_UNKNOWN` is not converted
  into zero or down; when negotiation is disabled, forced `port-speed` may be
  used (`SPEED_1GB` -> 1000 Mbps);
- `iana-if-type:ethernetCsmacd` with `logical=false` is classified physical;
  `iana-if-type:other` with `logical=true` is classified virtual for `Null0`;
- conflicting duplicate gNMI leaves are preserved as source conflicts and make
  the affected typed field invalid rather than choosing a silent winner;
- non-zero OpenConfig subinterfaces are explicit unsupported scope in this
  slice. They are not flattened into dotted interface strings because the
  common typed subject does not yet have structured subinterface identity;
- absence of IPv6 addressing remains unknown when the source did not expose
  that family/subtree; it is not represented as observed-none.

## SuzieQ source crosswalk

Pinned upstream SuzieQ `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` was inspected:

- `config/interfaces.yml` uses CLI commands for `iosxr`: `show interfaces`,
  `show ipv4 interface`, `show ipv6 interface`, and `show vrf all detail` with
  TextFSM extraction. It does not define the OpenConfig gNMI source used here.
- `_clean_iosxr_data` in `poller/worker/services/interfaces.py` infers
  subinterfaces from a dot in the interface name, bonds from member lists or
  EtherChannel type, converts TextFSM speed values from Kbps to Mbps, rewrites
  administrative state from the operational line-protocol text when not
  explicitly administratively down, supplies zero MAC addresses for null/VRF
  cases, and drops records that lack a type field.

Those upstream behaviors are source-format-specific and are not copied. The
gNMI adapter uses explicit OpenConfig state tokens, keeps absent/unsupported
fields unavailable, does not manufacture VRF entities, does not infer type from
name punctuation, and does not treat CLI subinterface naming as structured
identity. SuzieQ is therefore a semantic caution/cross-reference for IOS XR,
not an executable oracle for this different acquisition format.

## Implementation boundary

Implemented:

- `src/xrd_openconfig_interfaces.rs`
  - bounded decode of the versioned subscribe-once JSON representation;
  - canonical namespace-stripped recursive merge of non-atomic subtree updates;
  - typed `interface.state` facts for base interfaces;
  - explicit capture, invalid-input, empty, no-usable-record and output-budget
    outcomes;
  - coverage counters and expected population `None` (unknown);
  - genuine XRd fixture expected-value tests.
- `src/normalizer_registry.rs` / `src/nos_features.rs`
  - registered exact parser/mapping revisions;
  - declarative `interfaces.openconfig_gnmi` feature profile for
    `cisco.iosxr` version `=24.4.2`;
  - experimental shadow dispatch only. The current capture does not carry
    device-reported release/build identity, so qualified dispatch intentionally
    fails closed.

Not implemented / explicitly deferred:

- no production Core-managed XRd acquisition job;
- no persistent shadow observation lifecycle integration;
- no operator UI surface or browser claim;
- no authoritative graph projection;
- no structured non-zero subinterface identity;
- no network-instance/VRF attachment extraction;
- no LAG relationship extraction;
- no cross-source reconciliation with CLI or native Cisco YANG.

Operator/UI dependency: BN-030-25 feature exposure lifecycle and the shared
interface presentation work in the owner roadmap. This offline normalization
slice must not be promoted as operator-ready until acquisition, retention,
authorization and shared presentation exist.

## Independent expected values in the genuine fixture

`MgmtEth0/RP0/CPU0/0`:

- admin/oper: `UP`/`UP`
- ifindex: `3`
- type: physical (`iana-if-type:ethernetCsmacd`, `logical=false`)
- interface MTU: `1514`
- MAC: `d2:72:77:bd:1d:65`
- speed: `SPEED_1GB` -> `1000` Mbps because negotiation is disabled and
  negotiated speed is `SPEED_UNKNOWN`
- IPv4: `172.100.106.21/24` on subinterface index 0

`Null0`:

- admin/oper: `UP`/`UP`
- ifindex: `2`
- type: virtual (`iana-if-type:other`, `logical=true`)
- interface MTU: `1500`
- no ethernet subtree; speed is explicitly unsupported

# ADR: Cross-Device BMP + gNMI Correlation

**Status**: Proposed  
**Date**: 2026-05-23  
**Author**: Bonsai Engineering  
**Context**: D4-11 T5 — S-33 structural gap

## Problem Statement

BMP and gNMI produce BGP state-change events from **different device perspectives**. When a BGP session between Device A (gNMI-monitored) and Device B (BMP-monitored) flaps:

- gNMI fires `bgp_session_change` with `device_address = A`, `peer = B`
- BMP fires `bmp_session_change` with `device_address = B`, `peer_address = A`

The current `CorrelationBuffer` keys by `(device_address, semantic_type, sub_key)`. Since `device_address` differs between the two events, they land in **separate correlation slots** and are never merged. This means:

1. Two independent detections fire for the same physical event
2. Multi-source correlation (the core value of the correlation buffer) is lost for the most common cross-device scenario

## Decision

Introduce a **BgpSessionKey** as a canonical cross-device session identifier.

### Design

```
BgpSessionKey = sorted(lower_ip, higher_ip)
```

Where IPs are sorted lexicographically to create a deterministic canonical form regardless of which side reports the event.

### Implementation Plan

#### Phase 1: CorrelationBuffer Extension

1. Add a `cross_device_key: Option<String>` field to `CorrelationSlot`.
2. When `semantic_key_for_event()` returns a BGP event type (`bgp_neighbor_down`, `bgp_neighbor_up`), compute the `BgpSessionKey` from `(device_address, peer_address)`.
3. Before inserting into the primary `(device_address, semantic_type, sub_key)` slot, check if a slot with the same `BgpSessionKey` already exists under a different device_address.
4. If found: merge the new event into the existing slot (add source_type, increment count, extend state_change_event_ids).
5. Add a secondary index: `HashMap<String, CorrelationKey>` mapping `BgpSessionKey → primary CorrelationKey`.

#### Phase 2: Detection Dedup

6. When the correlation sweep fires a detection from a cross-device-merged slot, annotate the detection with `affected_devices: [A, B]` (both ends).
7. Suppress the duplicate detection that would have been fired from the second device's slot (already merged).

#### Phase 3: Graph Edge

8. Optionally create a `CORRELATED_WITH` edge between the two `StateChangeEvent` nodes from different devices, linking the BMP and gNMI observations of the same physical event.

### Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| IP address mismatch (loopback vs physical) | Normalize via Device node's `address` field; BMP `peer_address` may be loopback while gNMI uses management IP. Requires a device-address-to-loopback lookup table. |
| Performance overhead of secondary index | `DashMap` or `Mutex<HashMap>` is O(1) per lookup. BGP events are low-frequency (~tens/sec at most). |
| False merges (different sessions with same IP pair) | Sub-key includes AFI/SAFI or VRF if available; plain IP pair covers 99% of cases in single-VRF deployments. |
| Ordering sensitivity | `drain_expired()` must handle merged slots where events arrived at different times; use max(timestamp) as the slot's effective time. |

### Scope Boundaries

- **In scope**: BGP session events (up/down) from BMP + gNMI.
- **Out of scope**: Interface events (already single-device), OSPF/ISIS (typically single-source per device), BFD (already correlated by peer IP within same device).
- **Future**: Could extend to SNMP BGP trap + gNMI correlation using the same BgpSessionKey approach.

## Alternatives Considered

### A: Post-hoc detection merge (downstream)

Instead of merging in the correlation buffer, let both detections fire and merge them in the incident correlation layer.

**Rejected**: This defeats the purpose of the correlation buffer (reduce detection noise) and doubles the detection count. Incident merge logic would need to understand BGP session symmetry, adding complexity at the wrong layer.

### B: Unified device_address via session-level keying

Key all BGP events by `BgpSession.id` (which is already `device:peer`) and normalize to always use the lower-IP side as the "device".

**Rejected**: Breaks the per-device model that underpins blast radius calculation, device health scoring, and topology display. The `device_address` field is load-bearing across the entire system.

### C: Do nothing (accept duplicate detections)

Accept that BMP and gNMI will produce separate detections for the same BGP event.

**Acceptable short-term** but degrades signal quality as BMP adoption grows. Each dual-monitored session produces 2x detections.

## Decision Outcome

Proceed with the **BgpSessionKey** approach (Phase 1 + Phase 2). Phase 3 (graph edge) is optional polish.

Implementation should be gated behind a feature flag (`BONSAI_CROSS_DEVICE_CORRELATION=1`) for rollout safety, since it changes correlation semantics.

## References

- S-33 test result: "structural — cross-device device_address mismatch"
- `src/correlation_buffer.rs`: current per-device keying
- `src/streaming/bmp.rs`: BMP session event production
- `src/telemetry.rs`: gNMI BGP session path classification

# ADR: SuzieQ vs PyATS/Genie vs Bonsai-Native Parsing for Redundancy Discovery

**Status**: Proposed  
**Date**: 2026-05-23  
**Author**: Bonsai Engineering  
**Context**: D4-12 T3 — SuzieQ integration evaluation

## Problem Statement

Bonsai needs to discover redundancy groups (LAG, ECMP, dual-homed, VRRP/HSRP) during device onboarding to populate `RedundancyGroup` graph nodes. This requires parsing multi-vendor CLI output for:

- LAG/port-channel membership
- ECMP next-hop groups
- VRRP/HSRP virtual router state
- Dual-homed server detection (same MAC on two ToR switches)

Three approaches are under consideration:

1. **SuzieQ** (OSS network observability tool)
2. **PyATS/Genie** (Cisco-maintained, multi-vendor network test framework)
3. **Bonsai-native parsing** (custom parsers in Python/Rust)

## Options Analysis

### Option A: SuzieQ as a Library/Subprocess

**SuzieQ** is an open-source network observability tool that normalises multi-vendor data into a common schema. It supports Cisco IOS/NX-OS, Arista EOS, Juniper Junos, Nokia SRL, FRR, Cumulus.

**Pros:**
- Pre-built normalised tables: `lldp`, `bgp`, `routes`, `interfaces`, `macs`, `arpnd`, `ospf`, `evpn`
- Multi-vendor support covering our target platforms
- Active development (Dinesh Dutt / Stardust Systems)
- Can be used as a Python library (`suzieq.sqobjects`) or CLI (`sq`)
- Parquet-based data store allows direct querying
- Already handles LLDP neighbor discovery, BGP session state, routing table parsing

**Cons:**
- Heavy dependency: pulls in pandas, pyarrow, parquet, multiple vendor SSH libraries
- Requires its own inventory/credential management (overlaps with Bonsai vault)
- Polling model (SSH-based) — not streaming; adds latency vs gNMI
- License: Apache 2.0 (acceptable) but version stability varies
- Would need a sidecar process or embedded Python runtime
- Data model doesn't align 1:1 with Bonsai's graph schema — transformation layer needed

**Architecture:** Run SuzieQ as a sidecar container, triggered during onboarding via the existing sidecar framework. Parse output via JSON and map to Bonsai's `RedundancyGroup` graph model.

### Option B: PyATS/Genie

**PyATS** is Cisco's network automation test framework. Genie provides multi-vendor CLI parsers.

**Pros:**
- 4000+ pre-built CLI parsers across Cisco, Arista, Juniper (via Genie)
- Already used in Bonsai sidecar (`docker/sidecars/pyats/`)
- Structured JSON output per `show` command
- Precise per-command parsing (e.g., `show etherchannel summary` → LAG details)
- Well-tested parser quality for Cisco platforms
- No background process needed — invoke per-device during onboarding

**Cons:**
- Non-Cisco vendor support is weaker (Nokia SRL: partial, FRR: limited)
- Parser quality varies — some are outdated for newer OS versions
- Each redundancy type needs explicit per-vendor parser mapping
- No normalised cross-vendor schema — Bonsai must handle vendor differences
- Can be slow (SSH connect + multiple `show` commands per device)

**Architecture:** Already integrated. Extend `bootstrap_device_handler` to run additional Genie parsers during onboarding and map results to `RedundancyGroup` nodes.

### Option C: Bonsai-Native Parsing (Rust/Python)

**Pros:**
- Zero external dependencies
- Full control over parser quality and schema mapping
- Can use gNMI-streamed data (no SSH needed) for OpenConfig-compliant devices
- Fastest execution — no sidecar overhead
- Can be incrementally extended

**Cons:**
- Significant development effort per vendor × feature
- Must maintain parsers as vendor CLIs evolve
- Duplicates work that SuzieQ/Genie already does
- Most redundancy information is NOT available via gNMI on all platforms — CLI parsing is still needed for LAG discovery on many vendors

**Architecture:** Add parser functions in `python/bonsai_sdk/` or Rust modules. Use gNMI OpenConfig paths where available, fall back to PyATS sidecar for CLI-only data.

## Decision

**Recommended: Option B (PyATS/Genie) with selective native parsing.**

### Rationale:

1. **Already integrated**: PyATS sidecar is deployed and working. Extending it is lower risk than adding SuzieQ.
2. **Precise parsing**: Genie parsers are per-command, per-vendor, which gives better control than SuzieQ's polling model.
3. **Incremental native**: For OpenConfig-compliant devices (Nokia SRL), use gNMI paths for LAG/ECMP discovery. For CLI-only data, delegate to PyATS.
4. **SuzieQ future option**: If bonsai needs a "network state snapshot" feature (full topology baseline), SuzieQ becomes attractive as a scheduled sidecar. Park as future work.

### Implementation Plan:

1. Extend `bootstrap_device_handler` to run redundancy discovery parsers during onboarding
2. Define parser mapping per vendor:
   - **Cisco IOS/NX-OS**: `show etherchannel summary`, `show ip route`, `show vrrp`, `show hsrp`
   - **Arista EOS**: `show port-channel summary`, `show ip route`, `show vrrp`
   - **Nokia SRL**: gNMI `/interface[name=*]/lag` + `/network-instance[name=*]/route-table`
   - **Juniper Junos**: `show lacp interfaces`, `show route`
   - **FRR**: `show interface bond`, `show ip route`, `show vrrp`
3. Map parsed output to `RedundancyGroup` + `MEMBER_OF` edges
4. Run during onboarding bootstrap AND as a scheduled refresh (every 6h or on-demand)

### SuzieQ Deferred:

SuzieQ remains a candidate for future D4-12 T4 enhancement if:
- Bonsai adds a "full topology baseline" feature
- The number of vendor × feature combinations exceeds PyATS coverage
- SuzieQ stabilises its API for library use (currently oriented toward CLI/REST)

## References

- SuzieQ: https://github.com/netenglabs/suzieq
- PyATS/Genie: https://developer.cisco.com/docs/pyats/
- Existing sidecar: `docker/sidecars/pyats/`
- D4-12 T1: Redundancy graph model (already implemented)
- D4-17: Device onboarding bootstrap flow

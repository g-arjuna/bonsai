# ADR: Parsing Strategy for Redundancy Discovery & Operational Data

**Status**: Accepted (Revised)  
**Date**: 2026-05-23 (original) / 2026-05-23 (revised)  
**Author**: Bonsai Engineering  
**Context**: D4-12 T2/T3 — Redundancy discovery via parsing

## Problem Statement

Bonsai needs to discover redundancy groups (LAG, ECMP, dual-homed, VRRP/HSRP) during device onboarding to populate `RedundancyGroup` graph nodes. This requires parsing multi-vendor CLI output for:

- LAG/port-channel membership
- ECMP next-hop groups
- VRRP/HSRP virtual router state
- Dual-homed server detection (same MAC on two ToR switches)
- ARP/neighbor tables for host endpoint correlation

Four approaches were evaluated:

1. **SuzieQ** (OSS network observability tool)
2. **PyATS/Genie** (Cisco-maintained, multi-vendor network test framework)
3. **TextFSM + ntc-templates** (lightweight template-based CLI parser)
4. **Bonsai-native parsing** (custom parsers in Python/Rust)

## Honest Assessment (Revision Note)

The original ADR (batch21) recommended "PyATS/Genie, already integrated." A second-pass audit revealed:

| Component | Original Claim | Reality |
|-----------|---------------|---------|
| `docker/sidecars/pyats/` | "PyATS sidecar is deployed and working" | **Stub** — `fallback_parse()` was a line-splitter. No `pyats`/`genie` in `requirements.txt`. |
| `docker/sidecars/bonsai-native-parser/` | "Native parser for vendor-specific output" | **Stub** — `line_parse()` also a line-splitter. |
| `python/bootstrap_agent.py` | "Uses Genie learn() for data collection" | **True** — but only learns `interface`, `bgp`, `lldp`, `isis`, `platform`. No LAG, VRRP, routing, or ARP. |
| TextFSM | Not mentioned | **Zero presence** in codebase. |
| SuzieQ | "Deferred" | **Zero presence** — correct. |

**The gap**: RedundancyGroup schema existed (batch14), detection rules existed (batch14), but nothing populated them from device state. The parsing layer was hollow.

## Options Analysis

### Option A: SuzieQ (Deferred)

**Pros:** Pre-built normalised tables, multi-vendor, active development.  
**Cons:** Heavy deps (pandas/pyarrow), own inventory/credential model, SSH polling (same as PyATS), data model mismatch still requires transformation.  
**Decision:** Deferred. Overhead is not justified when Genie + TextFSM cover the same parsing needs with less operational complexity.

### Option B: PyATS/Genie (Primary — via sidecar)

**Pros:** 4000+ parsers, `device.learn()` for interface/bgp/lag/vrrp/routing/arp, structured JSON.  
**Cons:** Non-Cisco weaker (Nokia SRL: partial, FRR: limited), heavy container (~1.5GB), slow SSH connect.  
**Role:** Primary parser. `POST /learn` endpoint in sidecar handles full device learning. `POST /parse` handles individual command output.

### Option C: TextFSM + ntc-templates (Fallback)

**Pros:** 5000+ vendor templates, lightweight, community-maintained, easy to add custom templates.  
**Cons:** Returns flat list-of-dicts (not nested), no `learn()` concept, requires knowing exact CLI command.  
**Role:** Fallback parser when Genie can't parse a command (vendor gap or parser bug).

### Option D: Bonsai-Native Parsing (gNMI paths)

**Pros:** Zero deps, gNMI streamed data, fastest execution.  
**Cons:** Only works on OpenConfig-compliant devices, significant per-vendor effort.  
**Role:** Opportunistic. Use gNMI paths where available (Nokia SRL `/interface[name=*]/lag`).

## Decision

**Accepted: Genie (primary) + TextFSM (fallback) + gNMI (opportunistic).**

### Parsing Chain

```
Device onboarding
  │
  ├── bootstrap_agent.py → Genie learn()
  │     learns: interface, bgp, lldp, isis, lag, vrrp, routing, arp
  │     posts to: POST /api/devices/seed
  │
  ├── pyats-sidecar POST /parse → Genie parser → TextFSM fallback → line-split
  │     for ad-hoc CLI output parsing during investigations
  │
  └── gNMI subscriptions (streaming)
        for OpenConfig LAG, route-table on compliant devices
```

### Implementation (batch22):

1. **PyATS sidecar rewritten** (`docker/sidecars/pyats/`)
   - `requirements.txt`: Added `pyats[full]`, `genie`, `textfsm`, `ntc-templates`
   - `app.py`: Real parsing chain — Genie → TextFSM → line-split
   - `POST /parse`: Parse raw CLI output for any (vendor, command) pair
   - `POST /learn`: SSH to device, run `device.learn()` for feature set
   - `GET /healthz`: Reports Genie/TextFSM availability
   - Vendor mappings: cisco_iosxe/iosxr/nxos, arista_eos, juniper_junos, nokia_srlinux/sros, frr

2. **bootstrap_agent.py extended**
   - New data classes: `LagInfo`, `VrrpInfo`, `RouteInfo`, `ArpEntry`
   - New learn helpers: `_learn_lag()`, `_learn_vrrp()` (tries VRRP then HSRP), `_learn_routes()` (ECMP detection), `_learn_arp()`
   - `bootstrap_device()` now calls all 8 learn functions
   - `_seed_device()` and `preseed_graph()` send all data types

3. **Rust seed handler extended** (`src/http_server/managed_devices.rs`)
   - New structs: `SeedLagGroup`, `SeedVrrpInstance`, `SeedRoute`, `SeedArpEntry`
   - LAG → `RedundancyGroup(type=lag)` + `MEMBER_OF(Interface→RG)` edges
   - VRRP/HSRP → `RedundancyGroup(type=vrrp|hsrp)` + `MEMBER_OF(Device→RG)` edges
   - ECMP routes → `RedundancyGroup(type=ecmp)` + `MEMBER_OF(Device→RG)` for next-hops
   - ARP → `ArpEntry` nodes (for future dual-homed host detection)

### RedundancyGroup ID Conventions

| Type | ID Format | Example |
|------|-----------|---------|
| LAG | `lag-{device_addr}-{lag_name}` | `lag-10.0.0.1-Port-Channel1` |
| VRRP | `vrrp-{device_addr}-{iface}-{group_id}` | `vrrp-10.0.0.1-Vlan100-1` |
| HSRP | `hsrp-{device_addr}-{iface}-{group_id}` | `hsrp-10.0.0.1-Vlan100-1` |
| ECMP | `ecmp-{device_addr}-{prefix}` | `ecmp-10.0.0.1-10.1.0.0/24` |

### SuzieQ Remains Deferred

SuzieQ is a candidate for a future "network topology baseline" feature (full snapshot mode) if:
- The number of vendor × feature combinations exceeds Genie + TextFSM coverage
- Bonsai needs periodic full-state snapshots beyond what onboarding provides
- SuzieQ stabilises its Python library API for non-CLI use

## References

- SuzieQ: https://github.com/netenglabs/suzieq
- PyATS/Genie: https://developer.cisco.com/docs/pyats/
- ntc-templates: https://github.com/networktocode/ntc-templates (5000+ templates)
- TextFSM: https://github.com/google/textfsm
- PyATS sidecar: `docker/sidecars/pyats/`
- Bootstrap agent: `python/bootstrap_agent.py`
- Seed handler: `src/http_server/managed_devices.rs` (`device_seed_handler`)
- D4-12 T1: RedundancyGroup graph model (batch14)
- D4-12 T4: Redundancy loss detection rules (batch14)
- D4-17: Device onboarding bootstrap flow

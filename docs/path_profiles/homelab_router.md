# homelab_router — Home-lab and FRR-based router

**Environment**: home_lab  
**Roles**: router, pe, p, rr, leaf, spine, edge  
**Verification**: lab-verified (FRR on ContainerLab Holo topology)

## Purpose

A permissive catch-all profile for home-lab routers. All paths are optional so partial OC coverage is acceptable. The profile covers the full range of what FRR can advertise via gNMI.

## FRR gNMI OC Coverage (2025/2026)

FRR ships a gNMI server (`grpc` daemon) with the following OC model support:

| Model | Supported | Notes |
|-------|-----------|-------|
| `openconfig-interfaces` | ✅ Yes | Full counter + oper-state |
| `openconfig-bgp` | ✅ Yes | Session state, prefix counts; nested paths may vary |
| `openconfig-ospfv2` | ⚠️ Partial | Adjacency state; detailed LSA tables not available |
| `openconfig-isis` | ⚠️ Partial | Adjacency state; SR extension paths vary |
| `openconfig-lldp` | ⚠️ Depends | Requires lldpd integration; not always compiled in |
| `openconfig-bfd` | ⚠️ Partial | Session state when bfdd running |
| `openconfig-mpls` | ⚠️ Partial | LDP label bindings; RSVP-TE not in FRR |
| `openconfig-segment-routing` | ❌ Limited | SR-MPLS path SIDs; limited coverage |

## Holo as Alternative

[Holo](https://github.com/holo-routing/holo) is a Rust-based routing stack with stronger OC coverage than FRR for IS-IS, OSPF, and LDP. ContainerLab Holo images may produce more complete telemetry for lab verification.

## Sample Verified Paths (FRR)

```
interfaces                          -> ✅ counters + oper-state
network-instances/...bgp/neighbors  -> ✅ session state
network-instances/...ospf           -> ⚠️ adjacency only
lldp                                -> ⚠️ depends on image build
```

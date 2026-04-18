# Bonsai — Network State Engine

## What This Project Is
A streaming-first, graph-native network state engine. Ingests gNMI
telemetry from ContainerLab (Nokia SR Linux, Cisco IOS-XRd, Juniper
cRPD, Arista cEOS), writes to an embedded graph database, and closes
a detect-predict-heal loop. MIT licensed, open source, personal
learning project. Goal: replicate Google's ANO framework architecture
at lab scale using only open source primitives.

## Current Phase
Phase: 2 — The Graph (complete for SRL/XRd/cEOS; cRPD telemetry deferred)  
Last completed: Multi-vendor graph validated — SRL + XRd + cEOS all writing interfaces/BGP/LLDP/CONNECTED_TO. Capabilities-driven subscription, per-vendor blob walkers, CONNECTED_TO topology edges, BGP state normalized to lowercase.  
Next: Phase 3 — Python SDK (query layer over the graph)

## Architecture
- Rust core: tokio async runtime, tonic gRPC, prost protobuf
- Graph DB: **LadybugDB** (`lbug` crate, MIT, embedded, Cypher). Grafeo named fallback.
  Temporal: DIY bitemporal (valid_from/valid_to on all nodes/edges).
  Decision rationale in DECISIONS.md.
- Python layer: REST API consumer (PyO3 later), rules engine, ML pipeline
- Lab: ContainerLab — Holo/FRR for fast iteration, Nokia/Cisco/Juniper/Arista
  as primary vendor targets once accounts are approved

## Non-Negotiable Rules
- No SNMP, no NETCONF — gNMI only, always
- No async runtime other than tokio
- Every architectural decision gets an entry in DECISIONS.md with date and rationale
- Never add scope beyond current phase without flagging it explicitly
- Rust code must compile before ending a session — no broken state
- No campus/wireless, no optical transport, no Kubernetes, no RBAC — say no politely
- Credentials (username/password) must never appear in source code or committed files — use bonsai.toml (gitignored) or env vars

## Scope Guardrails (enforce these)
IN: DC + SP topologies, gNMI/OpenConfig only, four vendor families
    (Nokia SR Linux, Cisco IOS-XRd, Juniper cRPD/vJunosEvolved, Arista cEOS),
    Holo/FRR as OSS references, YANG paths: interfaces/BGP/OSPF/IS-IS/LLDP/
    platform + SP paths (openconfig-mpls, openconfig-segment-routing,
    openconfig-network-instance), closed-loop healing via gNMI Set,
    single-host deployment for v1.

OUT: SNMP, NETCONF, campus/wireless, optical transport, Kubernetes/HA/clustering,
     multi-tenancy/RBAC/auth beyond TLS, production WAL/replication,
     config-writing UI (Phase 6 UI is view-only), any fifth vendor in first 6 months.

## Anti-Patterns (things that will kill this project)
- Adding SNMP/NETCONF "because a user asked"
- Phase 6 UI growing into a product — it is a demo view, reject any admin/config/auth features
- Deploying to Kubernetes before v0.1 runs on one laptop
- Writing blog posts before Phase 2 works
- Chasing more vendors before the current four work vendor-neutrally
- Rewriting from Rust to Go because it's easier
- Accepting scope expansions that add breadth before depth of normalization

## File Structure
- /src — Rust core
- /python — Python SDK and rule engine
- /lab — ContainerLab topology YAMLs
  - /lab/fast-iteration — Holo/FRR topologies (immediate use)
  - /lab/real-vendors — Nokia/Cisco/Juniper/Arista topologies
- /docs — architecture notes
- DECISIONS.md — append-only decision log (never edit past entries)
- PROJECT_KICKOFF.md — origin thesis, full roadmap, research items
- bonsai.toml — local runtime config (gitignored; copy from bonsai.toml.example)
- bonsai.toml.example — committed template with placeholder values

## Build Commands
```
cargo build --release          # debug builds exceed MSVC 4GB static lib limit (lbug on Windows)
cargo run --release
cargo test --release
cargo clippy --release -- -D warnings   # must pass before any commit
```

**Windows note**: `cargo build` (debug) will fail with LNK1248 because lbug's C++ static lib
exceeds the MSVC 4GB limit in debug mode. Always use `--release` on this machine.

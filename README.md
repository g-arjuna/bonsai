# bonsai

> **Current status: Phase 1 in progress — nothing works yet.**

A streaming-first, graph-native network state engine for closed-loop autonomous
network operations. Replicates the architecture described in Google's ANO framework
paper at lab scale, using only open source primitives.

---

## The Gap This Fills

The open source ecosystem has strong individual components but no integrated system
that does all of this together:

| Layer | Open Source Today | Status |
|---|---|---|
| Multi-vendor streaming telemetry | gNMI + OpenConfig + gNMIc | Mature |
| Time-series metrics | Prometheus / InfluxDB | Mature |
| Normalized tabular state | SuzieQ (Parquet) | Good |
| Intended state / source of truth | Nautobot / NetBox | Mature |
| **Live graph of operational state, streaming updates** | **Nothing complete** | **The gap** |
| **LLM-agnostic query layer over graph state** | **Nothing complete** | **The gap** |
| **Closed-loop predict/heal pipeline on OSS** | **Nothing complete** | **The gap** |

Forward Networks and Selector are commercial because they built the graph + inference
layer. That is the exact layer bonsai targets.

---

## Architecture

```
┌──────────────────────────────────────────────────┐
│  ContainerLab topologies                         │
│  Nokia SR Linux · Cisco IOS-XRd                  │
│  Juniper cRPD · Arista cEOS                      │
│  Holo · FRR (open-source fast-iteration targets) │
│  gNMI Subscribe streams, OpenConfig YANG paths   │
└────────────────────┬─────────────────────────────┘
                     │ gRPC/gNMI
                     ▼
┌──────────────────────────────────────────────────┐
│  RUST CORE                                       │
│  ┌────────────────────────────────────────────┐  │
│  │ gNMI subscriber pool (tokio, per-device)   │  │
│  │  → protobuf decode, OpenConfig normalize   │  │
│  ├────────────────────────────────────────────┤  │
│  │ Graph writer (batched, debounced)           │  │
│  │  → embedded graph DB                       │  │
│  │  → temporal version chain                  │  │
│  ├────────────────────────────────────────────┤  │
│  │ Query API (Cypher over REST)               │  │
│  └────────────────────────────────────────────┘  │
└────────────────────┬─────────────────────────────┘
                     │ REST
                     ▼
┌──────────────────────────────────────────────────┐
│  PYTHON LAYER                                    │
│  Query SDK · anomaly rules · ML pipeline         │
│  Remediation via gNMI Set back to devices        │
└──────────────────────────────────────────────────┘
```

**Principles:**
- **Streaming-first** — no polling, no scheduled scrapes, everything flows as telemetry arrives
- **Graph-native** — relationships are first-class, topology traversal is the primary query pattern
- **Temporal by design** — every state change versioned, reconstruct graph state at any past time
- **LLM-agnostic** — Cypher query API, any consumer (Python, LLM, Grafana, ServiceNow) can use it

---

## Technology Stack

| Component | Choice | Notes |
|---|---|---|
| Core language | Rust (stable, edition 2024) | tokio async, tonic gRPC, prost protobuf |
| Graph DB | TBD — evaluating Kuzu v0.11.3 / Ladybug / ArcadeDB | See DECISIONS.md |
| Python integration | REST API (PyO3 later) | Phase 3+ |
| ML | PyTorch + scikit-learn | Phase 5 |
| Lab | ContainerLab | Holo/FRR now, real vendors once accounts approved |

---

## Scope

**In scope:** Data center + service provider topologies · gNMI/OpenConfig only ·
Nokia SR Linux · Cisco IOS-XRd · Juniper cRPD/vJunosEvolved · Arista cEOS ·
Holo + FRR as OSS references · YANG paths: interfaces, BGP, OSPF, IS-IS, LLDP,
platform, openconfig-mpls, openconfig-segment-routing, openconfig-network-instance ·
Closed-loop healing via gNMI Set · Single-host deployment.

**Out of scope:** SNMP · NETCONF · Campus/wireless · Optical transport · Kubernetes/HA ·
Multi-tenancy/RBAC · Production WAL/replication · Config-writing UI · Any fifth vendor
in the first 6 months.

---

## Roadmap

| Phase | Goal | Timeline |
|---|---|---|
| **1 — The Heartbeat** | Rust binary subscribes to gNMI, prints normalized JSON | Weeks 1–4 |
| 2 — The Graph | Telemetry writes to graph DB, Cypher queries return live + historical state | Weeks 5–12 |
| 3 — Python Layer | SDK queries graph, pushes remediation via gNMI Set | Weeks 13–18 |
| 4 — Rules Detect+Heal | Deterministic anomaly detection, closed-loop healing demo | Weeks 19–26 |
| 5 — ML Prediction | Autoencoder/LSTM predicts failures, classifier selects remediation | Months 7–10 |
| 6 — MVP UI | Live topology view, event stream, closed-loop trace — view-only | Weeks 44–52 |
| 7 — Presentation | Blog series, NANOG/AutoCon talk, demo video | Month 12+ |

---

## Phase 1 Success Criteria

- `cargo run` subscribes to N devices in parallel
- Handles subscription lifecycle (connect, authenticate, subscribe, reconnect on drop)
- Prints interface counter updates and BGP state changes as human-readable JSON
- Runs 24 hours without memory leaks or dropped subscriptions

---

## Repository Layout

```
/src          Rust core
/python       Python SDK and rule engine
/lab
  /fast-iteration   Holo/FRR ContainerLab topologies (immediate use)
  /real-vendors     Nokia/Cisco/Juniper/Arista topologies
/docs         Architecture notes
DECISIONS.md  Append-only architecture decision log
```

---

## License

MIT — see [LICENSE](LICENSE).

---

*Bonsai: deliberate cultivation of something precise and living.*

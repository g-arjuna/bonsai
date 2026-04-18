# bonsai

> **Current status: Phase 4 complete — detect-predict-heal loop working end-to-end. Next: Phase 5 ML prediction.**

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
| Graph DB | LadybugDB (`lbug` crate, MIT) | Embedded, Cypher, active Kuzu fork. See DECISIONS.md |
| Python integration | REST API (PyO3 later) | Phase 3+ |
| ML | PyTorch + scikit-learn | Phase 5 |
| Lab | ContainerLab | Nokia SR Linux running; Cisco/Juniper/Arista pending accounts |

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

| Phase | Goal | Status |
|---|---|---|
| **1 — The Heartbeat** ✓ | gNMI subscriber pool, interface counters + BGP ON_CHANGE, reconnect, graceful shutdown | **Complete** |
| **2 — The Graph** ✓ | Telemetry writes to LadybugDB graph, Cypher queries return live + historical state | **Complete** |
| **3 — Python Layer** ✓ | SDK queries graph, pushes remediation via gNMI Set | **Complete** |
| **4 — Rules Detect+Heal** ✓ | Deterministic anomaly detection, closed-loop healing demo | **Complete** |
| 5 — ML Prediction | Autoencoder/LSTM predicts failures, classifier selects remediation | Planned |
| 6 — Demo UI | Live topology view, event stream, closed-loop trace — view-only | Planned |

---

## Phase 1 — Completed

- [x] `cargo run` subscribes to 3 Nokia SR Linux nodes in parallel (tokio task per device)
- [x] Handles full subscription lifecycle: connect, authenticate, subscribe, reconnect on drop (exponential backoff)
- [x] Streams interface counter updates (SAMPLE/10s) and BGP neighbor state (ON_CHANGE) as JSON
- [x] Graceful Ctrl+C shutdown via shared watch channel
- [ ] 24-hour stability run (in progress)

## Phase 2 — Complete

- [x] Graph DB decided: LadybugDB (embedded, MIT, Cypher) — rationale in DECISIONS.md
- [x] Schema defined: Device, Interface, BgpNeighbor, LldpNeighbor, StateChangeEvent nodes
- [x] Graph writer wired: telemetry channel → spawn_blocking → LadybugDB Cypher upserts
- [x] Multi-vendor normalization: SRL + XRd + cEOS, Capabilities-based vendor detection
- [x] LLDP topology: CONNECTED_TO edges across all three vendors
- [x] Cypher queries validated: live topology + BGP state in graph

## Phase 3 — Complete

- [x] gRPC API server (tonic): Query, GetDevices, GetInterfaces, GetBgpNeighbors, GetTopology, StreamEvents
- [x] Python SDK: BonsaiClient with typed methods, end-to-end validated
- [x] BonsaiEvent broadcast: StateChangeEvent emitted on every BGP session-state change

## Phase 4 — Complete

- [x] Rule engine running, consuming StreamEvents from gRPC server
- [x] 8 rules: BgpSessionDown, BgpSessionFlap, BgpAllPeersDown, BgpNeverEstablished, InterfaceDown, InterfaceErrorSpike, InterfaceHighUtilization, TopologyEdgeLost
- [x] DetectionEvent written to graph for every fired rule (features_json = Phase 5 training data)
- [x] BGP session bounce: end-to-end auto-heal working on SRL (admin-state disable → enable)
- [x] Circuit breaker: ≥5 remediations per device in 10 min → halt
- [x] Remediation nodes written with outcome timestamps
- [x] Oper-status telemetry subscription: SRL (ON_CHANGE) + XRd (SAMPLE 30s)

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

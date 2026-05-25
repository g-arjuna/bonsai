# Bonsai — DV4 Supplement Backlog: Network-Wide DDoS Awareness

> **Sprint**: DV4-Supplement
> **Analysis basis**: Full review of DV4 codebase state (streaming sources: gNMI, syslog, SNMP, NetFlow, sFlow, OTLP, BMP, BGP-LS), existing graph schema, output adapters (Splunk, Elastic, SNOW EM, Prometheus), ML sidecar wiring, and investigation runtime.
> **Principle**: Every item is grounded in actual code state — not documentation assumptions. Architecture is additive: builds on top of existing DV4 infrastructure without breaking existing flows.

---

## Motivation

Bonsai already ingests rich multi-source telemetry from network devices. The goal of this supplement is to channel that data into a **network-wide DDoS situational awareness layer**. The key design principles are:

1. **Not perimeter-centric** — DDoS signals are gathered from all device layers (core, distribution, access, edge, backbone) simultaneously. No assumption that DDoS is only a perimeter/edge problem.
2. **Pattern-aware, not traffic-volume-only** — Detect 1990s-style floods AND 2026-style low-and-slow, amplification, reflection, and protocol-abuse attacks. Differentiate bulk legitimate traffic (a user downloading GBs) from attack signatures.
3. **Graph-first enrichment** — All signals feed the graph with DDoS-specific node types, edges, and temporal relationships. The ML sidecar (already integrated) consumes this graph as its feature store.
4. **Time-to-react first** — The moment an anomaly pattern is confirmed, the response chain is triggered: DDoS Cloud Sink API → BGP prefix advertisement change → BMP post-incident assurance.
5. **BMP as assurance layer** — BMP is used post-detection to verify that BGP route changes (blackhole/community signalling to cloud DDoS scrubbing) landed correctly and that prefix restoration worked.
6. **False positive discipline** — All DDoS detections require multi-source corroboration or ML confidence threshold before escalating. Single-source volume spikes are flagged for triage, not auto-remediated.

---

## Epic Overview

| Epic | Title | Priority |
|------|-------|----------|
| DS-1 | DDoS Signal Extraction: Multi-Source Telemetry Enrichment | P0 |
| DS-2 | DDoS Graph Schema: Nodes, Edges, Attack Fingerprints | P0 |
| DS-3 | DDoS Detection Rules: Pattern Library + Synthesizer Integration | P0 |
| DS-4 | DDoS Response: Cloud Sink Integration + BGP Signalling | P0 |
| DS-5 | BMP Post-Incident Assurance: Prefix Convergence Verification | P1 |
| DS-6 | DDoS ML Feature Pipeline: Graph-to-Feature Export for Sidecar | P1 |
| DS-7 | DDoS Incident UI: Timeline, Attack Map, Mitigation Tracker | P1 |
| DS-8 | DDoS Simulation + Testing Harness | P2 |

---


# App Dependency Matrix — Scoping Doc

> D2-8 T5 — 2026-05-17. Scoping for the App Dependency Matrix thread. Decides
> which ingestion angle to pursue first and documents what "done" looks like.

## Problem

bonsai detects network faults but cannot answer: "which application services
are affected by this fault?" The App Dependency Matrix closes that gap by
building a graph of application-to-network dependencies derived from real
traffic observations.

## Three angles

### Angle A — Netflow/sflow/IPFIX (recommended first)

**What**: ingest netflow v9/v10 or sflow v5 from routers and switches. Parse
flow records into `AppFlow` edges: src IP → dst IP:port, with bytes/packets/duration.

**Why first**:
- Well-defined binary format; parsers well-understood.
- Most network devices already export netflow — zero new agent installs.
- Produces IP-level app dependency matrix immediately.
- No host instrumentation required.

**Gap**: IP-level only. No process names, no service names. "10.0.1.5:443" is
a web service but we don't know which one.

**Effort**: 3 days (D2-8 T2).

### Angle B — eBPF socket-level visibility (DV3)

**What**: eBPF program on hosts that intercepts socket calls. Attributes each
flow to a PID/service name. Produces a process-aware app graph.

**Why later**: requires kernel ≥5.8, host agent deployment, and security review.
Builds on the DV1 eBPF spike (`experiments/ebpf_spike_20260516/`).

**Payoff**: full service-to-service dependency graph with process attribution.

**Effort**: 1 week agent + 3 days bonsai integration.

### Angle C — OTel span correlation (DV3)

**What**: accept OTLP traces. Parse span `peer.address`, `db.name`, `http.url`
attributes to materialise service dependencies from distributed traces.

**Why later**: depends on D2-6 (OTLP receiver). Requires application teams to
instrument services with OTel — not universally available.

**Payoff**: logical service graph (service names, not just IPs). Best for
microservices environments.

**Effort**: 2 days after D2-6 T2.

## Decision: Angle A first

Implement Angle A (netflow) in DV2. Angles B and C in DV3.

Rationale:
1. Unblocks `service_path_degraded` (D2-8 T4) without host instrumentation.
2. Netflow exporters already enabled on most lab routers.
3. The `HostEndpoint` graph node type is reused by D2-6 T1 (LLDP Host nodes).

## Data model (D2-8 T3)

```cypher
// AppFlow edge (aggregated per 60s)
(:HostEndpoint {address: "10.0.1.5"})-[:APP_FLOW {
  dst_port: 443,
  protocol: "TCP",
  bytes_per_sec: 12400,
  packets_per_sec: 84,
  last_seen_ns: 1716000000000000000
}]->(:HostEndpoint {address: "10.0.2.8"})

// HostEndpoint links to Device via LLDP adjacency (D2-6 T1)
(:HostEndpoint {address: "10.0.1.5"})-[:CONNECTED_TO]->(:Device {address: "leaf-01.dc1"})
```

## Detection rule: service_path_degraded (D2-8 T4)

Fires when:
1. An `AppFlow` edge's `bytes_per_sec` drops >80% within 60 seconds.
2. The network path between the two `HostEndpoint` nodes (via routing table)
   passes through a device that has a `bgp_session_down`, `interface_down`, or
   `bfd_session_down` detection in the last 5 minutes.

This is the "is the app broken because the network is broken?" answer.

## Done when (DV2)

- `src/streaming/netflow.rs` accepts netflow v9 + v10/IPFIX on UDP 2055.
- Lab router exports netflow to bonsai.
- `AppFlow` edges visible in the MCP graph Explorer.
- `service_path_degraded` fires when a lab BGP session goes down and affected
  flows drop.

## Done when (DV3)

- eBPF agent running on at least one lab host, service names attributable.
- OTel spans correlated with netflow flows for the same endpoint pair.
- Full service graph: `Service --[depends_on]--> Service` derived from OTel + netflow.

## Open questions

1. **Sampling rate**: at 1:1000 sflow sampling, low-bandwidth flows (backup
   jobs, health checks) will be invisible. Accept this for DV2 — only
   high-bandwidth production paths matter for `service_path_degraded`.
2. **NAT**: source IPs may be NATted. The reconciler (D2-9) needs to resolve
   NATted IPs to canonical `HostEndpoint` addresses using ARP/NDP data from LLDP.
3. **Collector placement**: netflow UDP must reach bonsai. In distributed
   collector mode, the collector nearest the router should ingest and forward
   parsed `AppFlow` events — not raw UDP datagrams — to the core via gRPC.

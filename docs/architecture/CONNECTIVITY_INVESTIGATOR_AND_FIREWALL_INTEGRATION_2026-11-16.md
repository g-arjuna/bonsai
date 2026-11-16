# Connectivity Investigator & Firewall Evidence Integration — Design

Status: **design + backlog intake only** (2026-11-16). No implementation
authorized or performed. Extends BN-024 path/effective-policy
architecture and the agent-native NetOps strategy (§15 addendum); does
not create a competing programme. Bonsai is a network context and
evidence engine — not a SIEM, not a firewall manager.

## E1. Connectivity Investigator — common request contract

### Request (UI / API / MCP — one shape, three surfaces)

```json
{
  "source":      {"ip": "10.1.0.5", "host": "optional", "service": "optional"},
  "destination": {"ip": "10.2.0.9", "host": "optional", "service": "https"},
  "scope":       {"tenant": "...", "site": "...", "vrf": "..."},
  "protocol":    "tcp",
  "dport":       443,
  "direction":   "forward|reverse|both",
  "time_window": {"from": "...", "to": "..."} | {"as_of": "..."},
  "symptom":     "unreachable|intermittent|slow"
}
```

All fields bounded: IP/host literals validated, port 0–65535, time
window clamped (default last 1h, max 24h), tenant/site/VRF resolved
through the existing scope authority — a request never widens scope.

### Evidence classes — kept distinct, never conflated

| Class | Meaning | Source |
|---|---|---|
| Topology | What *should* be connected (graph, RIB, LLDP) | core graph |
| Expected forwarding | What policy/ routes say *should* happen | BN-024 effective-policy path computation |
| Observed forwarding | What telemetry shows *did* happen (flows, session state, BMP/EVPN) | AppFlow, BGP/BFD state, collector feeds |
| Probe results | Active measurement (ping/tracepath where authorized) | bounded probe runner, operator-gated |
| Policy simulation | What a firewall *would* do — simulation, not observation | external policy engine query (see E2) |

### Path computation

- Forward path: S→D via existing path/profile machinery (ECMP
  enumerated, not collapsed — report all equal-cost next hops with
  per-path evidence).
- Reverse path: computed independently for stateful devices — a
  permitted forward flow through a stateful firewall implies a
  return-state expectation; absence of reverse evidence is reported as
  its own finding, not folded into "unreachable".
- NAT: record pre/post-NAT identities separately; a flow observed
  post-NAT must be attributed to the pre-NAT tuple for policy
  evaluation. Never silently rewrite evidence.
- Stateful firewall behavior: expected conntrack state (new/est/
  related/invalid) is modeled as a hypothesis alongside observed
  state; simulation output is labeled `simulated`, observed flow
  records labeled `observed`.

### Epistemic rules (non-negotiable)

- An **observed permit is not proof of destination delivery** — the
  flow may have been dropped downstream, blackholed, or the responder
  silent. Delivery requires endpoint evidence (response flow,
  probe reply, service log).
- A **missing log is not proof of deny** — log silence has at least
  four explanations (permit-unlogged, drop-unlogged, collector gap,
  retention expiry). Every negative finding carries an explicit
  `evidence_gap` list naming which explanations remain live.
- Every conclusion ships a **bounded deterministic evidence bundle**:
  capped node/edge/flow counts, stable ordering, content hash, and an
  explicit `next_checks` list (e.g. "query SIEM for deny events in
  window", "verify collector coverage for device X", "run authorized
  probe"). No open-ended "investigate further".

### API/MCP surface

- `POST /api/investigate/connectivity` → synchronous bounded bundle
  (default ≤5 s compute, hard cap; async job for larger windows).
- MCP tool `bonsai_connectivity_investigate` with identical schema —
  read-only, scope-checked against the caller's principal (extends the
  agent-native evidence interface from the NetOps strategy §15).
- UI: source/destination form → evidence bundle rendered with the five
  evidence classes visually separated.

## E2. Firewall integration strategy

Preferred order (each stage independently useful; later stages only
where earlier ones cannot answer):

1. **Query existing SIEM/security lake on demand** — Splunk search API,
   Sentinel/Loom, Elastic. Bonsai already has outbound Splunk HEC;
   the *inbound evidence query* adapter is new but small.
2. **Read-only firewall management-plane queries where authorized** —
   Panorama/Candidate config queries, FortiGate CMDB read, FMC API
   reads. Strictly query-only; no policy push, no packet capture.
3. **Selective structured operational events and aggregates** —
   syslog/CEF/LEEF/JSON security events already forwarded to the
   existing archive path, IPFIX `firewallEvent` IEs as *aggregates*
   (deny counts per tuple/class), never raw flow-per-packet volume.
4. **Full traffic/security log ingestion — OUT OF SCOPE.** Bonsai does
   not become a firewall log collector.

### Standards reviewed (as of Oct 2026)

| Standard | Relevance |
|---|---|
| RFC 7011 IPFIX | Flow export; `firewallEvent` Information Elements carry rule-id/action — usable as aggregate evidence |
| IANA firewallEvent IE 233 | Standards-based deny/permit event typing for IPFIX/structured logs |
| RFC 8158 NAT logging | NAT translation logging format for NAT44/64/NPTv6 evidence |
| RFC 5103 bidirectional flow | One record per bidirectional flow — halves cardinality vs unidirectional |
| RFC 9710 registry corrections | IANA registry fixes relevant to IE interpretation |
| RFC 8519 / OpenConfig ACL | Structured ACL policy model — candidate for *expected-policy* import (not traffic) |
| RFC 5425 secure syslog | TLS transport for the security-event path (see SYS-F) |
| CEF / LEEF / JSON | De-facto SIEM event formats; CEF needs bounded parsing, JSON preferred |
| OCSF | Emerging normalized security schema — evaluate for the SIEM adapter response mapping |

### Vendor capability notes (read-only assessment)

- **Palo Alto Panorama/PAN-OS:** REST API candidate-config + log-query
  queries (`type=log&log-type=traffic&filter=...`); Panorama gives
  multi-firewall aggregation. Read-only API key scoped to log viewer.
- **FortiAnalyzer/FortiGate:** FortiAnalyzer REST search API;
  FortiGate CMDB read for policy queries. Read-only ADOM-scoped.
- **Cisco FMC/eStreamer/packet tracer:** eStreamer (REST/SQS) for
  connection events; FMC API for policy reads; packet tracer is a
  *simulation* — maps to our `policy_simulation` class, not observed
  evidence.
- **Check Point:** management API + log exporter (OpenTelemetry/CEF);
  SmartEvent for correlation (query, don't replicate).
- **Cloud firewalls:** AWS Network Firewall / Azure Firewall flow
  logs land in the cloud SIEM — covered by stage 1, no direct creds.
- **Splunk search API:** `POST /services/search/jobs` + results poll;
  the natural first adapter target.

### Future SIEM-backed read-only proof of concept (not built)

- Role: `bonsai-evidence-reader` — dedicated service principal, never
  the operator's personal account.
- Tenant/index: explicit allowlist of indexes/sourcetypes; no
  cross-tenant wildcard.
- Limits: max time window (≤24 h), max events returned (≤10k),
  max query duration, query-template allowlist.
- **No unrestricted agent-generated SPL/CLI** — the adapter accepts
  structured predicates (device, tuple, action, rule-id, time) and
  compiles them to vendor queries internally. Policy export and
  packet captures are never issued.
- Evidence references: every returned record carries `sourcetype`,
  `index`, `event.id`, `query_sid` so findings cite the exact SIEM
  evidence.
- Credential isolation: SIEM creds live in the existing vault/secret
  path, separate from device creds; restricted deployments without
  direct firewall credentials work through stage-1 only.
- Data minimization: only fields needed for the evidence class are
  returned; no raw log blobs to hosted LLMs — outbound AI calls see
  the deterministic bundle, not raw SIEM records.
- Operator authorization: the PoC requires an explicit operator
  grant per tenant; agent-initiated queries are read-only and
  audit-logged (`audit.rs` path).

## Dependencies & acceptance (future slices)

- Depends on: BN-024 effective-policy path engine maturity, scope/
  principal model (BN-005), evidence-bundle format (this doc +
  agent-native strategy §15).
- Acceptance for the *design* is this document + backlog entries.
  Implementation acceptance (later): forward+reverse path correctness
  on a synthetic multi-vendor topology; ECMP/NAT/stateful cases;
  evidence-gap honesty tests (permit-without-delivery, missing-log
  ambiguity); SIEM PoC returning cited records within limits.

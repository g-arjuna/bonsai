# BN-021d — NetFlow v9/IPFIX identity isolation and template lifecycle

Date: 2026-11-11. Branch: `feat/bn021d-flow-exporter-identity` (base `main`,
post-merge of PR #2 / BN-021c+f).

Slice scope: correct, secure NetFlow v9/IPFIX exporter attribution and
template lifecycle — without inventing an authentication guarantee for
plain UDP.

## Findings addressed

| # | Finding | Class | Resolution |
|---|---------|-------|------------|
| A | `TelemetryUpdate.target` and provisional `exporter_id` derived from bare `peer.ip()` + Source ID/ODID — two NAT-shared exporters collapsed to one downstream identity | P0 attribution | New collision-resistant provisional source key `flowsrc/{collector}/{proto}/{ip}:{port}/{sid|odid}{id}`; `TelemetryUpdate.target`, `TelemetryEvent::NetflowRecord.exporter_id`, AppFlow node id and `BonsaiEvent.device_address` all carry it. `exporter_address` (bare IP) remains in the payload as transport evidence only. |
| B | IPFIX enterprise-IE parser advanced 8 bytes per field after only a 4-byte minimum check — malformed input could read past the buffer | P0 parser safety | `parse_ipfix_template_set` bounds-checks every specifier before reading; enterprise bit consumes the 4-byte PEN and stores `pen = Some(p)` so enterprise IEs are never decoded with standard IANA definitions; variable-length fields mark the template non-decodable; a truncated specifier discards the whole pending set. IPFIX message `Length` is validated against datagram length with zero-padding rule (RFC 7011 §3.1). |
| C | Template cache bounded only at the outer context (4096); nested domain/template maps unbounded; no expiry, no restart handling, no port-change isolation | P0/P1 lifecycle | `FlowTemplateCache` keyed by `(collector_id, SocketAddr, version)`; per-context domain cap, per-domain template cap, per-template field cap, global template-count and byte budgets; TTL expiry (`flow_template_ttl_secs`, default 1800); idle-context eviction; v9 `sys_uptime` decrease and IPFIX `export_time` regression clear the domain's templates (generation bump → `session_epoch`); source port is part of the context key so port changes cannot inherit templates. |
| D | IPFIX UDP template withdrawals would have been ambiguous vs TCP semantics | Spec compliance | RFC 7011 §8.4: UDP withdrawals (Field Count = 0) are ignored — counted via `bonsai_netflow_udp_withdrawal_ignored_total`, never delete state. TCP/SCTP withdrawal semantics documented as future work. |
| E | Graph `CARRIES_FLOW` linked any Device whose address matched the observed transport IP — unverified UDP could merge into canonical inventory | Trust boundary | `write_netflow_record` now fails closed for every flow update: no `DeviceAddress` refresh, no `CARRIES_FLOW`, no `SRC_HOST`/`DST_HOST`, and no flow-derived device alerts. Plain UDP always resolves `Provisional`/`Unknown` with `device_id = null`; `resolve_flow_identity` only reaches `IdentityResolver::resolve_verified_ingress` when a transport-attested `VerifiedIngressPrincipal` is supplied (impossible from UDP today), and no JSON artifact can substitute for that boundary. |

## Identity architecture

Every published flow carries a `TelemetrySourceIdentity` envelope plus
flat JSON fields:

| Field | Status | Meaning |
|---|---|---|
| `exporter_address` | evidence | bare observed transport IP (post-NAT) |
| `exporter_endpoint` / `transport_source` | evidence | `ip:port` as received |
| `collector_id` | configured scope | ingress namespace (`bn021d-nat-lab` in lab) |
| `protocol_version` | evidence | 9 or 10 |
| `observation_domain_id` + `observation_domain_kind` | evidence | NetFlow source-id vs IPFIX ODID — protocol context, NOT identity |
| `exporter_id` | **provisional** | `flowsrc/...` source key — observational separation only |
| `session_epoch` | evidence | template-generation counter (bumped on detected restart) |
| `identity_status` | resolver output | `provisional` for all plain UDP |
| `device_id`, `canonical_exporter_id` | **verified only** | `null` unless a verified-ingress resolution produced them |
| `identity_resolution` | resolver output | full `IdentityResolution` JSON for audit |

Trust rule enforced in code and tests: UDP source IP, source port,
Source ID and ODID are *evidence*. None of them — alone or combined —
can mint `device_id`. Only `VerifiedIngressPrincipal` (private fields,
`pub(crate)` transport-attestation constructor, fail-closed
deserialization — the BN-021f boundary) can drive
`resolve_verified_ingress`, and only credential-pinned bindings resolve.

## Template-cache design

- **Key:** `(collector_id, SocketAddr{ip,port}, protocol_version)` →
  `HashMap<domain_id, DomainTemplates>` → `HashMap<template_id, TemplateDef>`.
- **Bounds** (`NetflowConfig` → `FlowCacheLimits`):
  `max_flow_contexts` (4096), `max_flow_domains_per_context` (64),
  `max_flow_templates_per_domain` (512), `max_flow_fields_per_template`
  (256), `max_flow_total_templates` (65536), `max_flow_cache_bytes`
  (64 MiB approx), `flow_template_ttl_secs` (1800),
  `flow_context_idle_secs` (3600). Eviction is oldest-first per
  dimension; global limits enforce down to 90% in batches.
- **Expiry:** `lookup_template` returns `Expired` past TTL — expired
  templates never decode; data sets referencing them increment
  `bonsai_netflow_template_expired_drop_total`.
- **Replacement:** a new definition for an existing template id
  replaces it atomically (RFC 7011 §8.4 UDP semantics).
- **Restart:** v9 `sys_uptime` regression or IPFIX `export_time`
  regression clears that domain's templates and bumps `generation`
  (`bonsai_netflow_restart_detected_total`). Sequence gaps are counted
  (`bonsai_netflow_sequence_gap_total`) but do NOT invalidate templates
  (UDP reorder/loss makes them unreliable).
- **Port change:** source port is inside the context key — a new port
  is a new context with empty template state.
- **Collector restart:** all state is in-memory; restart = empty cache.
  Verified live (phase 3) and in `collector_restart_discards_template_state`.
- **Unknown templates:** data sets with no live template are dropped
  with `bonsai_netflow_unknown_template_drop_total`; no layout is ever
  guessed and no cross-context template is borrowed.

## Malformed-packet handling

- v9: header ≥20 B, every flowset `length ≥ 4` and in-bounds; template
  records fully validated before insert (oversized field counts skip
  or abort the set — never partial templates).
- IPFIX: `Length` ∈ [16, datagram], trailing bytes must be zero;
  every set bounded; template records bounds-checked per specifier;
  enterprise fields consume PEN; varlen fields → `decodable=false`.
- All failures `bail!` → counted (`bonsai_netflow_malformed_packet_total`)
  and logged at debug; the receiver loop never panics and keeps state
  consistent (a bad packet cannot partially mutate a domain).

## Live NAT verification (this directory)

Topology: `lab/acceptance/bn021d-flow-nat/` — three exporter containers
(exp-a/b/c) on one docker bridge; `bn021d-nat` DNATs udp/2055 → bonsai
and SNATs ALL exporter traffic to the single source `10.241.0.200`.
Bonsai runs the real `bonsai` binary (mode `all`, core GraphStore)
with `[streaming.netflow] enabled, collector_id="bn021d-lab"`.

Observed at the collector (see `runtime/appflow-all.json`):

| exporter_id (provisional) | decoded src | notes |
|---|---|---|
| `flowsrc/bn021d-nat-lab/nf9/10.241.0.200:49152/sid7` | 192.0.2.1 | exp-a v9 |
| `flowsrc/bn021d-nat-lab/ipfix/10.241.0.200:49152/odid7` | 192.0.2.11 | exp-a ipfix, same numeric domain 7 — protocol isolation |
| `flowsrc/bn021d-nat-lab/nf9/10.241.0.200:49153/sid7` | 192.0.2.1 | exp-a port change — new context, no inherited templates |
| `flowsrc/bn021d-nat-lab/nf9/10.241.0.200:28277/sid7` | 203.0.113.1 | exp-b — same sid7+tid400, DIFFERENT template def; decoded with its own layout |
| `flowsrc/bn021d-nat-lab/ipfix/10.241.0.200:28277/odid7` | 203.0.113.2 | exp-b ipfix |
| `flowsrc/bn021d-nat-lab/nf9/10.241.0.200:4983/sid99` | 198.51.100.9 | exp-c unbound claims |
| `flowsrc/bn021d-nat-lab/nf9/10.241.0.200:49999/sid99` | 192.0.2.55 | post-restart re-arm check |

Every row: `exporter_address = "10.241.0.200"` (the single NAT IP),
`identity_status = "provisional"`.

- **`runtime/appflow-carries.json`: 0 rows** — no `Device-[:CARRIES_FLOW]->AppFlow`
  edges: unverified UDP never acquired canonical device attribution.
- **`runtime/devices.json`: 0 rows** — no Device nodes were created
  from flow observations.
- **Template-id collision (Scenario B):** exp-b reused v9 sid=7/tid=400
  and ipfix odid=7 with different field layouts; each decoded only its
  own records (203.0.113.x vs 192.0.2.x) — no contamination.
- **Source-port change (Scenario D):** exp-a rebound 49152→49153;
  data sent before re-templating was dropped, post-template data landed
  under the new `…:49153` key.
- **v9 restart (Scenario E):** uptime regression cleared templates;
  data before refresh dropped, refreshed template decoded.
- **Malformed flood (Scenario H):** 5 malformed datagrams (truncated,
  bad version, garbage v9, impossible IPFIX length, random bytes) —
  all rejected with `netflow parse error` debug lines
  (`runtime/bonsai-service.log`), zero panics, service stayed up.
- **Collector restart (Scenario I):** `docker restart bn021d-bonsai`
  mid-scenario; post-restart data-only packets were dropped until a
  fresh template arrived (in-memory state discarded by design).
- **Unbound claims (Scenario G):** exp-c's plausible sid=99 flows
  stayed `provisional` with `device_id = null` — no merge into any
  canonical identity.

## Remaining limitations

- **Observability, not authentication.** Two exporters that present the
  *identical* post-NAT transport tuple (same IP:port, protocol, domain)
  at different times are indistinguishable to the collector — the
  `flowsrc` key cannot separate them and canonical attribution stays
  gated. This is documented, not papered over.
- **Canonical device attribution requires authenticated flow ingress**
  (e.g. a TLS/DTLS-terminating collector relay producing a
  `VerifiedIngressPrincipal`, or IPFIX over TCP/SCTP with
  authentication). `resolve_flow_identity` is the extension point;
  no such transport exists in this slice.
- **TCP/SCTP IPFIX** is not implemented; UDP withdrawal semantics are
  per RFC 7011 §8.4 — TCP withdrawal handling is future work.
- Template TTL/idle/context bounds are configurable but conservative
  defaults; very high-rate multi-domain exporters may evict early
  (fail-safe: drops, never mis-decodes).

## Independent-review hardening (11 November, post-review)

The independent review of PR #5 confirmed and fixed four defects; the
claims above are updated accordingly:

- **Untrusted graph side effects removed.** Provisional UDP flows no
  longer call `upsert_device_address` (previously every flow refreshed
  the shared IP-keyed `DeviceAddress.updated_at`, including aliases of
  pre-existing canonical Devices), and no longer create
  `SRC_HOST`/`DST_HOST` edges into `HostEndpoint` inventory. Because no
  authenticated NetFlow/IPFIX ingress exists yet, the graph writer now
  fails closed for EVERY `streaming/netflow/flow` update: no
  `CARRIES_FLOW`, no `DeviceAddress` freshness update, and no
  `HostEndpoint` edges. Even a complete serialized
  `identity_resolution` object is self-asserted msgpack evidence —
  collector ingest can carry forged payloads — so JSON shape alone can
  never authorize canonical state. Flow-derived utilization/DDoS
  StateChangeEvents are disabled for the same reason (they key off the
  observed exporter address and could attach to a colliding Device).
  Transport observations remain
  visible as source-scoped evidence on the AppFlow node
  (`exporter_id` = `flowsrc/...`, `exporter_address`).
- **Packet handling is transactional.** `parse_v9`/`parse_ipfix`
  validate every flowset/set boundary BEFORE any cache mutation
  (context creation, restart/sequence notes, template clearing,
  template insertion). A rejected packet can no longer modify template
  state — verified by `malformed_trailing_set_rejects_whole_packet` and
  `regressed_timestamp_plus_malformed_preserves_templates`. The earlier
  "all-or-nothing" wording is now actually enforced.
- **AppFlow id is the full 5-tuple.** It now includes `src_port`
  (`{exporter_id}:{src}:{sport}:{dst}:{dport}:{proto}`), so two flows
  differing only in source port no longer merge. `AppFlow.src_port`
  was added (schema + idempotent ALTER). Pre-existing AppFlow nodes
  written under the old id format are stale flow records, not
  correctness state; they are superseded by new writes.
- **New regression coverage** (`tests/bn021d_flow_exporter_isolation.rs`,
  now 15 integration tests): `graph_provisional_flow_preserves_canonical_state`
  seeds a colliding canonical Device + DeviceAddress + cross-site
  HostEndpoints and asserts zero authoritative edges/freshness updates,
  including a forged update carrying a complete self-asserted
  `identity_resolution`; `appflow_id_includes_source_port`; the two
  transactional-malformed tests above; and partial set-header
  regressions in `malformed_packets_are_safe`.

## Files

- `api/` — graph query results: `appflow-all.json` (7 provisional
  flows), `appflow-carries.json` (0 rows — no canonical device links),
  `devices.json` (0 rows).
- `service-logs/` — `bonsai-service.txt` (incl. `netflow parse error`
  rejections), `scenarios.txt`, `nat-rules.txt`.
- `runtime/` — gitignored live-run working dir (graph DBs, journals);
  reproducible via the lab scripts below.
- Lab scripts: `lab/acceptance/bn021d-flow-nat/{up,run_scenarios,verify}.sh`,
  `flow_exporter.py`, `bonsai.toml`, `image/`.
- Tests: `tests/bn021d_flow_exporter_isolation.rs` (15 integration +
  3 unit tests in `src/streaming/netflow.rs`).

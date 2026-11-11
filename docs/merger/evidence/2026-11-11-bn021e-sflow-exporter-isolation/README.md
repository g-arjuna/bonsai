# BN-021e (slice 1) — sFlow exporter-identity isolation

Date: 11 November 2026. Branch: `feat/bn021e-sflow-exporter-identity`
(base `main` `eabd85a`, post-merge of PR #5 / BN-021d).

Slice scope: apply the BN-021 trusted-identity contract to sFlow —
observational separation without inventing an authentication guarantee
for plain UDP. Syslog/SNMP/OTLP adapters remain open under BN-021e.

## Defects addressed

| # | Defect | Class | Resolution |
|---|--------|-------|------------|
| A | `agent_address` inside the sFlow header was treated as the authoritative exporter identity; the UDP transport endpoint (`peer`) was read and then discarded | P0 attribution | New provisional source key `sflowsrc/{collector}/{transport ip:port}/agent{addr}/sub{id}`; `TelemetryUpdate.target`, `exporter_id`, AppFlow node id and `BonsaiEvent.device_address` all carry it. `exporter_address` is now the observed transport IP; `agent_address`, `agent_sub_id`, `sflow_sequence`, `sflow_uptime` are preserved as self-reported evidence fields. |
| B | `sub_agent_id`, `sequence_number`, `uptime` were never parsed | P1 evidence loss | Header fields at offsets 12/16/20 are now read and carried into every published flow/counter update. |
| C | AppFlow id `{src}:{dst}:{dport}:{proto}` contained no exporter or source port — flows from different agents AND different 5-tuples merged globally | P0 correctness | AppFlow id is now `{exporter_id}:{src}:{sport}:{dst}:{dport}:{proto}` (same full-5-tuple rule as BN-021d); `AppFlow.src_port` is set. |
| D | `write_sflow_record` created `DeviceAddress`, `CARRIES_FLOW`, `SRC_HOST`/`DST_HOST` edges and fired utilization/DDoS device alerts from unauthenticated evidence; `write_sflow_counters` mutated canonical `Interface` counters keyed by the self-reported agent address | P0 trust boundary | Both projections now fail closed for every sFlow update: AppFlow + source-scoped `app_flow_event`/`sflow_counters_event` only. The unused `emit_flow_utilization_event`/`emit_ddos_pps_spike_event` helpers and the `ddos_cfg` write path were removed. Even a complete serialized `identity_resolution` is self-asserted msgpack evidence and cannot authorize canonical state. |
| E | No collector namespace for sFlow | P1 isolation | `SflowConfig.collector_id` added (default `default`, serde-compatible); settings restart preserves it via struct update. |

## Identity model (unchanged contract)

- Transport endpoint (`SocketAddr`) = observed evidence.
- sFlow agent address + sub-agent id = self-reported claims (evidence).
- Collector id = configured ingress scope.
- Canonical `device_id` requires a future typed, transport-attested
  `VerifiedIngressPrincipal` path — `resolve_flow_identity` is the
  extension point and is never reachable from plain UDP.

## Verification

- `cargo test -j 1 --test bn021e_sflow_exporter_isolation` — 7/7:
  NAT-shared agents separated, agent claim cannot merge distinct
  sources, collector scope separates keys, counter samples stay
  source-scoped, graph fails closed incl. forged
  `identity_resolution`, AppFlow src_port identity, same agent claim
  never merges AppFlows.
- `cargo test -j 1 -p bonsai --lib streaming::sflow` — unit tests for
  source-key separation and provisional resolution.
- `cargo check -p bonsai --all-targets` — clean.

## Remaining limitations

- Canonical sFlow attribution is disabled until authenticated ingress
  exists (BN-021g acceptance defines the verified path).
- sFlow counter samples no longer update canonical `Interface` state;
  they remain visible as `sflow_counters_event` broadcasts.
- sFlow has no template/restart state machine; sequence gaps are
  carried as evidence (`sflow_sequence`) but not yet gap-detected.

# BN-021 P0 — inbound listener & source-attribution audit (10 Nov 2026)

Repository-level audit of every telemetry/API listener: network path,
source-IP handling, and device-identity resolution. Triggered by the
10 Nov docker-proxy speaker collapse and the production architecture
review.

## Listener inventory

| Listener | Bind | Transport | Source-IP usage | Identity resolution today | Risk |
|---|---|---|---|---|---|
| BMP (live path) | `src/bin/routing_service.rs:47` | TCP | `peer_sock.ip()` → `speaker_addr` = PeerKey speaker | TCP source IP only; Initiation sysName parsed (`rbmp-core` `parse_initiation_tlvs` → SysName/SysDescr) but **not used for identity** | Critical — collapses under NAT (proven live) |
| BMP (legacy) | `src/streaming/bmp.rs:166` | TCP | same pattern | Retired per BN-007; retirement still pending projection parity | Same, dormant |
| Syslog | `src/signals/syslog.rs:111` UDP, `:131` TCP | UDP+TCP | `peer_addr` = transport sender stored on event | `hostname` parsed from message + `source_vendor` — both present but no trust model; peer_addr is sender-of-record | High — relay vs originator conflated |
| SNMP traps | `src/signals/snmp.rs:302` | UDP | `peer_addr` = UDP source | Source IP only; no engineID extraction seen | High under NAT/relay |
| NetFlow v9/IPFIX | `src/streaming/netflow.rs:45` | UDP | `peer.ip()` → `exporter_ip` on every flow record | Template cache keyed by `peer` socket addr (ip+ephemeral port) + source_id; `exporter_ip` propagated downstream = collapsed under NAT | Critical — both identity AND template-cache correctness |
| sFlow | `src/streaming/sflow.rs:39` | UDP | `agent_address` **from payload** | ✅ Already correct model — payload agent addr is authoritative, UDP src only logged | Low — reference implementation for others |
| OTLP | `src/streaming/otlp.rs:20` | TCP/HTTP | n/a for identity | Resource attributes expected (verify per-signal) | Low if attrs used |
| BGP-LS | `src/streaming/bgp_ls.rs:65` | TCP | peer addr | Configured-peer context | Medium |
| HTTP APIs | `server_startup.rs:1623`, `diagnostic_server.rs:185` | TCP | n/a | JWT auth | n/a |

## Docker network path (lab, verified 10 Nov)

- `bonsai-routing` now `network_mode: host` → BMP source IPs preserved
  (`172.100.207.11`/`.12` distinct again). App reaches it via bridge
  gateway `172.20.0.1:8081`.
- Previously: published `127.0.0.1:11019` unreachable from clab AND a
  stray host debug process owned `172.100.207.1:11019`; bridge-published
  port masqueraded sources to `172.20.0.1`.
- UDP listeners (syslog/snmp/netflow/sflow) were never exercised under
  the collapsed path — same NAT risk applies to any future published
  UDP ports.

## Identity-resolution gaps (application level)

1. **BMP**: `speaker_addr` = raw TCP source. Initiation sysName/sysDescr
   IS parsed by rbmp-core but unused — cheapest correct fix: resolve
   exporter → inventory device at session setup, key PeerKey by resolved
   exporter id with transport addr retained as evidence.
2. **PeerKey/speaker keys**: `PeerKey` speaker = transport IP; API
   `?speaker=` filter is transport-address semantics. Needs
   device_id/exporter_id selectors (BN-021 P1).
3. **Journal reconciliation**: stale `172.20.0.1` and older
   `172.100.207.x` speakers coexist in replay — need explicit alias
   retirement, not auto-merge.
4. **Change assurance immutability**: FIXED this session — captures and
   verdicts are now append-only (`css-…-{phase}-{ts}`, `civ-…-{ts}`);
   pairing selects newest per phase by `captured_at_ns`.
5. **No quarantine**: ambiguous/unknown exporters currently flow into
   RIB state unmarked — need `completeness_state`/identity-status
   propagation before multi-device production use.

## sFlow as the reference pattern

`sflow.rs:80` comment: "agent_address is the authoritative source
identity (not UDP src IP)" — payload-carried identity wins over
transport. Apply the same principle per protocol: BMP→Initiation,
NetFlow→engine/source-id+template ctx, Syslog→HOSTNAME (with trust),
SNMPv3→engineID, OTLP→resource attrs.

## Next implementation slices (ordered)

1. BMP exporter identity: capture Initiation sysName/sysDescr per
   connection, add `exporter_identity`/`identity_status` to speaker
   session, prefer it over `speaker_addr` in PeerKey + API selectors.
2. NetFlow/IPFIX: key template cache by (resolved exporter, source_id)
   not socket peer; propagate exporter identity on flow records.
3. Syslog/SNMP: split `transport_sender` vs `reported_hostname`/
   `engine_id` fields; identity-confidence tagging.
4. Monitoring-health: speaker disconnect → `monitoring_unavailable`
   distinct from peer-down (no silence inference).

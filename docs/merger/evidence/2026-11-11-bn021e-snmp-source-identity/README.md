# BN-021e (slice 3) — SNMP trap source-identity isolation

Date: 11 November 2026. Branch: `feat/bn021e-snmp-source-identity`
(base `main` `ed1a8a4`, post-merge of PR #9 / BN-021e syslog slice).

Slice scope: apply the BN-021 trusted-identity contract to the SNMP
*trap receiver* — separate observed transport, collector namespace,
configured polling target, and every self-reported protocol claim, and
fail the graph projection closed. SNMP *polling*/target management and
OTLP remain out of scope (OTLP is the next BN-021e slice; polling is
not redesigned here).

## Defects addressed

| # | Defect | Class | Resolution |
|---|--------|-------|------------|
| A | `SnmpTargetMap::resolve` matched the transport source IP to a configured target and returned that device's address as `TelemetryUpdate.target` — a sender at (or spoofing, at L3) a device IP acquired canonical-looking attribution | P0 attribution | New provisional source key `snmpsrc/{collector}/udp/{peer_ip}:{peer_port}` scopes `TelemetryUpdate.target`, `source_key`, `exporter_id` and every `StateChangeEvent.device_address`. The source-IP→target match is demoted to an explicit `candidate` hypothesis (`unique_target_match` / `ambiguous_target_match` / `unresolved`). |
| B | Duplicate configured IPs (e.g. private addresses reused across tenants) resolved to an arbitrary first match | P0 correctness | `resolve` collects all matches: exactly one → `Unique` candidate (only then may it steer vendor/role/site annotation and read-only probes); more than one → `Ambiguous` with all candidate addresses preserved and **no** arbitrary selection; none → `Unresolved`. Ambiguous/unresolved candidates never steer vendor selection. |
| C | Dedup ran on `(peer_ip, trap_oid)` **before** community/USM filtering — a rejected packet could suppress a later accepted observation with the same OID | P0 ordering | Filters now run first; dedup is keyed by `(trust domain, source key, trap OID)` and evaluated only for packets that passed filtering. `snmp_dedup_domain` isolates provisional vs future verified domains; the map is swept and hard-capped (8192 entries) with an eviction metric. |
| D | Community allowlist only applied when the parsed community was non-empty — an empty/missing/malformed community bypassed a configured allowlist | P1 | `community_permits`: with a non-empty allowlist, v1/v2c traps require a *present, listed* community; parse failures (`version=="unknown"`, empty community) fail closed. v3 is governed by the USM check. A matching community remains "accepted for parsing", never authenticated identity. |
| E | SNMPv3 security-name allowlist could be read as authentication | P0 mislabel | Recognized `security_name` is preserved as a claim with `usm_auth_status: "unverified"`. **The receiver does not verify the USM auth digest** — `authNoPriv`/`authPriv` are NOT claimed. Full USM authentication is explicitly deferred to BN-021g. Unknown security names are still dropped when `v3_users` is configured. |
| F | `write_snmp_fact_event` called `upsert_device` + mutating `join_snmp_fact`; `write_signal_state_change_event` upserted a Device and wrote canonical `StateChangeEvent`s + `REPORTED_BY` edges keyed to the claim-derived address | P0 trust boundary | Both projections fail closed: provisional `StateChangeEvent` scoped to the `snmpsrc/...` key (no `Device` upsert ⇒ no `REPORTED_BY` possible) + read-only candidate probes for interface/BGP/OSPF/ISIS facts. Facts emit `snmp_fact_candidate` / `snmp_fact_unresolved` / (ambiguous probe → `snmp_fact_unresolved`). |
| G | Self-reported v3 engine ID was double-hex-encoded (`hex::encode` applied to an already-hex string), corrupting the evidence | P2 correctness | `parse_usm_security_name` returns hex; the caller now uses it directly. Engine ID preserved as `usmEngineHex:contextEngineHex`, explicitly unverified. |
| H | No collector namespace for SNMP | P1 isolation | `SnmpConfig.collector_id` added (serde default `default`); settings restart preserves it via struct update. |

## Identity model (unchanged BN-021 contract)

Eight identity layers are now kept separate end-to-end:

1. **Observed UDP transport sender** (`peer_addr` = `ip:port`) — evidence.
2. **Collector namespace** (`SnmpConfig.collector_id`) — configured scope.
3. **Reported SNMP agent identity** — v1 `agent-addr` →
   `reported_agent_address` (self-reported, unverified).
4. **SNMPv3 security name** → `usm_security_name` +
   `usm_auth_status: "unverified"`.
5. **Reported engine IDs** → `reported_engine_id`
   (`usmEngine:contextEngine`, self-reported).
6. **Configured polling target** → `candidate` hypothesis only; a
   configured target is NOT automatically the authenticated trap sender.
7. **Candidate inventory identity** — read-only graph probes keyed on
   the candidate address.
8. **Authenticated canonical identity** — requires a future typed,
   transport-attested `VerifiedIngressPrincipal` (BN-021g);
   `resolve_flow_identity` is always called with `verified_ingress=None`,
   so `identity_status` is `provisional`/`unknown` and `device_id` is
   `null` by construction.

Source ports are transport evidence, not device identity; NAT-shared
IPs with distinct ports/claims stay in distinct source keys; nothing
merges on a shared IP or an engine-ID claim.

## SNMPv3 authentication status — accurate classification

- **Implemented:** BER/USM parameter *parsing* (engine ID, boots, time,
  security name), configured security-name allowlist, explicit
  `usm_auth_status: "unverified"` on every v3 observation.
- **NOT implemented:** verification of `msgAuthenticationParameters`
  (HMAC digest), timeliness (boots/time), or `privProtocol` decryption.
  Encrypted scoped PDUs (`scopedPDU` not a plaintext SEQUENCE) are
  rejected with an explicit parse error — reported, never silently
  dropped.
- **Consequence:** an SNMPv3 packet carrying an *allowed* username with
  a bogus/absent auth digest is accepted for parsing and published as
  provisional evidence — identical trust level to v1/v2c. No claim of
  authenticated SNMPv3 is made anywhere.

## What is preserved

- Raw forensic archive (`SnmpArchive`, JSONL) — verbatim event incl.
  community string and `raw_hex`; archive append now `flush()`es before
  returning (same tokio `fs::File` write-buffering fix as the syslog
  slice). **`flush()` drains tokio's pending blocking write so readers
  observe the bytes — it is NOT an `fsync` durability guarantee**; a
  host crash can still lose the tail of the archive. Periodic/explicit
  `sync_all` policy is a documented follow-up, not claimed here.
- Trap parsing for v1/v2c/v3-plaintext; OID-pattern fact extraction;
  Nokia TIMETRA-BGP-MIB peer-address index-suffix parsing.
- Community/USM filtering, dedup (now post-filter, bounded, metric'd),
  governor shedding, supervisor packet accounting, pattern hot-reload.
- Collector metrics (`bonsai_snmp_traps_total`,
  `bonsai_snmp_community_blocked_total`, `bonsai_snmpv3_usm_blocked_total`,
  `bonsai_snmp_dedup_suppressed_total`, `bonsai_snmp_fact_join_total`
  labelled `candidate`/`unresolved`).
- Credential hygiene: published `TelemetryUpdate.value` redacts
  `community` and `raw_hex` (`community_redacted`/`raw_hex_redacted`
  flags); redacted material remains only in the restricted archive.
- Read-only candidate probes: a `unique_target_match` candidate still
  correlates `link_down`/`link_up`→`Interface`,
  `bgp_peer_state`/`bgp_peer_backward_transition`→`BgpNeighbor`,
  `ospf_nbr_state_change`→`OspfNeighbor`,
  `isis_adj_state_change`→`IsisAdjacency` and reports `graph_probe`
  (incl. current canonical `graph_state`) in event detail — evidence,
  never a join. `correlation_buffer` was updated to the new
  `snmp_fact_candidate`/`snmp_fact_unresolved` event types so SNMP facts
  keep contributing correlation keys.

## Deliberate behavior changes

- `snmp_fact_joined` / `snmp_fact_orphan` → `snmp_fact_candidate` /
  `snmp_fact_unresolved`. Anything keyed on the old event types (Python
  rules, dashboards) no longer fires for SNMP facts — intentional fail
  closed.
- No `Device` upsert, no `DeviceAddress` alias refresh, no
  `REPORTED_BY` edge, no canonical `Interface`/`BgpNeighbor`/
  `BfdSession`/`IsisAdjacency`/`OspfNeighbor`/service/QoS/host mutation
  from trap claims. SNMP-driven canonical state changes and
  device-scoped remediation under trap-claimed identity are **off**
  until BN-021g.
- Device-scoped `SignalPolicy` filters key on `update.target` (now the
  source key): device-scoped SNMP policies will not match — same
  functionality-loss-not-fail-open assessment as the syslog slice
  (provisional scope gains no canonical authorization; role/site scopes
  still apply).
- The legacy mutating joiners (`join_snmp_fact` and the canonical
  `write_state_change_event` path for SNMP) were removed; retained in
  git history for the BN-021g verified path.

## Verification

- `cargo test -j 1 --test bn021e_snmp_source_identity` — **11/11**:
  source-IP/target impersonation (A), NAT-shared senders (B), duplicate
  tenant IPs (C), forged SNMPv3 username + engine ID (D/E), community
  allowlist empty/missing/disallowed/malformed (F), dedup poisoning (G),
  graph isolation + forged `identity_resolution` (H/I), v1/v2c/v3 parse
  compatibility + encrypted-PDU explicit error (J), archive+restart (K),
  malformed BER/oversized/truncated/v4 safety (L).
- `cargo test -j 1 --lib snmp` — parser/receiver unit tests incl.
  rewritten target-resolution tests (candidate semantics).
- BN-021b/c/d/e/f + BN-022 regression batch — see
  `docs/build-times/runs.csv`.
- Simulated/synthetic transport only (loopback UDP); no live SNMP
  agent/lab run — live verification remains under BN-021g.

## Credential exposure review

- Published `TelemetryUpdate.value` redacts `community` and `raw_hex`
  (`community_redacted`/`raw_hex_redacted` flags); the strings survive
  only in the JSONL archive.
- Graph `StateChangeEvent.detail` for SNMP carries the *published*
  (redacted) value; `SnmpFactEventDetail` contains only parsed fact
  fields — no community, auth material or packet hex.
- `BonsaiEvent.detail_json` inherits the same redacted payload; SSE/API
  surfaces therefore never see community strings.
- Logs record peer IP, version and event type only — never the
  community, USM names are not logged either.
- The archive lives at `runtime/signals/snmp.jsonl` (umask-dependent
  permissions, same as the syslog archive) and is not exposed by any
  HTTP/API route; it must be treated as a restricted forensic store
  because it intentionally retains community strings and raw packets.
  Tightening to `0600` is a reasonable follow-up shared with syslog.

## Remaining limitations

- Canonical SNMP attribution disabled until authenticated ingress
  exists (BN-021g: verified binding or authenticated trap transport).
- No durable provisional SNMP fact store — `snmp_fact_candidate` events
  are `StateChangeEvent`s in the graph; bounded-history follow-up may be
  needed.
- Archive crash durability relies on filesystem flush semantics (see
  above); no `fsync` per line.
- UDP source-IP spoofing is a transport limitation: a forged identical
  `ip:port` is indistinguishable at this layer (documented in
  `SnmpDedup`).
- Residual (pre-existing, syslog slice): `correlation_buffer` still keys
  on `syslog_fact_joined`/`syslog_fact_orphan` — dead arm for the new
  `syslog_fact_candidate`/`unresolved` names. Flagged for follow-up.

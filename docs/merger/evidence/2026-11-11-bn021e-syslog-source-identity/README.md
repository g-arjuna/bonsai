# BN-021e (slice 2) — syslog source-identity isolation

Date: 11 November 2026. Branch: `feat/bn021e-syslog-source-identity`
(base `main` `16eaa4e`, post-merge of PR #8 / BN-021e sFlow slice).

Slice scope: apply the BN-021 trusted-identity contract to syslog —
separate observed transport, relay, and self-reported originator
identity, and fail the graph projection closed. SNMP/OTLP adapters
remain open under BN-021e.

## Defects addressed

| # | Defect | Class | Resolution |
|---|--------|-------|------------|
| A | `SyslogTargetMap::resolve` mapped a *claimed* hostname inside the message to the configured address of a registered device; the resulting `TelemetryUpdate.target` was that canonical address, so telemetry was processed as originating from the device | P0 attribution | New provisional source key `syslogsrc/{collector}/{transport}/{peer}[/conn{id}]`. `TelemetryUpdate.target`, `source_key`, `exporter_id` and every `StateChangeEvent.device_address` carry it. The hostname match is demoted to an explicit `candidate` hypothesis (`unique_hostname_match` / `ambiguous_hostname_match` / `unresolved`). |
| B | Duplicate hostnames across sites/tenants were silently resolved to the first configured entry | P0 correctness | `resolve` now collects all hostname matches: exactly one → `Unique` candidate; more than one → `Ambiguous` with all candidate addresses preserved and **no** arbitrary selection; none → `Unresolved`. Ambiguous/unresolved claims no longer steer vendor pattern selection. |
| C | The previous fallback fabricated a target address from the claimed hostname or peer IP | P1 | Removed — unresolved sources carry no candidate address at all. |
| D | `write_syslog_state_change_event` → `write_signal_state_change_event` called `upsert_device` + `write_state_change_event` keyed to the resolved (claim-derived) address; `write_syslog_fact_event` called `upsert_device` and ran mutating joiners (`join_isis_fact` wrote `IsisAdjacency`; service/QoS/connection joiners created/updated nodes) | P0 trust boundary | Both projections now fail closed for EVERY syslog update: provisional `StateChangeEvent` scoped to the `syslogsrc/...` key (no `REPORTED_BY` edge possible — no `Device` is upserted) + source-scoped `BonsaiEvent`. Facts emit `syslog_fact_candidate` / `syslog_fact_unresolved` instead of `joined`/`orphan`. |
| E | Serialized `identity_status`/`identity_resolution`/`device_id` fields in the payload could be self-asserted | P0 | The graph path performs NO writes derived from payload identity fields; candidate data is used only for read-only probes. A forged `verified_ingress`/`resolved` claim changes nothing (regression test `forged_identity_and_state_claims_fail_closed`). |
| F | TCP had no session scope — sequential connections from one peer shared identity | P1 | Per-connection monotonic id (`/conn{n}`) added to TCP source keys. |
| G | No collector namespace for syslog | P1 isolation | `SyslogConfig.collector_id` added (default `default`, serde-compatible); settings restart preserves it via struct update. |

## Identity model (unchanged BN-021 contract)

- Transport endpoint (`SocketAddr`) = observed evidence.
- Claimed hostname / structured-data fields = self-reported claims.
- Relay vs originator: the transport source key identifies the *sender*
  (which may be a relay); `reported_hostname` preserves the claimed
  originator separately inside each payload.
- Collector id = configured ingress scope.
- Canonical `device_id` requires a future typed, transport-attested
  `VerifiedIngressPrincipal` path — `resolve_flow_identity` is always
  called with `verified_ingress = None`, so resolution is
  `provisional`/`unknown` by construction.

## What is preserved

- Raw JSONL archive (`SyslogArchive`) — verbatim message + parsed fields.
- Parquet telemetry archive — now partitioned by source key.
- Fact extraction (vendor patterns still selected by unique candidate).
- Shun evaluation — device-scoped rules match the unique candidate
  address (same semantics as before; the claim was always
  self-reported), otherwise the source key; global rules unaffected.
- Change-detection syslog triggers (vendor from unique candidate).
- Operational metrics (`bonsai_syslog_events_total`, fact-join counter
  now labelled `candidate`/`unresolved`/`ambiguous`).
- Read-only candidate probes: a unique hostname candidate still
  correlates against `BgpNeighbor`/`Interface`/`BfdSession`/
  `OspfNeighbor`/`IsisAdjacency` state and reports `graph_probe` in the
  event detail — as evidence, never as a join.

## Deliberate behavior changes

- `syslog_fact_joined` / `syslog_fact_orphan` event types are replaced
  by `syslog_fact_candidate` / `syslog_fact_unresolved`. Python
  detection rules matching the old types (disagreement monitors,
  orphan-gap monitor) no longer fire for syslog — intentional fail
  closed; they asserted agreement between unverified claims and
  canonical state. Category events keep `syslog_{category}` names but
  are scoped to the source key, so per-source alerting still works
  without impersonating a device.
- Device-scoped `SignalPolicy` filters key on `update.target`, which is
  now the source key — device-scoped syslog policies will not match
  (role/site scopes still apply). Recorded as residual gap.
- The legacy mutating joiners (`join_syslog_fact` and friends) were
  removed; they remain in git history for the BN-021g verified path.

## Verification

- `cargo test -j 1 --test bn021e_syslog_source_identity` — 10/10:
  impersonation, relay separation, TCP session scope, NAT collision,
  tenant ambiguity, unresolved origin, raw evidence, restart/replay,
  forged-identity fail-closed, read-only candidate probe.
- `cargo test -j 1 -p bonsai --lib syslog` — 13/13 incl. the 44-fixture
  corpus (`syslog_fixture_catalog_matches_expected_fields`) and the
  rewritten graph unit tests.
- `cargo check -p bonsai --lib --tests` — clean.
- BN-021/BN-022 regression batch — see `docs/build-times/runs.csv`.

## Remaining limitations

- Canonical syslog attribution is disabled until authenticated ingress
  exists (BN-021g defines the verified path, e.g. mTLS relay or
  per-device credentials).
- `syslog_fact_candidate` events carry read-only probe results; no
  durable provisional fact store exists yet.
- No live syslog relay lab was run; acceptance is fixture + synthetic
  transport level. Live-relay verification remains under BN-021g.

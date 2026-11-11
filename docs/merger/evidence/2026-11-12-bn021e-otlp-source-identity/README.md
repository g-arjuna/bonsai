# BN-021e (slice 4) — OTLP source/resource identity isolation

Date: 12 November 2026. Branch: `feat/bn021e-otlp-source-identity`
(base `main` `c50f688`, post-merge of PR #10 / BN-021e SNMP slice).

Slice scope: apply the BN-021 trusted-identity contract to the OTLP
HTTP/JSON receiver (`/v1/traces`, `/v1/metrics`) and its graph
projection. This completes the BN-021e unauthenticated-adapter set
(NetFlow/IPFIX, sFlow, syslog, SNMP, OTLP). OTLP/gRPC is NOT
implemented or claimed — the receiver is HTTP/JSON only, a subset of
the OpenTelemetry protocol.

## Defects addressed

| # | Defect | Class | Resolution |
|---|--------|-------|------------|
| A | `service.name` / `host.name` / `peer.address` — all producer-supplied — populated `TelemetryUpdate.target`, so claims became graph authority | P0 attribution | New provisional source key `otlpsrc/{collector}/http/{peer_ip}:{peer_port}` derived ONLY from the observed TCP peer via axum `ConnectInfo`. `X-Forwarded-For`, `Forwarded` and arbitrary headers are never consulted. Target, `source_key`, `exporter_id` and every `StateChangeEvent.device_address` carry the source key. |
| B | `write_otlp_span` upserted `Application` nodes from claimed `service.name`; `write_otlp_metrics` did the same plus wrote `cpu_pct`/`memory_mb`/`req_per_sec`/`error_rate`/`metric_json` — an arbitrary producer could rewrite application inventory and health | P0 trust boundary | Both writers fail closed: provisional `StateChangeEvent`s (`otlp_span_event`/`otlp_metric_event`) scoped to the source key + read-only candidate probes. No `Application` upsert, no health-column writes. |
| C | `WHERE d.address STARTS WITH $pfx` joins (RUNS_SERVICE, APP_IMPACTED_BY_NETWORK correlation) could match devices sharing an address prefix | P0 correctness | All prefix joins removed. Candidate probes use EXACT matches only and are read-only evidence — an exact match is not authentication. |
| D | `APP_IMPACTED_BY_NETWORK` edges were created from temporal coincidence between a claimed `peer.address` and unrelated network detections | P0 false attribution | Edge creation removed entirely; temporal proximity remains investigable via source-scoped events, never as a canonical relationship. |
| E | Inconsistent application identity: spans used `app:{service_name}`, metrics used `{peer_address}:{service_name}` or bare service name — collision-prone and merge-prone | P1 correctness | Consistent provisional id `otlpapp/{source_key}/{service_name}` scoped to the observing connection+collector; same service name from different sources never merges; raw labels are never global identifiers; no permanent nodes created. |
| F | Unbounded per-request work: body arrays (resource groups × scopes × spans × data points × attributes) had no application-level budgets | P1 resource safety | Explicit budgets: 32 resource groups, 32 scopes/group, 1024 items/scope, 4096 items/request, 256 attributes scanned, 512-char string fields, timestamp sanity (non-positive or >+24h → receive time), non-finite metric values dropped (never coerced to 0). Drops counted via `bonsai_otlp_items_dropped_total`. Axum `Json` retains the default 2 MiB body limit; serde_json recursion limit bounds nesting. |
| G | No collector namespace for OTLP | P1 isolation | `OtlpConfig.collector_id` added (serde default `default`); settings restart preserves it via struct update. |

## Identity model

Nine layers kept separate:

1. **Observed HTTP transport peer** (`ConnectInfo<SocketAddr>`) —
   evidence; per-connection `ip:port`.
2. **Collector namespace** (`OtlpConfig.collector_id`).
3. **Authenticated ingress principal** — none exists for OTLP/HTTP;
   not fabricated.
4. **OTLP resource attributes** — self-reported claims.
5. **Instrumentation scope** — preserved in payload structure.
6. **Reported service identity** — `service.name` claim.
7. **Reported host identity** — `host.name` claim.
8. **Reported remote peer** — `peer.address` claim.
9. **Canonical entity identity** — requires BN-021g verified ingress;
   `identity_status` is `provisional`, `device_id` is `null`,
   `authority` is `unverified` by construction.

Source-key semantics: the key identifies the TCP *connection endpoint*
observed by this collector — appropriate for HTTP keep-alive (many
requests per connection) and bounded by live connections. Reconnects
get new ephemeral ports → new provisional keys (session-scoped, not
stable device identity; no unbounded persisted identities are minted
per request). Behind NAT, two senders on one IP are separated by port;
a shared IP+port is indistinguishable at this layer (documented UDP/TCP
limitation). Reverse-proxy deployments collapse to the proxy endpoint
— a trusted-proxy boundary is explicitly deferred to BN-021g.

## What is preserved

- `/v1/traces` and `/v1/metrics` JSON routes unchanged; OTLP JSON
  subset (resourceSpans/scopeSpans/spans; resourceMetrics/
  scopeMetrics/metrics with gauge/sum/histogram data points).
- Attribute extraction (`service.name`, `host.name`, `peer.address`,
  `db.name`, `http.url`, span name/ids, metric name/kind/value).
- Histogram rate estimate (sum/count), `asDouble`/`asInt` values.
- Parquet telemetry archive (`classified_event_type` → `otlp_span` /
  `otlp_metrics`) — durable evidence when `archive.enabled`.
- Graph `StateChangeEvent`s — durable graph evidence.
- Source-scoped `BonsaiEvent`s (SSE) with `identity_status:
  "provisional"`.
- Collector metrics + new `bonsai_otlp_items_dropped_total`.
- Graceful shutdown via axum `with_graceful_shutdown`.
- Read-only candidate probes: exact-match `Device.address`,
  `Device.hostname`, `HostEndpoint.ip` lookups reported inside event
  detail as `candidate_probe` — evidence for investigation, never a
  join, and the probe result itself discloses only data the same
  graph already exposes to authorized readers (StateChangeEvent detail
  inherits existing graph read ACLs — BN-022 boundary unchanged).

## Deliberate behavior changes

- **Canonical Application health metrics no longer update from OTLP.**
  `cpu_pct`, `memory_mb`, `req_per_sec`, `error_rate`, `metric_json`
  on `Application` nodes now only change via verified/other sources.
  Previously-populated values remain but are now STALE relative to
  live OTLP producers — dashboards must distinguish
  authoritative/stale vs provisional (follow-up: surface
  `identity_status` in Application APIs/UI).
- **No new Application nodes** are created for unverified service
  names (previously every distinct `service.name`/`peer` combo could
  mint `app:*` nodes — a high-cardinality write amplifier).
- **No RUNS_SERVICE / HOST_RUNS_SERVICE / CARRIES_APPLICATION /
  APP_IMPACTED_BY_NETWORK** edges from OTLP claims.
- Event types: spans keep `otlp_span_event`; metrics gain
  `otlp_metric_event` (previously emitted no event at all — net gain
  in observability).
- Device-scoped `SignalPolicy` filters key on the source key (same
  non-fail-open assessment as syslog/SNMP).

## Verification

- `cargo test -j 1 --test bn021e_otlp_source_identity` — **8/8**:
  transport-vs-claims separation + provisional app id consistency (E/I),
  shared service names across collectors (D), malformed JSON /
  missing fields / oversized groups / huge attributes / absurd
  timestamps (J), graph fail-closed incl. forged service+device+
  identity_resolution, prefix collision, metric overwrite, network-
  impact correlation (A/B/C/F/G/H), source scoping (I), replay (K),
  live HTTP→bus→graph end-to-end (L).
- `cargo test -j 1 --lib` + BN-021b/c/d/e/f + BN-022 regression batch —
  see `docs/build-times/runs.csv`.
- Simulated verification: live axum receiver on loopback + direct
  `GraphStore::write`; no production OTLP exporter lab — live
  verification remains under BN-021g.

## Remaining limitations

- Canonical OTLP attribution disabled until authenticated ingress
  (BN-021g: mTLS collector gateway or equivalent verified boundary).
- Per-connection source keys are session-scoped; long-lived
  connections are stable, reconnects rotate keys — acceptable for
  evidence, not a device identifier.
- Reverse-proxy deployments see only the proxy endpoint; trusted
  proxy header handling requires an authenticated boundary first.
- Application health coverage from OTLP is now evidence-only;
  operator-facing staleness/coverage surfacing is a follow-up.
- `StateChangeEvent` volume grows with span/metric rate (bounded by
  ingestion budgets); durable provisional history policy is a
  BN-021g/retention concern.

## BN-021g proposal (next slice — see final report for detail)

1. **First bounded slice: authenticated collector relay for OTLP +
   flow protocols.** Reuse the BN-021f mTLS machinery
   (`VerifiedIngressPrincipal`, `match_ingress_credential`,
   `vexp:*` scope) to terminate OTLP HTTP and NetFlow/sFlow/syslog/
   SNMP relay collectors behind mutually-authenticated ingress.
   Rationale: one verified boundary restores canonical correlation
   for four protocols at once; SNMPv3 USM crypto verification is a
   separable later slice.
2. Verified principal → typed `VerifiedIngressPrincipal` at the graph
   boundary only; payload JSON can never mint it (already enforced —
   `transport_attested` is serde-skipped).
3. Canonical projection re-enabled per-protocol behind
   `IdentityStatus::Resolved` only; provisional path stays as
   quarantine/fallback.
4. Production acceptance matrix: NAT, duplicate IPs, impersonation,
   restart/replay, revocation, rotation, multi-protocol correlation.

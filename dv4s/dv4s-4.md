## DS-4 — DDoS Response: Cloud Sink Integration + BGP Signalling

### Analysis

Bonsai already has output adapters (`src/output/`) for Splunk, Elastic, Prometheus, ServiceNow EM, and SNMP. None are DDoS-cloud-sink-aware. DDoS mitigation platforms have their own APIs (Cloudflare Magic Transit, Akamai Prolexic, Lumen DDoS Mitigation, Radware DefensePro, Arbor Sightline) which receive "signal" API calls to activate scrubbing or blackholing. In parallel, BGP-based mitigations (RTBH via RFC 7999, FlowSpec via RFC 5575) can be signalled directly to upstream routers or to cloud providers as BGP communities. Bonsai needs to:

1. **Detect fast** → already built in DS-3.
2. **Signal cloud sink via API** → a new output adapter `src/output/ddos_cloud_sink.rs`.
3. **Signal via BGP** → a new `bgp_mitigation_signal.rs` that uses the existing `gnmi_set.rs` infrastructure to push RTBH community or FlowSpec rules to configured devices.
4. **Track the mitigation lifecycle** via `MitigationAction` nodes (DS-2 T1).
5. **Confirm via BMP** that the routes changed as expected (DS-5).

**Existing infrastructure to leverage:**
- `src/output/traits.rs` — `OutputAdapter` trait. New cloud sink adapters will implement this.
- `src/credentials.rs` + `CredentialVault` — All API keys for cloud sinks stored in vault by alias.
- `src/gnmi_set.rs` — Already supports gNMI SET operations. Route-policy community injection can be done via gNMI SET.
- `src/http_server/remediation.rs` — Human-in-the-loop (HITL) approval path already exists. BGP mitigation with HITL = safest path.
- `src/integrations/` — Pattern for new integration modules.

**Supported cloud sink integration models:**
- **Direct API**: Cloud provider REST/JSON API receives prefix + action (scrub/blackhole/rate-limit). Provider re-routes traffic to scrubbing centre.
- **BGP RTBH**: Bonsai announces prefix with `65535:666` (RFC 7999) community to upstream via policy. Upstream drops traffic at peering point. Fast but brutal — all traffic to prefix dropped.
- **BGP FlowSpec (RFC 5575)**: Bonsai injects FlowSpec rule (match src/dst/proto/port, action drop/redirect) into routing table. Fine-grained — can drop only SYN packets to port 443 from ASN 12345 while allowing others.
- **BGP Community Signalling to scrubber**: Announce prefix with provider-specific community (e.g., `64512:9999` for Cloudflare) to divert traffic through scrubbing without full blackhole.

### Tasks

**T1 — DDoS Cloud Sink output adapter**

New file `src/output/ddos_cloud_sink.rs`:

```rust
pub struct DdosCloudSinkAdapter {
    config: DdosCloudSinkConfig,   // from [ddos.cloud_sinks[]]
    vault: Arc<CredentialVault>,
    http: reqwest::Client,
    audit: OutputAdapterAuditLog,
}
```

- Implements `OutputAdapter` trait with `OutputTopic::DetectionEvents`.
- Filters bus for `source_type="detection"` events with `rule_id IN ["ddos_confirmed", "ddos_campaign"]`.
- On receive: calls provider-specific API to signal mitigation start.
- Supported provider adapters (pluggable via `provider_type` config field):
  - `CloudflareProvider`: `POST https://api.cloudflare.com/client/v4/accounts/{account_id}/magic-transit/routes` with prefix + blackhole/advertise action.
  - `AkamaiProvider`: Akamai Prolexic API `POST /session-provisioning/v1/sessions` with SID, prefix, action.
  - `LumenProvider`: Lumen/CenturyLink DDoS API `POST /v1/mitigations` with prefix, type=blackhole/divert.
  - `ArborProvider`: Arbor Sightline REST API `POST /api/sp/mitigations/` with TMS mitigation JSON.
  - `GenericRestProvider`: configurable REST endpoint, method, auth (Bearer/Basic/API-key-header), payload template (Handlebars-style `{{prefix}}` substitution). For any provider not natively supported.
- On API call: writes `MitigationAction` node to graph with full request/response (sanitised — API keys redacted).
- API auth: `credential_alias` in config → vault resolve.
- Config structure in `src/config.rs`:

```toml
[[ddos.cloud_sinks]]
name = "cloudflare-mt"
provider_type = "cloudflare"          # cloudflare / akamai / lumen / arbor / generic_rest
enabled = false                       # off by default — explicit enablement required
credential_alias = "cloudflare_api_token"
account_id = "xxx"                    # provider-specific
api_base_url = "https://api.cloudflare.com/client/v4"
min_confidence_to_trigger = 0.7       # confidence threshold from DS-3 T5
require_hitl = true                   # human approval before API call
hitl_timeout_seconds = 300
protected_prefixes = ["198.51.100.0/24", "203.0.113.0/24"]
auto_restore_after_minutes = 60       # if 0, no auto-restore
```

**T2 — BGP RTBH signalling via gNMI SET**

New file `src/integrations/bgp_mitigation.rs`:

- `BgpMitigationSignal` struct: `prefix`, `action` (rtbh/flowspec/community_signal/restore), `target_devices` (list of device addresses to apply on), `rtbh_community`, `flowspec_rule_json`.
- `announce_rtbh()`: uses `gnmi_set.rs` to push route-policy community tag to device.
  - Nokia SRL: gNMI SET on `/routing-policy/community-set[name=RTBH-BLACKHOLE]/member` + static route to Null0 with community.
  - Cisco IOS-XR: gNMI SET on `/routing-policy/route-policies/route-policy[name=RTBH_POLICY]/statements`.
  - Arista EOS: gNMI SET or SSH fallback (EOS gNMI SET has limited policy write support).
  - FRR: SSH/CLI via PyATS sidecar (FRR gNMI SET not fully available for BGP policy). Use `vtysh -c "route-map RTBH permit 10 ; match community RTBH_COMM"`.
  - **Safety**: `announce_rtbh()` ALWAYS requires HITL approval (`hitl_timeout_seconds` from config) before executing. The HITL check uses the existing remediation HITL mechanism (`src/remediation/`).
- `restore_prefix()`: reverse of `announce_rtbh()` — removes community from prefix, withdraws null route.
- `inject_flowspec_rule()`: injects RFC 5575 FlowSpec NLRI via BGP. This is a more advanced capability — requires BGP daemon support (FRR supports FlowSpec, Cisco XR supports it). Deferred to T5.
- `announce_scrubbing_community()`: applies provider-specific divert community instead of full blackhole. Less disruptive than RTBH.

**T3 — Mitigation lifecycle state machine**

Implement `DdosMitigationFsm` in `src/integrations/bgp_mitigation.rs`:

```
States: Idle → Detected → AwaitingHitl → Signalling → Active → Verifying → Mitigated → Restoring → Restored

Transitions:
  Idle         → Detected    : ddos_confirmed detection fires
  Detected     → AwaitingHitl: cloud sink or BGP RTBH enabled + require_hitl=true
  AwaitingHitl → Signalling  : operator approves (HITL)
  AwaitingHitl → Idle        : operator rejects (HITL) OR timeout
  Detected     → Signalling  : require_hitl=false (auto-trigger)
  Signalling   → Active      : cloud sink API returns 2xx OR gNMI SET succeeds
  Signalling   → Detected    : cloud sink API returns error (retry with backoff)
  Active       → Verifying   : BMP assurance check triggered (DS-5)
  Verifying    → Mitigated   : BMP confirms RTBH/scrubbing route received
  Verifying    → Active      : BMP check inconclusive (retry after 30s)
  Mitigated    → Restoring   : auto_restore_after_minutes elapsed OR operator restore command
  Restoring    → Restored    : cloud sink API restore 2xx + BMP confirms prefix restored
```

- Each transition emits a `BonsaiEvent` with `source_type="ddos_fsm"` for UI SSE streaming.
- `DdosEvent.state` field (DS-2 T1) is updated on every transition.
- SSE endpoint: `/api/ddos/events/stream` — clients subscribe for real-time state transitions.

**T4 — Mitigation API endpoints**

New handler file `src/http_server/ddos.rs`:

- `POST /api/ddos/mitigation/request` — trigger mitigation for a prefix/event (validates confidence, checks HITL if required, creates `MitigationAction` node, calls cloud sink adapter).
- `POST /api/ddos/mitigation/{id}/approve` — HITL approval for pending mitigation.
- `POST /api/ddos/mitigation/{id}/reject` — HITL rejection.
- `POST /api/ddos/mitigation/{id}/restore` — manual restore of a mitigated prefix.
- `GET /api/ddos/events` — list all `DdosEvent` nodes with state, confidence, affected prefixes, mitigation actions.
- `GET /api/ddos/events/{id}` — full detail of one event including attack vectors, corroborating detections, mitigation timeline.
- `GET /api/ddos/events/stream` — SSE stream for real-time DDoS event state changes.
- `GET /api/ddos/config` — current DDoS config (cloud sinks, protected prefixes, thresholds).
- `PATCH /api/ddos/config` — update DDoS config at runtime (persisted to ConfigItem DB).
- `GET /api/ddos/baselines` — current traffic baselines per device/interface/protocol.
- `GET /api/ddos/mitigations/active` — all currently active mitigations (state = Active or Mitigated).
- Role requirements: `GET` endpoints → Viewer+. Mitigation trigger/approve/restore → Operator+. Config PATCH → Admin.

**T5 — FlowSpec injection (advanced, deferred)**

- Implement RFC 5575 FlowSpec NLRI construction for FRR BGP daemon.
- FRR `vtysh` command generation: `bgp flowspec-mode {device_id}`, `flowspec src-prefix {prefix} proto tcp dst-port 80 action drop`.
- Validate via BMP: after FlowSpec inject, monitor BMP ROUTE_MONITORING for FlowSpec NLRI receipt by downstream BGP peers.
- This is a P2 task — requires lab validation with FRR FlowSpec capability before implementation.

**T6 — Protected prefix management API + UI**

- `GET /api/ddos/protected-prefixes` — list of prefixes in the protection scope.
- `POST /api/ddos/protected-prefixes` — add a prefix with metadata (owner, ASN, description, cloud_sink_bindings).
- `DELETE /api/ddos/protected-prefixes/{prefix}` — remove from protection scope.
- Stored as `ConfigItem` records with `config_class="ddos_protected_prefix"`.
- UI: `DdosConfig.svelte` sub-page within the DDoS dashboard (DS-7) for prefix management.
- **Auto-discovery**: when `AffectedPrefix` nodes are created by detection (DS-2 T1), prompt operator "Would you like to add 198.51.100.0/24 to the protected prefix list?"

**T7 — Rate limiting and safety guards**

Critical safety mechanisms to prevent accidental network disruption:
- **Max concurrent mitigations**: configurable `ddos.max_concurrent_mitigations` (default 3). If exceeded: queue new mitigations for HITL even if `require_hitl=false`.
- **Cool-down period**: `ddos.mitigation_cooldown_seconds` (default 300) — cannot re-trigger mitigation for same prefix within cool-down after restore.
- **Confidence floor**: `ddos.min_confidence_for_auto_trigger` (default 0.75) — below this, ALWAYS require HITL regardless of config.
- **Protected devices**: `ddos.never_apply_rtbh_on` — list of device addresses where RTBH gNMI SET is never permitted (e.g., core routers where mis-config could cause outage).
- **Dry-run mode**: `ddos.dry_run = true` — all API calls and gNMI SETs are simulated (logged but not executed). Default `true` on first install.
- All safety guard violations are audit-logged as `ddos_safety_guard_triggered`.


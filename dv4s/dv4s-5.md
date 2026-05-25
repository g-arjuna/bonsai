## DS-5 — BMP Post-Incident Assurance: Prefix Convergence Verification

### Analysis

BMP (BGP Monitoring Protocol, RFC 7854) gives Bonsai a passive, real-time view of what BGP routes every monitored router has in its Adj-RIB-In (routes received from peers) and Loc-RIB (routes selected for the routing table). This makes BMP the ideal **assurance layer** for DDoS mitigation confirmation:

1. **Mitigation confirmation**: When Bonsai issues an RTBH community or calls a cloud sink API, the expected outcome is that the attacked prefix gets routed to a null route or a scrubbing centre. BMP should show the prefix's next-hop change (to 0.0.0.0/Null, or to a scrubbing centre address) on relevant BGP sessions within a convergence window (typically 10-60 seconds).

2. **Scrubbing community confirmation**: If a provider-specific divert community was announced, BMP on the upstream peering session should show the prefix being re-advertised WITH the community back towards the network, confirming the provider received and actioned it.

3. **Restoration verification**: When mitigation is restored (RTBH withdrawn, scrubbing community removed), BMP should confirm the prefix is re-announced to legitimate next-hops within the convergence window.

4. **Failure escalation**: If BMP does not confirm within the convergence window, emit `ddos_mitigation_unconfirmed` detection and alert the operator. This prevents silent failures where the API call succeeded but BGP didn't propagate.

5. **Post-incident prefix audit**: After restoration, BMP data allows auditing: were there any unexpected origin ASN changes? Were any more-specific prefixes injected during the attack? Did any peer de-prefer the prefix during the incident? This is the "post-incident analysis" dimension.

**Existing infrastructure:**
- `src/streaming/bmp.rs`: Full BMP parser, ROUTE_MONITORING, STATS_REPORT, PEER_UP/DOWN.
- `src/graph/mod.rs`: `BgpSession`, `DdosRouteEvent` (added DS-2 T1), `HAS_DDOS_ROUTE_EVENT` rel.
- `src/streaming/bmp.rs`: `write_bmp_route_monitoring()` already writes prefix path attributes.
- D4-11 (completed): BMP STATS_REPORT parsing writes adj_rib_in route counts.

### Tasks

**T1 — BMP route event classifier for DDoS patterns**

Extend `write_bmp_route_monitoring()` in `src/streaming/bmp.rs`:

For each ROUTE_MONITORING UPDATE, classify the event against active `DdosEvent` nodes:
- **RTBH confirmation check**: If prefix matches an `AffectedPrefix` with `rtbh_applied=true` AND next-hop changes to `0.0.0.0` or `192.0.2.1` (RFC 5737 documentation range used as null) OR AS_PATH contains configured RTBH peer ASN → write `DdosRouteEvent(type=rtbh_community_received)` and call `confirm_mitigation_action()`.
- **Scrubbing community check**: If prefix matches `AffectedPrefix.prefix` AND UPDATE communities contain configured scrubbing community value → write `DdosRouteEvent(type=scrubbing_community_signalled)`.
- **Unexpected origin detection**: If prefix is in `protected_prefixes` AND UPDATE origin ASN NOT in `allowed_origin_asns` config AND this is a new announcement (not withdrawal) → write `DdosRouteEvent(type=unexpected_origin_as)`. This is the hijack early-warning.
- **Prefix restoration check**: If prefix was previously `rtbh_applied=true` AND UPDATE is a new announcement with a non-null legitimate next-hop AND community list does NOT contain RTBH community → write `DdosRouteEvent(type=prefix_restored)` and call `confirm_restoration()`.
- **Victim withdraw**: If prefix in `protected_prefixes` AND UPDATE is a `WITHDRAWN_ROUTES` entry (not just implicit withdraw) → write `DdosRouteEvent(type=victim_prefix_withdrawn)`.

New helper: `lookup_active_ddos_event_for_prefix(prefix: &str) -> Option<String>` — Cypher query against `AffectedPrefix` nodes with active `DdosEvent` links.

**T2 — Assurance timer + confirmation engine**

New module `src/integrations/bmp_assurance.rs`:

```rust
pub struct BmpAssuranceEngine {
    graph: Arc<GraphStore>,
    pending_confirmations: Arc<Mutex<HashMap<String, PendingConfirmation>>>,
    // keyed by MitigationAction.id
}

struct PendingConfirmation {
    mitigation_action_id: String,
    ddos_event_id: String,
    expected_event_type: String,    // "rtbh_community_received" / "prefix_restored"
    prefix: String,
    deadline_ns: i64,
    check_interval_secs: u64,
}
```

- `register_pending(mitigation_action_id, ddos_event_id, prefix, expected_type, convergence_window_secs)` — adds to pending map.
- Background sweep every 5s: for each `PendingConfirmation`, query graph for `DdosRouteEvent` matching expected_type + prefix + occurred_at_ns > mitigation_requested_at_ns.
- If found: call `confirm_mitigation_action(mitigation_action_id, confirmed_at_ns, source="bmp_assurance")`. Updates `MitigationAction.confirmed_at_ns` and `confirmation_source`. Transitions `DdosEvent.state` to `Mitigated` or `Restored` via FSM.
- If not found before deadline: emit `ddos_mitigation_unconfirmed` detection (severity=`critical`) with context. Also emits `BonsaiEvent(source_type="ddos_assurance", event_type="mitigation_unconfirmed")`.
- Wired into `server_startup.rs` alongside the BMP receiver.

**T3 — BMP STATS_REPORT DDoS correlation**

Extend `write_bmp_statistics_report()` in `src/streaming/bmp.rs`:

During an active `DdosEvent` (state = Active or Mitigating):
- Monitor `rejected_prefixes` (STATS type 0): sudden drop in rejected prefixes for a previously attacked session may indicate filtering is working.
- Monitor `adj_rib_in` count: rapid growth in Adj-RIB-In can indicate prefix injection/hijack attack.
- Monitor `duplicate_prefix_withdrawal` (STATS type 4): high rate → possible BGP instability caused by attack.
- Write counter history to `BgpSession.adj_rib_in_history_json` (last 10 readings as JSON array with timestamps) for trend analysis.
- Fire `ddos_bgp_table_instability` (severity=`high`) if adj_rib_in changes >10% within one STATS_REPORT cycle AND active DdosEvent exists.

**T4 — Post-incident BGP prefix audit**

New API endpoint `GET /api/ddos/events/{id}/bgp-audit`:

After a DdosEvent transitions to `Restored` state, generate a BGP audit report:
- **Origin ASN timeline**: Were there any unexpected origin ASN advertisements for affected prefixes during the incident window?
- **More-specific prefix analysis**: Were any /25, /26, /27 more-specifics of the attacked prefix announced by unknown ASNs? (MOAS — Multiple Origin AS, hijack signal).
- **Convergence time**: Measured delta between `MitigationAction.requested_at_ns` and `MitigationAction.confirmed_at_ns` per action.
- **Session stability**: Were any BMP sessions lost during the incident (could indicate router CPU DDoS impacting BMP)?
- **Peer de-preference events**: Did any BGP peer withdraw sessions during the incident?

Data sources: `BgpSession`, `DdosRouteEvent`, `MitigationAction`, `BmpSession` nodes.
Returns JSON report suitable for incident post-mortem documentation.

**T5 — Protected prefix allowed-origin config**

Extend `src/config.rs` with `DdosProtectedPrefix`:

```toml
[[ddos.protected_prefixes]]
prefix = "198.51.100.0/24"
description = "Customer A primary block"
origin_asn = 65001
allowed_origin_asns = [65001, 65002]   # secondary ASNs allowed
rtbh_community = "65535:666"
scrubbing_community = "64512:9999"
cloud_sink_binding = "cloudflare-mt"    # which cloud sink to notify
auto_rtbh_on_confirmed = false          # require HITL before RTBH
```

- `scripts/import_protected_prefixes.py` — bulk import from CSV or from NetBox IP prefix table (NetBox enricher already has subnet nodes).
- `GET /api/ddos/protected-prefixes` uses both config-file entries and `ConfigItem` DB entries (config_class=ddos_protected_prefix).

**T6 — BMP session loss as DDoS indicator**

When BMP sessions (`BmpSession` nodes) are disconnected during a period with active `DetectionEvent` nodes on the same device, correlate:
- If `peer_down_reason = "remote_system_closed"` (BMP PEER_DOWN reason 4) AND `DetectionEvent` with `rule_id` matching interface/CPU stress pattern exists within ±60s → create `DDOS_CORROBORATED_BY(DdosEvent→DetectionEvent)` link.
- BMP session loss during attack is a secondary indicator — router CPU pressure from attack traffic caused BMP session to drop.
- Fire `ddos_bmp_session_lost_under_attack` (severity=`medium`) with context.

**T7 — Multi-session convergence tracking**

For large networks with many BGP speakers, RTBH propagation should be confirmed on ALL sessions, not just one:

- When `announce_rtbh()` fires, identify all BMP-monitored sessions where the affected prefix is in Adj-RIB-In.
- Register one `PendingConfirmation` per session.
- Track `confirmed_session_count` vs `expected_session_count` on `MitigationAction`.
- If `confirmed_session_count / expected_session_count < 0.8` after convergence window → `ddos_mitigation_partial_convergence` detection (severity=`high`) — some routers didn't receive the RTBH.
- Full confirmation only when all expected sessions confirm.


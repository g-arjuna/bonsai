## DS-7 — DDoS Incident UI: Timeline, Attack Map, Mitigation Tracker

### Analysis

The existing UI (`ui/src/routes/`) has purpose-built pages for Incidents, Topology, Explorer, Collectors, Governance, Settings, etc. None have DDoS-specific views. The DDoS UI requires a different information architecture than general incident management:

- **Time is the primary axis** — DDoS events evolve rapidly (seconds to minutes). A timeline view where detections, cloud sink calls, BGP changes, and confirmations are plotted chronologically is more valuable than a flat incident list.
- **Network scope needs spatial context** — an "attack map" showing which devices are involved, which prefixes are targeted, and where the attack traffic is flowing is essential for operator situational awareness.
- **Mitigation status must be glanceable** — operators under attack need to see at a glance: "is mitigation active? is it confirmed? is it working?"
- **Real-time updates are essential** — SSE streaming (already used in Governance.svelte) must drive live updates without polling.

The UI must not require a page refresh or manual query — it should auto-update via the `GET /api/ddos/events/stream` SSE endpoint defined in DS-4 T4.

### Tasks

**T1 — New `DdosDashboard.svelte` top-level page**

New file `ui/src/routes/DdosDashboard.svelte`:

**Layout (3 main areas):**

*Top strip — Live Status Bar*:
- Active attack count badge (green=0, amber=detecting, red=confirmed/campaign).
- Current mitigations count.
- Last updated timestamp (live from SSE).
- Global "DRY RUN MODE" warning badge if `ddos.dry_run=true` in config.
- Quick-action buttons: "View Active", "Configure", "Export Report".

*Left panel (1/3 width) — Active Events List*:
- One card per `DdosEvent` node with state ≠ `Idle` and ≠ `Restored`.
- Card shows: state pill (color-coded per FSM state), primary vector badge, affected prefix(es), confidence score ring (reuse Graph Health score ring component), device count, start time + elapsed.
- Click → loads event detail in right panel.
- "Past 24h" toggle shows resolved events.

*Right panel (2/3 width) — Event Detail + Timeline*:
- Header: DdosEvent state + confidence + affected prefixes.
- **Timeline tab** (default): vertical timeline of all events linked to this DdosEvent in chronological order:
  - `DetectionEvent` entries (colour: orange — detection signal)
  - `MitigationAction` entries (colour: blue — action taken)
  - `DdosRouteEvent` entries (colour: purple — BGP evidence)
  - State transitions (colour: grey — FSM state changes)
  - Each entry: source badge (gNMI/syslog/SNMP/NetFlow/sFlow/BMP), icon, timestamp, brief description.
- **Attack Vectors tab**: table of `AttackVector` nodes — vector_type, protocol, dst_port, observed_pps, observed_gbps, top_src_asns (chip list), tcp_flags_pattern.
- **Affected Prefixes tab**: table of `AffectedPrefix` nodes — prefix, origin_asn, rtbh_applied badge, scrubbing_applied badge, mitigation timing.
- **Mitigation tab**: ordered list of `MitigationAction` nodes — action_type, provider, status, confirmation badge (✓ BMP confirmed / ⏳ pending / ✗ unconfirmed), timing.
- **BGP Audit tab** (post-incident, visible when state=Restored): triggers `GET /api/ddos/events/{id}/bgp-audit`, renders convergence timeline, unexpected ASN events, more-specific prefix table.

**T2 — Attack map topology overlay**

Extend `ui/src/lib/Topology.svelte` (existing topology canvas) with DDoS overlay mode:

- New `ddosMode: boolean` prop. When true:
  - Devices involved in active `DdosEvent` nodes get a pulsing red halo overlay (CSS animation, `box-shadow: 0 0 12px var(--state-failed-border)`).
  - Role-specific overlays:
    - `targeted` devices: solid red halo.
    - `amplifier` devices: amber pulsing halo.
    - `transit` devices: grey overlay.
  - Active `AffectedPrefix` prefixes shown as floating label badges near the device with the most traffic exposure.
  - `CARRIES_FLOW` edges with high deviation_score shown as animated dashed lines (CSS keyframe animation on stroke-dashoffset).
  - Control plane stressed devices (`copp_drop_rate > threshold`): CPU icon overlay badge.
- DdosDashboard includes an embedded `<Topology ddosMode={true} />` panel (compact, no full-page navigation).
- Toggle button: "Topology View" / "Timeline View" for the right panel.

**T3 — Mitigation control panel**

Within the Event Detail right panel, **Mitigation tab** includes interactive controls:

- If `MitigationAction` is in `AwaitingHitl` state:
  - Large "APPROVE MITIGATION" button (green, prominent) + "REJECT" (grey).
  - Shows: action description, affected prefix, cloud sink name, dry-run indicator.
  - Confirmation modal: "You are about to instruct [Cloudflare Magic Transit] to divert traffic for [198.51.100.0/24]. This will affect [N] downstream devices. Confirm?"
  - On approve: `POST /api/ddos/mitigation/{id}/approve` + optimistic UI update.
- If state is `Mitigated`:
  - "RESTORE PREFIX" button (amber) — with same HITL modal pattern.
  - Shows: estimated traffic volume restored, cloud sink to notify.
- If state is `Active` and `auto_restore_after_minutes > 0`:
  - Countdown timer: "Auto-restore in [HH:MM:SS]". Operator can click "Restore Now" to skip.
- All control buttons are role-gated: Viewer sees them disabled with tooltip "Requires Operator role".

**T4 — DDoS configuration sub-page**

New sub-page accessible from DdosDashboard "Configure" button:

*Section 1 — Protected Prefixes*:
- Table of configured prefixes with columns: prefix, description, origin_asn, cloud_sink_binding, rtbh_community, auto_rtbh toggle.
- "Add Prefix" modal form.
- "Import from NetBox" button → calls `scripts/import_protected_prefixes.py` equivalent via API.

*Section 2 — Cloud Sinks*:
- Cards for each `[[ddos.cloud_sinks]]` entry showing: provider logo/name, enabled badge, last API call result, API response time.
- "Test Connection" button → `POST /api/ddos/cloud-sinks/{name}/test` (pings provider API with dry-run request).
- Edit form (inline) for min_confidence_to_trigger, require_hitl, auto_restore_after_minutes.

*Section 3 — Thresholds*:
- Sliders + number inputs for: `deviation_score_suspect_threshold` (default 5), `deviation_score_confirm_threshold` (default 10), `min_confidence_for_auto_trigger` (default 0.75), `corroboration_window_seconds` (default 60).
- "Dry Run Mode" toggle — prominently styled with amber warning when enabled.
- Save → `PATCH /api/ddos/config`.

*Section 4 — Baseline Review*:
- Table per device/interface with: current p50/p95/p99, deviation_score now, last_updated.
- Drift alerts shown inline (if `ddos_baseline_drift` detection exists for this device).
- "Acknowledge Drift" button per row.

**T5 — Real-time SSE integration**

Connect DdosDashboard to `GET /api/ddos/events/stream` SSE endpoint:

```javascript
// In DdosDashboard.svelte onMount:
const evtSource = new EventSource('/api/ddos/events/stream');
evtSource.onmessage = (e) => {
    const event = JSON.parse(e.data);
    // Dispatch to local store by event_type:
    // ddos_state_change → update event card state pill + timeline
    // ddos_mitigation_confirmed → update mitigation action confirmed badge
    // ddos_vector_update → update attack vectors tab
    // ddos_baseline_drift → show drift banner
    updateDdosStore(event);
};
```

- Svelte store `ddosStore.js`: `writeable` stores for `activeEvents`, `mitigationActions`, `routeEvents`.
- All UI components reactively derive from store — no polling loops.
- SSE reconnect logic: exponential backoff on connection loss (reuse pattern from Governance.svelte SSE implementation).

**T6 — Incident correlation: DDoS events in existing Incidents page**

Integrate DDoS events into the existing `Incidents.svelte` page:

- New event type filter: "DDoS" toggle alongside existing "Detection", "Remediation" filters.
- DDoS events rendered as a special card variant: attack badge with primary vector, affected prefix chips, confidence score, mitigation status pill.
- Click → opens DdosDashboard right-panel detail in a drawer overlay (reuse `DeviceDrawer.svelte` pattern).
- DDoS events included in ServiceNow incident creation (DS-4 cloud sink integration creates SNOW incidents with DDoS-specific fields: `ddos_event_id`, `affected_prefixes`, `mitigation_status`).

**T7 — Explorer pre-canned DDoS queries**

Extend `Explorer.svelte` pre-canned queries section with DDoS queries:

```
"Active DDoS events with mitigation status" →
MATCH (d:DdosEvent) WHERE d.state <> 'Idle' AND d.state <> 'Restored'
MATCH (d)-[:DDOS_HAS_MITIGATION]->(m:MitigationAction)
RETURN d.id, d.state, d.primary_vector, d.confidence, d.max_observed_gbps, m.action_type, m.confirmation_source

"Traffic baseline deviations > 5× across all devices" →
MATCH (b:TrafficBaseline) WHERE b.deviation_score > 5
MATCH (b)-[:BASELINE_FOR]->(i:Interface)
RETURN i.device_address, i.if_name, b.protocol, b.p95_pps, b.last_value_pps, b.deviation_score ORDER BY b.deviation_score DESC

"BGP RTBH events in last 24 hours" →
MATCH (r:DdosRouteEvent) WHERE r.event_type IN ['rtbh_community_received', 'prefix_restored']
AND r.occurred_at_ns > (timestamp() - 86400000000000)
RETURN r.prefix, r.event_type, r.bgp_community, r.occurred_at_ns ORDER BY r.occurred_at_ns DESC

"Devices acting as amplifiers in confirmed attacks" →
MATCH (d:DdosEvent)-[:DDOS_INVOLVES_DEVICE {role: 'amplifier'}]->(dev:Device)
RETURN dev.address, dev.hostname, count(d) as attack_count ORDER BY attack_count DESC

"Unconfirmed mitigations" →
MATCH (m:MitigationAction) WHERE m.confirmed_at_ns IS NULL
AND m.requested_at_ns < (timestamp() - 300000000000)
RETURN m.id, m.action_type, m.provider, m.target_prefix, m.api_http_status, m.requested_at_ns
```

**T8 — DDoS nav integration**

Wire `DdosDashboard` into `ui/src/App.svelte`:

- Add to NAV array under a new "Protect" section (after "Operate"):
  ```javascript
  { path: '/ddos', label: 'DDoS', icon: 'shield-alert', component: DdosDashboard }
  ```
- Nav group label: "Protect".
- Nav icon: `shield-alert` (Lucide icon, already used in icons.svg).
- Active attack badge on nav item: if `activeEvents.length > 0`, show red dot on nav icon.
- App.svelte `onMount`: poll `GET /api/ddos/events?state=active&limit=1` every 30s to update nav badge count (lightweight polling — SSE is per-page).


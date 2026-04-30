# campus_wlc — Campus wireless LAN controller

**Environment**: campus_wireless  
**Roles**: wlc, edge-wlc  
**Verification**: not-yet-verified

## OpenConfig Wireless Coverage Gap (as of 2026)

OpenConfig does not have ratified YANG models for wireless LAN controller state as of 2026. The OpenConfig working group has drafts for access-point and wireless-client models but no major WLC platform has shipped gNMI streaming for those paths yet.

**What this means for bonsai**:
- The `campus_wlc` profile is intentionally minimal — interface and system health via OC only.
- RF telemetry (RSSI, SNR, channel utilisation), client association counts, SSID-level stats, and AP join/leave events are NOT available via this profile.
- Vendor-native gNMI paths do exist on Cisco Catalyst Center / WLC 9800 and Aruba Central but require vendor plugin catalogues.

## Vendor Plugin Catalogue Guidance

For full wireless telemetry, create a plugin catalogue in `config/path_profiles/plugins/`:

```
plugins/
  cisco-wlc-9800/
    MANIFEST.yaml
    cisco_wlc_ap_state.yaml       # AP join state, channel, Tx power
    cisco_wlc_client_stats.yaml   # per-SSID client counts, association events
    cisco_wlc_rf_health.yaml      # channel utilisation, interference
```

Each profile uses `vendor_only: ["cisco_wlc"]` and references Cisco IOS-XE YANG models.

## What the Base Profile Monitors

- Management interface up/down (critical — loss of management path = loss of all AP control)
- System CPU/memory via `openconfig-system` (WLC control plane overload = AP association failures)
- LLDP when WLC advertises it on the uplink port

## When to Use This Profile

Use `campus_wlc` as the base profile for any WLC device. Pair with a vendor plugin catalogue for RF and client telemetry.

# Playbook Library Gap Analysis — D4-15 T5

*Generated for DV4 batch 18. Compares rule_ids from `python/bonsai_sdk/rules/` against `playbooks/library/`.*

## Coverage Matrix

| Rule ID | Source Module | Playbook Exists | File |
|---------|--------------|-----------------|------|
| `bfd_session_down` | `bfd.py` | ✅ | `bfd_session_down.yaml` |
| `bgp_session_down` | `bgp.py` | ✅ | `bgp_session_down.yaml` |
| `bgp_session_flap` | `bgp.py` | ✅ | `bgp_session_flap.yaml` |
| `bgp_all_peers_down` | `bgp.py` | ✅ | `bgp_all_peers_down.yaml` |
| `bgp_never_established` | `bgp.py` | ✅ | `bgp_never_established.yaml` |
| `interface_down` | `interface.py` | ✅ | `interface_down.yaml` |
| `interface_error_spike` | `interface.py` | ✅ | `interface_error_spike.yaml` |
| `interface_high_utilization` | `interface.py` | ✅ | `interface_high_utilization.yaml` |
| `topology_edge_lost` | `topology.py` | ✅ | `topology_edge_lost.yaml` |
| `frr_bgp_session_down` | *(variant)* | ✅ | `frr_bgp_session_down.yaml` |
| `route_flap_detected` | `streaming.py` | ❌ | — |
| `unexpected_as_path` | `streaming.py` | ❌ | — |
| `route_leak_detected` | `streaming.py` | ❌ | — |
| `sr_policy_degraded` | `streaming.py` | ❌ | — |
| `srlg_risk_detected` | `streaming.py` | ❌ | — |
| `snmp_cold_warm_start` | `snmp.py` | ❌ | — |
| `snmp_auth_failure_burst` | `snmp.py` | ❌ | — |
| `snmp_environmental_threshold_breach` | `snmp.py` | ❌ | — |
| `snmp_fru_failure` | `snmp.py` | ❌ | — |
| `syslog_auth_failure_cluster` | `syslog.py` | ❌ | — |
| `syslog_hardware_error` | `syslog.py` | ❌ | — |
| `syslog_software_crash` | `syslog.py` | ❌ | — |
| `syslog_license_expiry` | `syslog.py` | ❌ | — |
| `syslog_protocol_error` | `syslog.py` | ❌ | — |
| `syslog_bpduguard_activation` | `syslog.py` | ❌ | — |
| `syslog_stp_topology_change` | `syslog.py` | ❌ | — |
| `syslog_gnmi_disagreement` | `syslog.py` | ❌ | — |
| `orphan_interface_mention` | `syslog.py` | ❌ | — |
| `multi_source_correlation` | `syslog.py` | ❌ | — |
| `syslog_bfd_disagreement` | `syslog.py` | ❌ | — |
| `syslog_config_change_cluster` | `syslog.py` | ❌ | — |
| `syslog_hardware_interface_correlation` | `syslog.py` | ❌ | — |
| `config_changed` | `config.py` | ❌ | — |
| `config_caused_fault` | `config.py` | ❌ | — |
| `optical_rx_degrading` | `optical.py` | ❌ | — |
| `rack_isolated` | `rack.py` | ❌ | — |
| `host_network_fault` | `host.py` | ❌ | — |
| `service_path_degraded` | `app.py` | ❌ | — |

### Rust-side detection rules (no playbook applicable)
| Rule ID | Source | Notes |
|---------|--------|-------|
| `thermal_sensor_warning` | `graph/mod.rs` | Fired inline, no remediation playbook needed |
| `thermal_sensor_critical` | `graph/mod.rs` | Fired inline, no remediation playbook needed |

## Summary

- **Total Python rule IDs**: 38 (including `frr_bgp_session_down` variant)
- **Playbooks exist**: 10
- **Missing playbooks**: 28
- **Coverage**: 26.3%

## Priority Gaps (recommended next playbooks)

### High Priority — automated remediation candidates
1. **`route_flap_detected`** — dampening or prefix filtering
2. **`route_leak_detected`** — route-policy correction
3. **`config_caused_fault`** — config rollback
4. **`snmp_cold_warm_start`** — verify post-restart health
5. **`syslog_hardware_error`** — hardware diagnostics + NOC alert

### Medium Priority — diagnostic/investigation playbooks
6. **`syslog_auth_failure_cluster`** — security investigation
7. **`syslog_software_crash`** — core dump collection + service restart
8. **`optical_rx_degrading`** — optics cleaning / replacement SOP
9. **`snmp_environmental_threshold_breach`** — HVAC / facility alert
10. **`rack_isolated`** — physical layer investigation

### Lower Priority — informational or already covered by correlation
11. `multi_source_correlation` — informational, no action needed
12. `config_changed` — informational, no action needed
13. `orphan_interface_mention` — naming consistency audit
14. `syslog_gnmi_disagreement` — telemetry gap investigation
15. `service_path_degraded` — application-layer triage

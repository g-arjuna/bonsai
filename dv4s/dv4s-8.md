## DS-8 — DDoS Simulation + Testing Harness

### Analysis

End-to-end DDoS detection testing in a lab environment without actual attack traffic requires signal injection at the telemetry layer — not actual packet floods. The existing `tests/chaos_harness/` and `tests/event_driver/` frameworks provide a pattern for injecting signals via the gRPC API. However, DDoS testing requires injecting:

1. **High-rate metric anomalies** via synthetic gNMI telemetry (pps spike at 50× baseline).
2. **SYN-flood NetFlow records** with `tcp_flags=SYN_ONLY` and many source IPs.
3. **sFlow samples** with UDP dst_port=53 (DNS amplification simulation).
4. **syslog messages** matching CoPP violation and ACL deny flood patterns.
5. **SNMP traps** with ifInDiscards spike.
6. **BMP ROUTE_MONITORING** with RTBH community (simulating upstream response).
7. **Controlled multi-wave attacks**: single-vector → multi-vector → campaign → mitigation → restoration.

The testing harness also needs to validate the **time-to-detect** (TTD) and **time-to-mitigate** (TTM) metrics which are core to the DDoS supplement's value proposition.

### Tasks

**T1 — DDoS signal injection driver**

New file `tests/ddos_harness/inject.py`:

```python
class DdosSignalInjector:
    """
    Injects synthetic DDoS signals into Bonsai via its telemetry ingestion APIs.
    All injections are tagged with injection_id for traceability.
    """
    
    def inject_pps_spike(self, device_address, if_name, spike_multiplier=50, duration_s=60):
        """Inject gNMI-style interface PPS spike via Bonsai telemetry ingest."""
        # POST to /api/telemetry/inject (new endpoint, see T4)
    
    def inject_syn_flood_flows(self, target_prefix, src_count=1000, pps=50000):
        """Inject synthetic NetFlow records with SYN_ONLY flags from diverse sources."""
    
    def inject_dns_amplification_flows(self, target_prefix, amplification_factor=30):
        """Inject sFlow samples with udp_dst_port=53 at above-baseline rate."""
    
    def inject_copp_violation_syslog(self, device_address, class_name, drop_pps=5000):
        """Inject syslog message matching CoPP violation pattern."""
    
    def inject_rtbh_bmp_event(self, prefix, community="65535:666", from_session_id=None):
        """Inject BMP ROUTE_MONITORING UPDATE with RTBH community for prefix."""
    
    def inject_if_discard_snmp_trap(self, device_address, if_index, discard_count=100000):
        """Inject SNMP ifInDiscards trap."""
    
    def simulate_ddos_scenario(self, scenario: DdosScenario):
        """Run a full scenario: sequence of injections with timing."""
```

**T2 — DDoS test scenarios YAML**

New file `tests/ddos_harness/scenarios/`:

- `syn_flood_single_device.yaml`:
  ```yaml
  name: "SYN flood single device — expect ddos_confirmed"
  steps:
    - at_ms: 0
      action: inject_pps_spike
      device: "172.100.109.16"
      if_name: "ethernet-1/1"
      multiplier: 50
    - at_ms: 5000
      action: inject_syn_flood_flows
      target_prefix: "198.51.100.0/24"
      src_count: 500
      pps: 80000
    - at_ms: 8000
      action: inject_copp_violation_syslog
      device: "172.100.109.16"
      drop_pps: 3000
  expect:
    ddos_suspect_within_ms: 10000
    ddos_confirmed_within_ms: 15000
    ddos_confirmed_confidence_min: 0.6
  ```

- `dns_amplification_campaign.yaml` — multi-device DNS amplification from 3 reflectors.
- `ntp_amplification.yaml` — NTP monlist amplification pattern.
- `bgp_hijack_with_rtbh.yaml` — unexpected origin ASN → RTBH response → BMP confirmation.
- `multi_vector_campaign.yaml` — SYN flood + DNS amp + ICMP flood simultaneously from different source ASNs.
- `false_positive_bulk_download.yaml` — high PPS but legitimate: single source IP, TCP ACK-heavy traffic pattern, no SYN-only, no CoPP violations. **Must NOT fire ddos_confirmed.**

**T3 — DDoS harness runner**

New file `tests/ddos_harness/run.py`:

```python
def run_scenario(scenario_file: str, bonsai_url: str, dry_run: bool = False):
    """
    1. Load scenario YAML.
    2. Record baseline metrics.
    3. Execute injection steps with timing.
    4. Wait for expected detections.
    5. Measure TTD (time-to-detect) and TTM (time-to-mitigate if applicable).
    6. Write results to runtime/driver_results/ddos_{scenario_name}.json.
    """

def run_all_scenarios(bonsai_url: str):
    """Run all scenario files + generate summary report."""

def measure_ttd(expected_rule_id: str, injection_start_ns: int, timeout_s: int = 60) -> float:
    """Poll /api/detections until expected rule_id fires, return elapsed seconds."""
```

Outputs:
- Per-scenario JSON with: scenario_name, TTD_seconds, TTM_seconds, detections_fired (list), false_positive_count, confidence_scores.
- Summary markdown to `docs/DDOS_TEST_REPORT.md`.

**T4 — Telemetry injection API endpoint**

New endpoint `POST /api/telemetry/inject` in `src/http_server/observability.rs`:

- **Lab/test use only**: gated on `[lab] enabled = true` in config AND Operator+ role.
- Accepts JSON matching the existing `TelemetryUpdate` structure (already defined in `src/telemetry.rs`).
- Puts the synthetic update directly onto the internal event bus (bypassing the actual gNMI/syslog/NetFlow receivers).
- Full DDoS-relevant update types accepted: `InterfaceCounters`, `ProtocolPacketRate`, `CoppViolation`, `LptsDropStats`, `ForwardingResourcePressure`, `SflowRecord`, `NetflowRecord`, `SyslogFact`, `SnmpFact`.
- Security: injection endpoint is completely disabled in production mode (`[lab] enabled = false`).
- Injection events are tagged with `source_type="synthetic_injection"` and `injection_id` in graph writes for traceability.

**T5 — Time-to-react metrics**

Instrument the detection pipeline to record TTD/TTM:

New properties on `DdosEvent`:
- `injection_id: String` — links to the test injection that triggered this (nullable in production).
- `ttd_ms: Int64` — time from first telemetry anomaly to `ddos_suspect` detection fire.
- `ttc_ms: Int64` — time from first signal to `ddos_confirmed` fire (time-to-confirm).
- `ttm_ms: Int64` — time from `ddos_confirmed` to `MitigationAction.requested_at_ns` (time-to-mitigate).
- `ttv_ms: Int64` — time from mitigation request to BMP confirmation (time-to-verify).

API: `GET /api/ddos/metrics/timing` — aggregated TTD/TTC/TTM/TTV statistics (p50, p95, p99) across all completed DdosEvent nodes. Used for SLA reporting and benchmark tracking.

**T6 — False positive validation suite**

Critical anti-regression tests for false positive scenarios:

New file `tests/ddos_harness/scenarios/false_positive_suite.yaml` covering:

- `bulk_download.yaml`: Single source IP, large TCP flows, ACK-dominant, no CoPP violations → must NOT fire `ddos_confirmed`.
- `bgp_reconvergence_traffic.yaml`: Traffic spike during BGP reconvergence (high pps but short-lived, correlates with `bgp_neighbor_down` detection) → classify as `bgp_reconvergence` not `ddos_suspect`.
- `backup_window.yaml`: Scheduled nightly backup traffic (UDP dst_port=445/SMB or TCP dst_port=22/SSH, consistent source/dest, no port diversity) → must NOT fire.
- `videoconf_burst.yaml`: Sudden UDP video conferencing burst (SSRC-consistent RTP, dst_port=3478 STUN, single source ASN) → must NOT fire.
- `night_batch_job.yaml`: Periodic high-volume TCP connection from single server at 2am (matches baseline weekday pattern at different time window) — demonstrates time-of-day baseline awareness.

Each false positive test PASSES only if no `ddos_suspect` or higher fires within 120s of injection.

**T7 — Ubuntu testing guide integration**

Add DDoS testing phases to the Ubuntu testing guide (`docs/UBUNTU_TESTING_GUIDE.md`):

```
Phase 24: DDoS Signal Extraction Validation
  S-70: Verify TrafficBaseline nodes created for all interfaces
  S-71: Verify gNMI DDoS path profile subscribed on test device
  S-72: Verify syslog DDoS patterns match test messages

Phase 25: DDoS Detection Validation
  S-73: Run syn_flood_single_device scenario — verify ddos_suspect fires < 10s
  S-74: Run syn_flood_single_device — verify ddos_confirmed fires < 15s
  S-75: Run dns_amplification_campaign — verify ddos_campaign fires < 30s
  S-76: Run false_positive_bulk_download — verify NO ddos_confirmed fires

Phase 26: DDoS Response Validation (dry-run only in lab)
  S-77: Verify MitigationAction node created when ddos_confirmed fires
  S-78: Verify API call logged (dry-run) to configured cloud sink
  S-79: Inject rtbh_bmp_event — verify BMP assurance confirms within 30s
  S-80: Verify DdosEvent transitions to Mitigated state after confirmation

Phase 27: BMP Post-Incident Assurance
  S-81: Inject prefix_restored BMP event — verify DdosEvent transitions to Restored
  S-82: Verify BGP audit report generated with correct convergence time
  S-83: Verify MitigationAction.revert_confirmed_at_ns populated
```


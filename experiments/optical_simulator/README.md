# Optical Simulator (D2-7 T4)

Synthetic source that emits fake optical telemetry conforming to the OpenConfig
`openconfig-platform-optical-channel` schema. Used to validate the D2-7 data
model and `optical_rx_degrading` detection rule before real optical hardware is
available in the lab.

## Usage

```bash
# Prerequisites: pip install grpcio requests (or the bonsai dev venv)
python3 experiments/optical_simulator/simulate.py --bonsai-url http://127.0.0.1:3000

# Inject a degradation scenario on channel 1:
python3 experiments/optical_simulator/simulate.py --scenario degrade --channel ch-1

# List available scenarios:
python3 experiments/optical_simulator/simulate.py --list-scenarios
```

## Scenarios

| Scenario | Description |
|---|---|
| `steady` | All channels healthy, rx_power held at −3 dBm |
| `degrade` | Channel rx_power drops 0.5 dBm per tick over 20 ticks |
| `hard_fail` | Channel rx_power drops to −40 dBm in one step |
| `flap` | Channel rx_power oscillates ±4 dBm every 30s |
| `multi_degrade` | All channels degrade simultaneously (rack-level fibre event) |

## Output format

Each tick emits a JSON payload to stdout (and optionally POSTs to the bonsai
SSE injection endpoint when `--bonsai-url` is set):

```json
{
  "device_address": "optical-line-01.dc1",
  "event_type": "optical_channel_state",
  "occurred_at_ns": 1716000000000000000,
  "channels": [
    {
      "name": "ch-1",
      "rx_power_dbm": -4.2,
      "tx_power_dbm": 2.1,
      "osnr_db": 28.5,
      "pre_fec_ber": 1.2e-4,
      "laser_bias_ma": 52.3,
      "temperature_c": 44.1
    }
  ]
}
```

## Validation checklist (Ubuntu)

1. Start simulator: `python3 experiments/optical_simulator/simulate.py --scenario degrade`
2. Observe `optical_channel_state` events in the Live feed.
3. After 20 ticks (~20s), confirm `optical_rx_degrading` detection fires.
4. Query graph: `MATCH (oc:OpticalChannel) RETURN oc.name, oc.rx_power_dbm ORDER BY oc.rx_power_dbm`

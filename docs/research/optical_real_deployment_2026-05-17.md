# Optical Real-Deployment Integration Scoping

> D2-7 T5 — 2026-05-17. Documents vendor coverage, telemetry methods, and
> calibration patterns for real optical hardware integration. The D2-7 data
> model and synthetic simulator are complete; this doc governs what's needed
> to go from simulator to live hardware.

## Vendors observed in the field

| Vendor | Platform | Telemetry method | OpenConfig coverage | Notes |
|---|---|---|---|---|
| Ciena | 6500, Waveserver | gNMI (OpenConfig) + NETCONF | `openconfig-platform-optical-channel` v1.0+ | Ciena ships OC optical-channel since 6500 OS 11.0. Power values in dBm × 100 (integer) — divide by 100. |
| Cisco | NCS 1000/2000, NCS 5500 (DWDM line card) | gNMI (native Cisco-IOS-XR-controller-optics-oper) + OpenConfig | Partial — `oc-platform` optical-channel present but `pre_fec_ber` is in a Cisco-native leaf | NCS 5500 DWDM line cards require `Cisco-IOS-XR-controller-optics-oper` for BER. OC model covers power + OSNR. |
| Nokia | 1830 PSS, 7750 with coherent | gNMI (SR Linux style) + SNMP (ENTITY-SENSOR-MIB) | Partial — Nokia 1830 has no OpenConfig support; uses SNMP + NETCONF | Nokia FP4-based 7750 SR line cards do support OC on newer SR OS. |
| Juniper | PTX10000, ACX | gNMI (OpenConfig) | Good — `openconfig-platform-optical-channel` supported on PTX JunOS 19.2+ | OSNR available only on PTX; ACX omits OSNR. |
| Arista | 400G-ZR+ line cards (7800R3A) | gNMI (native EOS path) | Partial — EOS 4.26+ has `Arista-EOS-optical` YANG; OC optical-channel arrives in 4.28+ | |
| FRR / white-box | Open ROADM MSA transceivers | SNMP (ENTITY-SENSOR-MIB) | None — white-box uses vendor transceiver SNMP only | Open ROADM API is REST, not gNMI. Separate integration needed. |

## gNMI path matrix

### OpenConfig (preferred)

```
openconfig-platform:components/component[name=...]/optical-channel/state/
  input-power/instant        # rx_power_dbm (dBm × 100 on some vendors)
  output-power/instant       # tx_power_dbm
  laser-bias-current/instant # laser_bias_ma
  chromatic-dispersion/instant
  polarization-mode-dispersion/instant
  second-order-polarization-mode-dispersion/instant
  osnr                       # available on PTX, NCS 1000, Ciena 6500
  esnr                       # available on Ciena, NCS 2000
  pre-fec-ber/instant        # pre_fec_ber (field name varies)
  post-fec-ber/instant
```

### Cisco-IOS-XR native (fallback for NCS BER)

```
Cisco-IOS-XR-controller-optics-oper:optics-oper/optics-ports/optics-port/
  optics-info/transceiver-info/pre-fec-bit-error-rate
  optics-info/transceiver-info/osnr
```

### ENTITY-SENSOR-MIB (legacy / Nokia 1830 / white-box)

OIDs:
- `entPhySensorValue` (1.3.6.1.2.1.99.1.1.1.4) — raw sensor reading
- `entPhySensorType` (1.3.6.1.2.1.99.1.1.1.1) — type 8 = dBm × 100

## Calibration patterns

### Per-vendor power scaling

| Vendor | rx_power unit in gNMI | Bonsai normalisation |
|---|---|---|
| Ciena 6500 | integer dBm × 100 | divide by 100.0 |
| Cisco NCS (OC) | float dBm | use as-is |
| Nokia SR OS (OC) | float dBm | use as-is |
| SNMP ENTITY-SENSOR | integer × `entPhySensorPrecision` | apply precision shift |

### Per-span baseline rx_power

Different span lengths produce different nominal rx_power. A degradation alarm
using a fixed −12 dBm floor is wrong for a −5 dBm nominal channel (3 dBm
headroom). The correct approach is a **per-channel baseline**: record the 30-day
P99 of rx_power for each channel and alarm when it drops >3 dBm below that
baseline.

**DV3 upgrade path**: add a `baseline_rx_dbm` attribute to `OpticalChannel` nodes.
Update the gNMI collector to compute a rolling 30-day max and store it. Update
`optical_rx_degrading` to use `baseline_rx_dbm - 3.0` as the threshold instead of
the fixed −12 dBm floor.

## Calibration for OSNR

OSNR depends on the number of amplifier spans:
- 10-span system: nominal ~30 dB
- 30-span system: nominal ~18 dB

Use relative drops (>3 dB drop from baseline) rather than absolute thresholds
for OSNR anomaly detection.

## Integration plan (DV3)

1. Enable `openconfig-platform-optical-channel` paths in the relevant path profiles.
2. Add Cisco-IOS-XR native fallback paths to `sp_pe_full.yaml` for NCS BER.
3. Add SNMP Nokia 1830 poller (separate from gNMI subscriber — Nokia 1830 is
   SNMP-only).
4. Implement per-channel baseline calibration (30-day rolling max in graph).
5. Add `optical_rx_degrading` escalation: severity `warn` at baseline −3 dBm,
   severity `critical` at baseline −6 dBm.

## Lab status (DV2)

No real optical hardware in the lab. D2-7 T4 synthetic simulator provides
`optical_channel_state` events for rule validation. Real hardware integration
is DV3+ when SP lab receives DWDM test equipment.

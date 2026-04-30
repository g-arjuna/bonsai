# dc_leaf_minimal — DC leaf baseline

**Environment**: data_center, home_lab  
**Roles**: leaf, access  
**Verification**: lab-verified (Nokia SR Linux, Cisco IOS-XRd)

## YANG Models Required

| Path | Model | Notes |
|------|-------|-------|
| `interfaces` | `openconfig-interfaces` | All vendors |
| `network-instances` | `openconfig-bgp`, `openconfig-network-instance` | All vendors |
| `lldp` | `openconfig-lldp` | All vendors |
| `bfd` | `openconfig-bfd` | Optional; when advertised |
| `interface[name=*]/statistics` | `srl_nokia` | Nokia SRL only |
| `network-instance[name=default]/protocols/bgp/...` | `srl_nokia` | Nokia SRL only |
| `Cisco-IOS-XR-infra-statsd-oper:...` | `Cisco-IOS-XR-infra-statsd-oper` | IOS-XR only |
| `Cisco-IOS-XR-ethernet-lldp-oper:...` | `Cisco-IOS-XR-ethernet-lldp-oper` | IOS-XR only, optional |

## Sample Telemetry Shape (Nokia SRL)

```json
{
  "path": "interface[name=ethernet-1/1]/oper-state",
  "val": {"oper-state": "up"}
}
{
  "path": "network-instance[name=default]/protocols/bgp/neighbor[peer-address=192.0.2.1]",
  "val": {"session-state": "ESTABLISHED", "peer-as": 65001}
}
```

## Known Device Behaviours

- **Nokia SR Linux**: `srl_nokia` model is advertised as the first capability; native paths return before OC paths resolve.
- **Cisco IOS-XRd**: `openconfig-lldp` is not always advertised even when LLDP is running; native LLDP path is the reliable fallback.
- **FRR / Holo**: `openconfig-interfaces` is advertised; BGP support depends on FRR version and OC plugin.
- **Arista cEOS**: All OC paths work; no native vendor paths needed.
- **Juniper cRPD**: OC paths generally work; `openconfig-bfd` may not be advertised in all versions.

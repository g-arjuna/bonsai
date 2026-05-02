# homelab_switch — Home-lab switch: OpenConfig interfaces, LLDP, and VLAN — all optional for maximum compatibility with lab images.

**Environment**: home lab  
**Roles**: switch, leaf, access, spine  
**Vendor scope**: all vendors (OpenConfig + per-vendor natives)  
**Verification**: not-yet-verified

## Rationale

Lab switches (Arista cEOS, Nokia SR Linux in L2 mode, generic VMs) have inconsistent gNMI model coverage. All paths are marked optional; discovery selects what the device actually advertises.

## Subscribed Paths

| Path | Origin | Mode | Interval | Models | Vendors | Optional |
|------|--------|------|----------|--------|---------|----------|
| `interfaces` | openconfig | SAMPLE | 30s | `openconfig-interfaces` | all vendors | no |
| `interfaces` | openconfig | ON_CHANGE | — | `openconfig-interfaces` | all vendors | no |
| `lldp` | openconfig | ON_CHANGE | — | `openconfig-lldp` | all vendors | yes |
| `vlans` | openconfig | ON_CHANGE | — | `openconfig-vlan` | all vendors | yes |
| `stp` | openconfig | ON_CHANGE | — | `openconfig-spanning-tree` | all vendors | yes |
| `interface[name=*]/statistics` | native | SAMPLE | 30s | any of: `srl_nokia` | nokia_srl | yes |

## YANG Models Required

| Model | Vendor scope |
|-------|-------------|
| `openconfig-interfaces` | all vendors |
| `openconfig-lldp` | all vendors |
| `openconfig-spanning-tree` | all vendors |
| `openconfig-vlan` | all vendors |
| `srl_nokia` | nokia_srl (any-of) |

## Path Rationales

- **`interfaces`** [openconfig] — OpenConfig interface counters.
- **`interfaces`** [openconfig] — OpenConfig interface oper-state.
- **`lldp`** [openconfig] — OpenConfig LLDP for lab topology discovery.
- **`vlans`** [openconfig] — OpenConfig VLAN when the device advertises the VLAN model.
- **`stp`** [openconfig] — OpenConfig spanning-tree when available.
- **`interface[name=*]/statistics`** [native] — SR Linux native interface counters for cEOS/SRL lab images.

## Known Gaps

<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->

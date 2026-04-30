# Path Profile Companion Notes

Each file documents one profile in `config/path_profiles/`. Notes cover: which YANG models are required, known device behaviour, lab verification status, and vendor gaps.

## Profile Index

| Profile | Environment | Roles | Status |
|---------|------------|-------|--------|
| [dc_leaf_minimal](dc_leaf_minimal.md) | data_center, home_lab | leaf, access | lab-verified |
| [dc_spine_standard](dc_spine_standard.md) | data_center, home_lab | spine, superspine, distribution, border | lab-verified |
| [dc_superspine_standard](dc_superspine_standard.md) | data_center | superspine, spine, distribution, border | not-yet-verified |
| [dc_border_standard](dc_border_standard.md) | data_center | border, edge, border-leaf | not-yet-verified |
| [dc_evpn_leaf](dc_evpn_leaf.md) | data_center | leaf, access, border-leaf | not-yet-verified |
| [sp_p_core](sp_p_core.md) | service_provider | p, core, edge | lab-verified |
| [sp_p_sr_te](sp_p_sr_te.md) | service_provider | p, core, edge | not-yet-verified |
| [sp_pe_full](sp_pe_full.md) | service_provider | pe, rr, peering | lab-verified |
| [sp_pe_l3vpn](sp_pe_l3vpn.md) | service_provider | pe, peering | not-yet-verified |
| [sp_pe_evpn](sp_pe_evpn.md) | service_provider | pe, peering | not-yet-verified |
| [sp_rr_basic](sp_rr_basic.md) | service_provider | rr, route-reflector | not-yet-verified |
| [sp_peering_edge](sp_peering_edge.md) | service_provider | peering, edge, ce-facing | not-yet-verified |
| [campus_access](campus_access.md) | campus_wired | access, edge | not-yet-verified |
| [campus_distribution](campus_distribution.md) | campus_wired | distribution, core, border | not-yet-verified |
| [campus_core](campus_core.md) | campus_wired | core, distribution, border | not-yet-verified |
| [campus_wlc](campus_wlc.md) | campus_wireless | wlc, edge-wlc | not-yet-verified |
| [homelab_router](homelab_router.md) | home_lab | router, pe, p, rr, leaf, spine, edge | lab-verified |
| [homelab_switch](homelab_switch.md) | home_lab | switch, leaf, access, spine | lab-verified |

## Verification Status

- **lab-verified**: profile was applied to a live ContainerLab device and telemetry was observed on all non-optional paths.
- **not-yet-verified**: profile is structurally complete; lab run pending real vendor image availability.

## Adding a Profile

1. Create `config/path_profiles/<name>.yaml` using the v2 schema (requires `environment`, `vendor_scope`, `roles`).
2. Create `docs/path_profiles/<name>.md` with YANG model list, sample telemetry shape, and known behaviours.
3. Run `cargo test --release -- catalogue` to verify it loads cleanly.
4. Run against a live device and update status to `lab-verified`.

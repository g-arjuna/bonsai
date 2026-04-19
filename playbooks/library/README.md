# Playbook Library

This directory contains the harvested remediation playbooks that map Bonsai
detection rules to vendor-specific actions or explicit manual-investigation
policies.

## Current files

- `bgp_session_down.yaml`
- `bgp_session_flap.yaml`
- `bfd_session_down.yaml`
- `bgp_all_peers_down.yaml`
- `bgp_never_established.yaml`
- `interface_down.yaml`
- `interface_error_spike.yaml`
- `interface_high_utilization.yaml`
- `topology_edge_lost.yaml`

## What is executable today

- `nokia_srl` `bgp_session_down`
  - neighbor `admin-state` disable/enable bounce
- `nokia_srl` `topology_edge_lost`
  - interface-scoped LLDP `admin-state` disable/enable bounce

## What is intentionally manual-only today

- All flap, all-peers-down, and never-established BGP detections
- All BFD detections
- All interface detections
- Cisco XRd and Arista cEOS `bgp_session_down`
- Cisco XRd and Arista cEOS `topology_edge_lost`

These are stored as first-class artifacts because "no automatic remediation"
is still a valid and important catalog decision.

## Next harvest targets

1. Add a VRF-aware SR Linux variant for `bgp_session_down` once network-instance
   context is present in detection features.
2. Validate Cisco XRd and Arista cEOS per-neighbor BGP reset/admin-state paths
   from vendor YANG plus lab testing.
3. Extend the graph schema with interface admin/oper status so interface
   recovery can be verified cleanly.
4. Revisit interface admin-state playbooks on Cisco/Arista through OpenConfig
   `interfaces/interface/config/enabled` after graph verification improves.

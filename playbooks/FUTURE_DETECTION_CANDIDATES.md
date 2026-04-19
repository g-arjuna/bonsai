# Future Detection Candidates

These are the next detection rules worth adding after the current Phase 4
catalog, chosen from a day-2 operations perspective for DC/SP topologies.

They are listed here to guide future implementation. They are **not** yet live
rules in the current Bonsai engine unless added separately in Python/Rust.

## Tier 1: highest operational value

### `lldp_neighbor_lost`

- Why it matters:
  Strong signal of physical/topology change in leaf-spine and routed fabrics.
- Candidate features:
  `device_address`, `if_name`, `event_type`, `occurred_at_ns`, plus future
  `neighbor_id` / `system_name`.
- Likely graph verification:
  `MATCH (d:Device {address: $device_address})-[:HAS_LLDP_NEIGHBOR]->(n:LldpNeighbor {local_if: $if_name}) RETURN count(n) > 0`
- Candidate remediation posture:
  Human-first at first; possible future LLDP interface admin-state re-enable on
  vendors with explicit per-interface LLDP controls.
- Sources:
  - [SR Linux LLDP guide](https://documentation.nokia.com/srlinux/26-3/books/interfaces/lldp.html)

### `ospf_adjacency_down`

- Why it matters:
  Common routed-fabric and SP underlay failure mode.
- Candidate features:
  `device_address`, `if_name`, future `area`, `neighbor_id`, `old_state`,
  `new_state`.
- Candidate remediation posture:
  Human-first initially; later possibly safe single-interface or protocol
  interface re-enable if the graph can verify adjacency return.
- Sources:
  - [SR Linux OSPF guide](https://documentation.nokia.com/srlinux/22-6/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-ospf.html)
  - [OSPF harvest note](C:\Users\arjun\Desktop\bonsai\playbooks\sources\ospf_adjacency_down.md)

### `isis_adjacency_down`

- Why it matters:
  Core SP/routed-underlay signal with strong operational value.
- Candidate features:
  `device_address`, `if_name`, future `level`, `neighbor_system_id`,
  `old_state`, `new_state`.
- Candidate remediation posture:
  Human-first initially; later possibly bounded per-interface family admin-state
  or BFD-linked actions.
- Sources:
  - [SR Linux IS-IS guide](https://documentation.nokia.com/srlinux/22-3/SR_Linux_Book_Files/Configuration_Basics_Guide/configb-is-is.html)
  - [IS-IS harvest note](C:\Users\arjun\Desktop\bonsai\playbooks\sources\isis_adjacency_down.md)

## Tier 2: useful after graph/schema growth

### `interface_admin_down_unexpected`

- Why it matters:
  Separates true admin shutdown from physical loss, which is critical for safe
  interface auto-remediation.
- Why not now:
  Current graph does not store interface admin-state directly.

### `control_plane_source_interface_lost`

- Why it matters:
  Explains multi-peer routing loss caused by loopback/system interface issues.
- Why not now:
  Requires stronger model of loopback/system interfaces and protocol sourcing.

### `protocol_bfd_mismatch`

- Why it matters:
  Day-2 troubleshooting often lands on "protocol is flapping because BFD policy
  is inconsistent".
- Why not now:
  Requires intent-level config correlation not yet represented.

## Tier 3: later SP-specific growth

### `mpls_lsp_down`

- Why it matters:
  Service-provider transport health and fast reroute validation.
- Why later:
  Better after IGP/BFD adjacency modeling is mature.

### `sr_policy_inactive`

- Why it matters:
  Important for SR-TE operational assurance.
- Why later:
  Needs richer policy and path-state graph support.

## Design note

Every future detection added here should be accompanied by:

1. a clear graph-verifiable success condition,
2. a bounded vendor/YANG harvest session,
3. and an explicit reason why the first playbook is executable or manual-only.

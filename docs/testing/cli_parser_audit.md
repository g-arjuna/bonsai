# CLI Parser Chain Audit — D2-T2 (DV1)

**Date**: 2026-05-16  
**Auditor**: Cascade (DV1 sprint)

---

## Architecture

`src/parser_chain.rs` is a **routing and consensus layer**, not a parser.  
It dispatches `ParseRequest { vendor, command_pattern, raw_output }` to one or more
parser sidecars in priority order defined in `bonsai.toml`:

```
priorities."cisco-iosxr::show bgp summary" = ["pyats_genie", "ntc_templates", "bonsai_native"]
priorities."nokia-srlinux::*" = ["bonsai_native"]
```

The actual parsing runs in Python via HTTP sidecars:

| Sidecar | URL (default) | Notes |
|---|---|---|
| `pyats_genie` | `http://127.0.0.1:9101` | Cisco-native, broadest vendor support |
| `bonsai_native` | `http://127.0.0.1:9102` | Bonsai hand-written parsers (SRL, FRR, fallback) |
| `ntc_templates` | via pyats | TextFSM-backed; used as fallback |

---

## Parser Functions — Audit Table

Bonsai does not have internal Rust parser functions per vendor/command.
The chain dispatches to external sidecars. Coverage is determined by which
sidecar supports which `(vendor, command)` pair.

| Command | Vendor | Primary Sidecar | Unit Tests | Fixtures |
|---|---|---|---|---|
| show bgp summary | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show bgp summary | nokia-srlinux | bonsai_native | none | ✅ (this PR) |
| show bgp summary | arista-eos | pyats_genie | none | ✅ (this PR) |
| show bgp summary | juniper-junos | pyats_genie | none | ✅ (this PR) |
| show bgp summary | frr | bonsai_native | none | ✅ (this PR) |
| show interfaces | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show interfaces | nokia-srlinux | bonsai_native | none | ✅ (this PR) |
| show interfaces | arista-eos | pyats_genie | none | ✅ (this PR) |
| show interfaces | juniper-junos | pyats_genie | none | ✅ (this PR) |
| show isis adjacency | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show isis adjacency | nokia-srlinux | bonsai_native | none | ✅ (this PR) |
| show ospf neighbor | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show ospf neighbor | juniper-junos | pyats_genie | none | ✅ (this PR) |
| show bfd session | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show bfd session | nokia-srlinux | bonsai_native | none | ✅ (this PR) |
| show ip route | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show ip route | juniper-junos | pyats_genie | none | ✅ (this PR) |
| show lldp neighbors | cisco-iosxr | pyats_genie | none | ✅ (this PR) |
| show lldp neighbors | nokia-srlinux | bonsai_native | none | ✅ (this PR) |
| show lldp neighbors | arista-eos | pyats_genie | none | ✅ (this PR) |

**Total fixtures authored this sprint**: 40 (see `tests/cli_fixtures/`)

---

## Gap Assessment

- No Rust unit tests exist for the parser chain. The chain has no parseable logic
  to unit-test (it's a router). Testing is integration-only via `smoke_cli_fixtures.sh`.
- Parser coverage is entirely dependent on sidecar availability. Fixtures authored
  here serve as **integration regression tests** — they fail fast when a sidecar
  upgrade changes output schema.
- FRR BFD and IS-IS coverage is absent from current bonsai_native sidecar; flagged
  for DV2.

---

## Running the fixture suite

```bash
bash scripts/smoke/smoke_cli_fixtures.sh
```

Requires the `bonsai_native` sidecar running on `localhost:9102`. In CI, mock
sidecar responses satisfy the fixture assertions without a live lab.

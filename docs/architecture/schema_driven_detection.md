# Schema-Driven Detection — D2-2 Architecture

> Authored DV2, 2026-05-17. Supersedes the hand-coded state-string approach
> identified as a maintenance liability in DV1 (F-11 diagnostic).

## Problem

Prior to DV2, detection rules embedded vendor state strings directly:

```python
_DOWN_STATES = {"down", "admin_down"}  # admin_down: SR Linux BFD admin-disable
```

This pattern has three failure modes:

1. **Silent miss on new vendor**: A new vendor emits `"adminDown"` (camel-case); the rule doesn't fire.
2. **Diverging constants**: Each of `bfd.py`, `interface.py`, `bgp.py` maintains its own copy of these sets — they drift.
3. **No test coverage at the string level**: The F-11 bug (BFD `admin_down` not firing) was not caught by any existing test because there were no per-vendor state fixtures.

## Solution

A two-layer architecture:

```
config/vendor_state_mapping/<vendor>.yaml   ← YANG path + semantic enum + raw strings
        │
        ▼
python/bonsai_sdk/state_mapping.py          ← loads YAML at import, provides is_down/is_up
        │
        ▼
python/bonsai_sdk/rules/{bfd,interface,bgp}.py   ← call is_down/is_up, no raw strings
```

### Layer 1 — Vendor YAML Registry

Each file in `config/vendor_state_mapping/` defines one vendor:

```yaml
vendor: nokia_srl
state_mappings:
  bfd_oper_state:
    yang_path: "openconfig-bfd:bfd/sessions/session/state/oper-state"
    semantic_states:
      UP:         ["up"]
      DOWN:       ["down"]
      ADMIN_DOWN: ["admin_down"]
    treat_as_down: ["DOWN", "ADMIN_DOWN"]
    treat_as_up:   ["UP"]
```

**Semantic states** are vendor-independent labels (`UP`, `DOWN`, `ADMIN_DOWN`).  
**Raw strings** are the exact bytes emitted by the vendor's gNMI subscription.  
**treat_as_down / treat_as_up** control which semantics trigger detection.

### Layer 2 — Python Adapter (`state_mapping.py`)

```python
from bonsai_sdk.state_mapping import is_down, is_up

vendor = client.device_vendor(f.device_address)   # cached graph query
if is_down(vendor, "bfd_oper_state", f.new_state):
    ...
```

- Loaded once at import. Reload with `state_mapping.reload()` if YAMLs change at runtime.
- Vendor aliases normalised: `"nokia"`, `"nokia_srlinux"`, `"nokia_srl"` → `"nokia_srl"`.
- Unknown vendor or unknown leaf → returns `False` (safe default, no false positives).

### Layer 3 — State Transition Fixtures

`tests/state_transition_fixtures/<vendor>/<rule_id>.yaml` contain per-fixture
expected-fire assertions. Run with:

```bash
python tests/state_transition_fixtures/run_fixtures.py
```

No bonsai process or lab required. Mac-safe.

## Adding a New Vendor

1. Create `config/vendor_state_mapping/<new_vendor>.yaml`.
2. Add aliases to `_VENDOR_ALIASES` in `state_mapping.py` if the vendor string
   coming from the graph differs from the YAML `vendor:` key.
3. Create `tests/state_transition_fixtures/<new_vendor>/` with per-rule fixtures.
4. Run `python tests/state_transition_fixtures/run_fixtures.py` — all existing
   tests must still pass.

No changes to any detection rule Python files are required.

## Adding a New Leaf (e.g. ISIS adjacency state)

1. Add a `<leaf_name>:` block to the relevant vendor YAML(s).
2. In the new detection rule, call `is_down(vendor, "<leaf_name>", raw_value)`.
3. Add fixtures for the new rule.

## `device_vendor()` — Graph Round-Trip

`client.device_vendor(address)` executes:

```cypher
MATCH (d:Device {address: '<address>'}) RETURN d.vendor LIMIT 1
```

Result is cached in `client._vendor_cache` for the lifetime of the sidecar process.
Cache is per-process — a sidecar restart clears it cleanly.

## Fallback Behaviour

If `is_down()` returns `False` for an unknown vendor or unmapped state, the rule
does not fire. This is intentional — it is safer to miss a detection on an
unconfigured vendor than to produce spurious alerts on unmapped strings.

Operators must add the vendor YAML to enable detection for a new NOS.

## Files Changed (DV2)

| File | Change |
|---|---|
| `config/vendor_state_mapping/nokia_srlinux.yaml` | NEW — SR Linux BFD/interface/BGP mappings |
| `config/vendor_state_mapping/cisco_iosxr.yaml` | NEW — stub, TODO items noted |
| `config/vendor_state_mapping/cisco_iosxe.yaml` | NEW — stub |
| `config/vendor_state_mapping/juniper_junos.yaml` | NEW — stub |
| `config/vendor_state_mapping/arista_eos.yaml` | NEW — stub |
| `config/vendor_state_mapping/frr.yaml` | NEW — stub (fast-iter lab) |
| `python/bonsai_sdk/state_mapping.py` | NEW — adapter module |
| `python/bonsai_sdk/detection.py` | Added `vendor: str = ""` to `Features` |
| `python/bonsai_sdk/client.py` | Added `device_vendor()` with cache |
| `python/bonsai_sdk/rules/bfd.py` | Replaced `_DOWN_STATES` set with `is_down` |
| `python/bonsai_sdk/rules/interface.py` | Replaced string tuple with `is_down` |
| `python/bonsai_sdk/rules/bgp.py` | Replaced `_HARD_DOWN_STATES` with `is_down/is_up` |
| `tests/state_transition_fixtures/nokia_srlinux/bfd_session_down.yaml` | NEW — 9 fixtures |
| `tests/state_transition_fixtures/nokia_srlinux/interface_down.yaml` | NEW — 7 fixtures |
| `tests/state_transition_fixtures/nokia_srlinux/bgp_session_down.yaml` | NEW — 8 fixtures |
| `tests/state_transition_fixtures/run_fixtures.py` | NEW — fixture runner |

# D2-T1 — event_detection.rs Retirement Gate Smoke Report
**Date**: 2026-05-16  
**Duration**: ~14 hours (cycles 1–18, started 2026-05-15T18:56Z, stopped 2026-05-16T09:06Z)  
**Lab**: `bonsai-dc` — 8 SR Linux nodes (srl-super1/2, srl-spine1/2, srl-leaf1/2/3/4)  
**Chaos plan**: `chaos_plans/always_on_dc.yaml`  
**Total injections**: ~1,980 across all cycles  

---

## Gate Criteria from D2-T1 Spec

| # | Criterion | Result |
|---|---|---|
| 1 | Lab up (8 SRL nodes, BGP EVPN converged) | ✅ PASS — all nodes running, fresh CA, TLS verified |
| 2 | Bonsai running with `BONSAI_REQUIRE_SIDECAR=rules` | ✅ PASS — `/health = ok` |
| 3 | Rules sidecar registered and heartbeating | ✅ PASS — `rules-local` registered, status=healthy |
| 4 | Fault injection ≥ 1 hour | ✅ PASS — ~14 hours, ~1,980 injections |
| 5a | `/api/detections` has `bgp_session_down` rows from sidecar | ✅ PASS — 46 detections |
| 5b | `/api/detections` has `bfd_session_down` rows from sidecar | ❌ FAIL — 0 detections despite bfd faults injected |
| 5c | `/api/detections` has `interface_down` rows from sidecar | ❌ FAIL — 0 detections despite interface_shut faults injected |
| 6 | `detections_out_total` counter incrementing | ⚠️ WARN — counter not exposed in heartbeat payload |

---

## Final Detection Tally

```
bgp_session_down:  46
bgp_session_flap:   4
bfd_session_down:   0  ← gap
interface_down:     0  ← gap
Total:             50
```

---

## What Was Confirmed

- The Python rules sidecar (`python/collector_engine.py`) is registering, staying healthy, and producing
  `bgp_session_down` detections in real time as chaos injects BGP faults. This confirms the F-1/F-2
  fix is working end-to-end: HTTP bind propagates cleanly → sidecar registers → BGP detections flow.
- `bgp_session_flap` also fires (debounced flap detection is live).
- The sidecar stays registered across 14 hours and 18 chaos cycles (no crashes, no re-registration loops).

---

## What Was Not Confirmed — New Finding F-11

Despite ~1,980 chaos injections over 14 hours, including confirmed `bfd_session_down` and
`interface_shut` fault types visible in `chaos_runs/*/injections.csv`, **zero** `bfd_session_down`
or `interface_down` detections appeared in `/api/detections`.

The sidecar advertises both capabilities in `/api/sidecars`:
```json
"capabilities": ["bgp_session_down", ..., "bfd_session_down", "interface_down", ...]
```

Root cause not diagnosed in this smoke. Likely candidates:
1. The gNMI subscription path for BFD state (`/bfd/subinterfaces/subinterface[..]`) is not
   emitting `StateChangeEvent` rows that the sidecar's BFD rule can match against.
2. The `interface_down` rule in `collector_engine.py` may require a different event schema than
   what the gNMI `interface_shut` injection produces (oper-status vs admin-status field name).
3. The sidecar's BFD rule may have a debounce window or peer-IP matching logic that doesn't
   align with how the clab topology labels interfaces.

---

## Gate Verdict

**GATE NOT CLOSED.** `event_detection.rs` remains.

BGP retirement criterion is met. BFD and interface_down criteria are not.

**New backlog item F-11**: Sidecar `bfd_session_down` and `interface_down` rules not producing
detections despite lab faults being injected. Must be diagnosed and fixed before D2-T1 gate closes.
This is a Mac-side investigation (read `collector_engine.py` BFD rule logic + gNMI subscription
path for BFD + StateChangeEvent schema for interface oper-status).

---

## Next Action

On Mac:
1. Read `python/collector_engine.py` — BFD rule handler and interface_down rule handler.
2. Read `src/gnmi_collector.rs` or equivalent — what subscription paths are configured for BFD and
   interface oper-status, and what fields appear in `StateChangeEvent` for those paths.
3. Fix the mismatch (either subscription path, event schema, or rule matching logic).
4. Re-run smoke on Ubuntu — only needs ~30 minutes once the rule mismatch is fixed.
5. Once all three rule_ids appear in detections → delete `event_detection.rs`.

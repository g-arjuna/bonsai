# BN-013 first slice — live incident journey trace (9 November 2026)

Traced one live incident journey end-to-end on the packaged deployment and
recorded break points. No device writes; one investigation record created.

## Journey trace

1. **Incident surface** — `GET /api/incidents` (bsk_ operator key) returns 2
   live grouped incidents (`incidents.json`):
   - `d0bfc7c1…` `bgp_neighbor_up` warning on `172.100.207.12`, 5 cascading
     events; `f0d976b6…` same rule on `172.20.0.1`, 4 cascading.
   - Each carries root detection, `cascading[]`, `affected_devices`,
     `correlation_chain`, `blast_radius_summary`, `co_fire_signature`,
     `incident_type`, `grouping_rationale` — grouping/provenance fields are
     populated, not stubbed.
2. **Investigation creation** — `POST /api/investigations` with
   `{detection_id: <incident root id>, device_address, trigger:
   "bn013-first-slice"}` → HTTP 200, investigation
   `3f597cb3-f64e-4d3b-984d-fdc7362becbc`, `status=running`
   (`create-investigation.json`).
3. **Investigation detail** — `GET /api/investigations/{id}` returns
   `{investigation, tool_calls[]}` (`investigation-detail.json`).

## Break points observed

- **AI investigation does not complete in-window**: ~55 s after creation the
  record still shows `status=running`, `summary=""`, `tokens_used=0`,
  `cost_usd=0.0`, `tool_calls=[]`. Either no AI provider is configured
  (`resolve_active_ai_provider` found none → spawn skipped silently) or
  `investigation_runtime::spawn_investigation` stalled. No error surfaces
  on the record — status stays `running` indefinitely. **Break point #1:
  investigation lifecycle has no timeout/failure transition visible via API.**
- **`completed_at_ns` equals `started_at_ns` while status=running** —
  timestamp semantics unclear (field may be set at creation, not
  completion). **Break point #2: ambiguous completion timestamp.**
- **Incident→investigation linkage is manual**: the incident payload has
  no `investigation_id` field and nothing auto-creates an investigation
  from a grouped incident; operator must POST with the root detection id.
  Acceptable for prototype, but the journey has no guided hand-off.
  **Break point #3: no incident→investigation automation.**
- Positive: incident grouping, correlation chain, blast radius, and the
  investigations API surface all work live with real BMP-sourced data.

## Next slice candidates

- Determine whether an AI provider is configured in this deployment
  (`resolve_active_ai_provider` path) and why the spawned investigation
  never transitions; add a `failed`/`timeout` status if provider absent.
- Adjudication/feedback endpoints (`POST .../feedback`,
  `investigation_accuracy_handler`) untested — next trace step.

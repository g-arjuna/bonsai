# BN-014a — lbug MERGE upsert parser fix + live capture verification

Date: 2026-11-09. Scope: fix `POST /api/webhooks/change-event` →
`ingest_webhook_change` → `upsert_change_request` failing at prepare time,
then unblock BN-014a baseline/candidate capture live verification.

## Root cause

lbug 0.15.3's Cypher lexer (`lbug-src/src/antlr4/Cypher.g4` +
`keywords.txt`) reserves `DESC` (ORDER BY direction) and does **not** list
it among `iC_NonReservedKeywords`. `oC_Parameter: '$' (oC_SymbolicName |
DecimalInteger)` therefore cannot tokenize `$desc` — the parser reports
`expected rule oC_SingleQuery` at the `$desc` offset (~126–136), exactly
matching the live error.

Reproduced outside the app by compiling a probe directly against the
already-built `liblbug` rlib (`rustc --extern lbug=...`):

- `... c.short_description = $desc` → PREPARE FAIL, parser exception at
  the `$desc` token (both the minimal and the full production statement).
- Same failure for `CREATE (q:SavedQuery {description: $desc})` and
  `CREATE (:Playbook {description: $desc})` — the same latent bug existed
  in `create_saved_query` and both Playbook write paths.
- `$type`, bare `SET` after `MERGE` (parsed as a following `oC_Set`
  updating clause), and `ON CREATE/ON MATCH SET` all parse fine.
- With `$desc` renamed to `$description`, the full 17-property
  `MERGE ... ON CREATE SET ... ON MATCH SET` statement prepares and
  executes; second execute takes the ON MATCH path and updates props.

## Fixes (all same-session, each verified live)

1. `src/integrations/change_management.rs` — `$desc` → `$description` in
   the ChangeRequest upsert (both SET clauses + params).
2. `src/graph/mod.rs` (`create_saved_query`) and
   `src/http_server/playbooks.rs` (create + YAML-migration upsert) —
   same `$desc` → `$description` rename; these prepares were equally
   broken.
3. `load_device_ci_map` selected `d.snow_sys_id`, a column that does not
   exist on `Device` (binder exception on the webhook path after fix 1).
   ServiceNow CI sys_ids actually live on `EnrichmentProperty` nodes
   (`key='snow_ci_id'`, `value`=sys_id, `device_address`=hostname). The
   map now queries `MATCH (d:Device) RETURN d.address, d.hostname` plus
   `MATCH (ep:EnrichmentProperty {key: 'snow_ci_id'}) RETURN
   ep.device_address, ep.value` and resolves hostname→address in Rust.
4. `src/http_server/change_capture.rs` `routing_get` sent
   `Authorization: Bearer <token>`; the routing service only accepts the
   `x-bonsai-routing-token` header (same as the `/api/routing/v1/*`
   proxy). Changed to `.header("x-bonsai-routing-token", t)` — verified
   directly against `bonsai-routing:8081` (200 with live peer data).

`link_device_to_change`, `link_event_to_change`,
`link_incident_to_change` and `active_change_context` were checked
against the lbug grammar/probe — plain `MATCH ... MERGE` edge patterns,
no reserved-word params; no changes needed.

## Verification

- New regression test
  `integrations::change_management::tests::change_request_upsert_creates_then_matches`
  (`cargo test -p bonsai --lib change_request_upsert`): real `GraphStore`
  in a tempdir, exercises upsert create + ON MATCH update, then the full
  `ingest_webhook_change` path including `load_device_ci_map` and device
  linking. **1 passed.**
- `cargo check -p bonsai --lib`: pass (9.4 s, sccache warm).
- Packaged image rebuilt twice (`docker compose ... build
  bonsai-standalone bonsai-routing`, ~8–9.5 min release bins each) and
  recreated; `/health` ok.

### Live results (packaged `bonsai:latest`, admin JWT)

```
POST /api/webhooks/change-event  {"change_id":"bn014a-test-001",...}
  → 200 {"change_request_id":"manual:bn014a-test-001","success":true}
     (run twice — second run exercises ON MATCH SET)

POST /api/changes/manual:bn014a-test-001/capture {"phase":"baseline"}
  → 200 route_count=1, peer_count=2, routes_ok, peers_ok, errors=[]
POST /api/changes/manual:bn014a-test-001/capture {"phase":"candidate"}
  → 200 route_count=1, peer_count=2, routes_ok, peers_ok, errors=[]

GET /api/changes/manual:bn014a-test-001/snapshots
  → diffs[0]: status="diffed", route_count_delta=0, peer_count_delta=0,
    prefixes_added=[], prefixes_removed=[]
    snapshots: baseline+candidate each carry prefixes=["10.10.1.0/24"],
    peer_states={"up":2} for device 172.100.207.12 (live containerlab
    NOS feeding BMP via bonsai-routing)
```

Raw responses: `webhook-ingest.json`, `snapshots.json`, `health.json`.

## Notes / limits

- The diff is legitimately empty — no routing change was made between
  baseline and candidate; the capture/diff machinery itself is what was
  under test. A nonzero diff requires an actual route change during a
  change window (BN-014b+ scope).
- `affected_devices` entries that are not IPs resolve through the
  hostname/`snow_ci_id` maps; unknown refs fall back to the raw string
  and the device link simply no-ops if no `Device` node matches.
- SavedQuery/Playbook `$desc` fixes are prepare-level verified (param
  renamed to a non-reserved word; both tables declare `description`);
  their HTTP paths were not exercised live in this session.

# BN-013a — investigation lifecycle fix (9 November 2026)

Fixes break point #1 from `2026-11-09-bn013-incident-trace/`: investigations
created while no AI provider is configured stayed `status=running` forever
(`tokens_used=0`, `tool_calls=[]`, no failure transition).

## Root cause

`create_investigation_handler` (`src/http_server/remediation.rs`) only calls
`investigation_runtime::spawn_investigation` inside
`if let Some((ai_cfg, api_key)) = resolve_active_ai_provider(&state).await`.
When the vault/env fallback resolves `None`, nothing ever transitions the
record — it remains `running` indefinitely.

## Fix (commit `c685a48`)

Added an `else` branch: when no provider resolves, the handler calls
`store.complete_investigation(id, "failed", "STOP_REASON=no_ai_provider…",
"{}", 0, 0.0)` and returns the refreshed record, so the API response itself
reflects the terminal state.

## Live verification (packaged `bonsai:latest`, rebuilt 463 s)

`POST /api/investigations` → HTTP 200, record
`90c1dec8-a240-4b4e-aa63-e340a0b693d7`:

- `status: "failed"` (was `running` forever before)
- `summary: "STOP_REASON=no_ai_provider: no AI provider configured;
  investigation cannot run."`
- `completed_at_ns: 1794204252187922993` — real completion timestamp, no
  longer equal to `started_at_ns`
- `GET /api/investigations/{id}` confirms persisted `failed` status,
  `tool_calls: []`

Evidence: `create-investigation.json`, `investigation-detail.json`.

## Honest notes

- This deployment has **no AI provider configured** (vault active-provider
  unset; env fallback requires `api_key_env`). Grounded-RCA quality remains
  unprovable until a provider is configured — that is a BN-004 dependency,
  not a code defect.
- Spawned investigations that hang mid-run still have no watchdog/timeout
  transition — follow-up if a provider is ever configured.
- Next slice BN-013b: adjudication + feedback live exercise (endpoints
  exist, untested).

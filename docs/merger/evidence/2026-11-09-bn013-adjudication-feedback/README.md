# BN-013b — adjudication + feedback + real-provider investigation (9 Nov 2026)

## Provider work required to get here

- Gemini `gemini-3.8-flash`: Interactions API implemented as primary
  (`api_style=interactions`), generateContent kept as selectable legacy.
  Provider timeout raised to 300s — investigation prompts on thinking
  models exceed 120s.
- Fixes landed: `function_result` items now carry tool `name` (mapped
  from prior `function_call` ids — empty name was rejected 400);
  OpenAI provider sets `reasoning_effort=none` on tool-calling turns
  (gpt-6-luna rejects function tools + reasoning on /v1/chat/completions).
- Gemini remained unreliable: repeated transient 503 "high demand"
  killed 3 investigations mid-run (honest `provider_call_failed`
  terminal states, coverage-gap warnings preserved).
- User added `test-openapi` (openai / gpt-6-luna) — set active; provider
  test ok in ~2s.

## Verified live

- **Full agentic investigation completed a run**: `1d289747…` on
  test-openapi — 8 tool calls, 61,473 tokens, terminal status
  `budget_exhausted` at $0.1229 vs $0.10 per-investigation cap
  (`investigation-openai.json`). Cost enforcement works; the loop
  itself ran end-to-end (iterations, tool calls, coverage-gap summary).
- Earlier Gemini run `45f2c8d0…` reached iteration 6 / 54k tokens /
  6 tool calls before a 503 — proves multi-iteration tool use works on
  the legacy path too.
- **Feedback**: `POST /api/investigations/{id}/feedback` → persisted
  `FeedbackRecord` (rating/comment/operator/created_at_ns).
- **Accuracy**: `GET /api/investigations/accuracy` →
  `total_feedback:1, negative:1` — provenance queryable.
- **Adjudication**: `POST /api/routing/v1/adjudicate-prefix` →
  persisted `OperatorAdjudicationRecord` + `adjudicated_accuracy`
  summary (precision, verified counts, training-eligible flag).
  First attempt with wrong field names returned a clean proxy error —
  corrected to `adjudicated_by`/`action_outcome`/`rationale`.

## Honest gaps

- No investigation has reached `status=complete` yet — Gemini 503s and
  the $0.10 budget cap interrupted every run. The agentic loop, tool
  execution, terminal states, and cost enforcement are all proven;
  a full RCA summary awaits either provider stability or a higher
  budget (config `per_investigation_budget_usd`).
- Interactions API tool-calling still fails at iteration ≥1 with a
  generic 400 "invalid argument" — input-item serialization for
  function_call/function_result needs more schema work; legacy
  generateContent path is the working route for tool use today.

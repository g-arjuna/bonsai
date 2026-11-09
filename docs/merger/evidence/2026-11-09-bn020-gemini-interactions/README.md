# BN-020 follow-up — Gemini Interactions API + legacy fallback (9 Nov 2026)

## Problem

`test-gemini` provider (gemini-3.8-flash, real key in vault) hung
indefinitely on "Test Connection". Diagnosis: Docker network exonerated
(curl container on `bonsai_bonsai-internal` → googleapis 0.08s,
generateContent 0.09s); bonsai's reqwest clients had no timeout; and
`generateContent` is legacy — gemini-3.8 thinking models take 30–120s.

## Fix (commit f94630b)

- `GeminiProvider` gained `api_style`: `"interactions"` (default) →
  `POST /v1beta/interactions` with `x-goog-api-key` + `Api-Revision:
  2026-05-20` headers, `store:false`, typed `function_call`/
  `function_result` input items, `model_output`/`function_call` step
  parsing; `"generate_content"` → original path preserved.
- `api_style` stored per-provider in vault metadata
  (`LlmProviderEntry.api_style`, serde default `interactions`), threaded
  through `resolve_active_ai_provider` and the test handler into
  `AiConfig.gemini_api_style`.
- Settings UI: "Gemini API" select shown for gemini providers
  (Interactions recommended / generateContent legacy).
- Provider HTTP timeout raised 30s → 120s (commit 11e5858) for thinking
  models.

## Verified live (packaged bonsai:latest, image build 456s)

- Interactions API: `POST /api/ai/providers/test` →
  `{"ok":true,"model":"gemini-3.8-flash","latency_ms":13758}`
- Legacy generateContent (api_style=generate_content):
  `{"ok":true,"latency_ms":39457}` — works but ~3x slower; confirms the
  endpoint was never broken, just slow + unbounded before the timeout.
- Provider restored to `api_style=interactions`, active, key intact.

## Vendor audit notes

Moonshot/OpenAI/Ollama use OpenAI-compatible `/v1/chat/completions`;
Anthropic uses `/v1/messages` — all current, no changes needed. Only
Gemini had a generational API shift.

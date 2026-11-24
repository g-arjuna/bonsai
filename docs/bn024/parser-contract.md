# BN-024-01 — Parser chain wire contract

Status: implemented in `src/parser_chain.rs`, `src/enrichment/parser_chain_enricher.rs`,
`docker/sidecars/pyats/app.py`, `docker/sidecars/bonsai-native-parser/app.py`.

## Wire contract

`POST {sidecar}/parse` — identical shape for both sidecars:

Request:

```json
{
  "parser": "<wire parser name>",
  "vendor": "<bonsai vendor id>",
  "command": "<literal CLI command>",
  "command_pattern": "<same value, kept for older sidecars>",
  "raw_output": "<raw device output>"
}
```

Response:

```json
{
  "parser": "<echoes the request's wire parser>",
  "backend_used": "genie | textfsm | fallback | line_split",
  "vendor": "...",
  "command": "...",
  "parsed_json": { "...": "..." }
}
```

`backend_used` is optional on the wire (older sidecars omit it); Rust maps it
to a quality class and never invents one.

## Backend dispatch

Internal parser-chain names (used in `parser_chain.priorities` config keys)
map explicitly to sidecars and wire parser names:

| Chain name      | Sidecar              | Wire `parser` | `backend_used` when it works |
|-----------------|----------------------|---------------|------------------------------|
| `bonsai_native` | local (no HTTP)      | n/a           | `line_split`                 |
| `pyats_genie`   | `pyats_url`          | `genie`       | `genie`                      |
| `ntc_templates` | `pyats_url`          | `textfsm`     | `textfsm`                    |
| `suzieq_native` | —                    | —             | **unsupported** (BN-024: no SuzieQ runtime) |

Any other name fails the attempt with `unsupported parser backend`.

Default priority order is structured-first: `pyats_genie`, `ntc_templates`,
`bonsai_native`. Unstructured line-split output is a last resort and can no
longer shadow a structured backend.

## Quality semantics

`ParseAttempt.quality` / `ParseDecision.quality`:

- `structured` — `backend_used` ∈ {`genie`, `textfsm`}
- `unstructured_fallback` — `backend_used` ∈ {`fallback`, `line_split`}
- `unknown` — `backend_used` absent or unrecognized

The CLI enricher maps quality to capture confidence (`high` / `low` / `medium`)
and records `parse_quality` plus per-attempt `backend_used` in `details`, so
line-split output is never presented as structured fact.

## Response validation

Rust rejects a sidecar response when:

- HTTP status is not 2xx,
- the body is not valid `ParseResponse` JSON,
- `parser` does not echo the requested wire parser name (backend-identity
  mismatch).

Sidecar calls have a 10 s timeout; a hung sidecar fails the attempt instead of
stalling enrichment. Failed attempts fall through to the next configured
parser; if all fail, `parse()` returns an explicit error.

## Compatibility notes

- `command_pattern` is still sent alongside `command` so older sidecar images
  that only know `command_pattern` keep working; the pyats sidecar ignores
  unknown fields.
- `ParseResponse.backend_used` is `Option` — old sidecars without it produce
  `quality = "unknown"`, not a fabricated `structured`.
- `suzieq_native` previously routed to the native line-split sidecar and
  silently "succeeded". It is now an explicit unsupported-backend failure until
  a real SuzieQ-derived parser exists (BN-024-02+).

## Tests

- Rust wiremock contract tests in `src/parser_chain.rs` (`cargo test -p bonsai
  --lib parser_chain`) pin the exact request body, backend dispatch, response
  validation, quality labeling, and fallback ordering.
- `tests/test_bn024_parser_contract.py` AST-checks both sidecar apps expose the
  contract fields without requiring fastapi/pydantic installed.

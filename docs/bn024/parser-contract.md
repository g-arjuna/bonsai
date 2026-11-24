# BN-024-01 — Parser chain wire contract

Status: implemented in `src/parser_chain.rs`, `src/enrichment/parser_chain_enricher.rs`,
`docker/sidecars/pyats/app.py`, `docker/sidecars/bonsai-native-parser/app.py`,
`scripts/cli_capture.py`.

This document describes the *corrected* contract after the BN-024 G0 review
(CR-01…CR-07). Earlier claims that structured-first ordering prevented
fallback shadowing, and that AST checks proved backend execution, were wrong
and are superseded here.

## Wire contract

`POST {sidecar}/parse` — identical shape for both sidecars:

Request:

```json
{
  "parser": "<wire parser name>",
  "vendor": "<backend vendor id>",
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
  "outcome": "structured_success | structured_empty | unstructured_fallback | invalid_input",
  "vendor": "...",
  "command": "...",
  "detail": "<optional sanitized backend diagnostics>",
  "parsed_json": { "...": "..." }
}
```

`backend_used` and `outcome` are optional on the wire (older sidecars omit
them); Rust maps absent values to `unknown` quality / `unknown_quality`
outcome and never invents structured authority for them.

### Outcome semantics (pyats sidecar)

Each backend reports one of: `structured_success`, `structured_empty`
(parser ran, legitimately zero records — e.g. an empty but valid BGP table),
`unsupported` (no Genie OS / NTC template for vendor+command),
`backend_unavailable` (parser library missing), `parser_error` (execution
failure). Only `structured_*` outcomes surface as `backend_used` `genie` /
`textfsm`. Everything else degrades to `backend_used: "fallback"` with
`outcome: "unstructured_fallback"` (or `invalid_input` for empty captures),
plus a sanitized `detail` string. Raw device output and credentials are never
included in diagnostics.

The Genie path uses the real `genie.libs.parser.utils.get_parser(command,
device)` → `(parser_class, kwargs)` → `parser.cli(output=...)` contract
(verified against genie 26.10 in the packaged image); the earlier
`get_parser(os, command)` invocation was invalid and could never have run a
parser.

## Backend dispatch

Internal parser-chain names (used in `parser_chain.priorities` config keys)
map explicitly to sidecars and wire parser names:

| Chain name      | Sidecar              | Wire `parser` | Permitted `backend_used` |
|-----------------|----------------------|---------------|--------------------------|
| `bonsai_native` | local (no HTTP)      | n/a           | `line_split`             |
| `pyats_genie`   | `pyats_url`          | `genie`       | `genie`, `fallback`      |
| `ntc_templates` | `pyats_url`          | `textfsm`     | `textfsm`, `fallback`    |
| `suzieq_native` | —                    | —             | **unsupported** (BN-024: no SuzieQ runtime) |

Any other name fails the attempt with outcome `unsupported`. An explicit
`genie` request answered by `backend_used: "textfsm"` (or vice versa) is
rejected as `invalid_response` — cross-backend substitution is not accepted
as the requested parser's success.

## Platform → backend vendor translation

`ParseRequest.vendor` carries the canonical Bonsai platform identity (or a
registry alias). `PlatformProfile.parser_backend_id` in
`src/platform_registry.rs` is the single explicit translation boundary to the
backend vendor key the sidecars understand (`cisco.iosxr` → `cisco_iosxr`,
`arista.eos` → `arista_eos`, `juniper.junos` → `juniper_junos`,
`nokia.srlinux` → `nokia_srlinux`, `nokia.sros` → `nokia_sros`,
`cisco.iosxe` → `cisco_iosxe`, `cisco.nxos` → `cisco_nxos`, `frr` → `frr`).
Platforms with `parser_backend_id: None` (including `unknown`) fail the
attempt as `unsupported` before any HTTP call — an unrecognized vendor cannot
masquerade as a structured parse. Priority lookup keys accept the raw vendor
string, the canonical `platform_id`, or the backend id, so existing
underscore/dash config keys keep working.

A vendor mapping is necessary but not sufficient: it identifies the backend
dialect, not proof that a given command is supported. Unsupported commands
surface as `unsupported`/`parser_error`/`unstructured_fallback`, never as
structured facts.

## Quality and outcome semantics

`ParseAttempt.quality` / `ParseDecision.quality`:

- `structured` — `backend_used` ∈ {`genie`, `textfsm`} with a consistent
  payload (object for Genie, record list for TextFSM; non-null, non-scalar,
  not fallback-shaped)
- `unstructured_fallback` — `backend_used` ∈ {`fallback`, `line_split`}
- `unknown` — `backend_used` absent (legacy sidecar; never promoted)

`ParseAttempt.outcome` uses: `structured_success`, `structured_empty`,
`unstructured_fallback`, `unsupported`, `backend_unavailable`, `timeout`,
`invalid_input`, `invalid_response`, `parser_error`, `unknown_quality`.

`structured_empty` is a *successful structured result*: an empty BGP table is
meaningful evidence and terminates the chain like any structured output. It
is never conflated with parser failure.

## Chain selection policy

An executed attempt is not a successful result. The chain:

1. Validates each response (backend identity, vendor/command echo, payload
   shape, backend/outcome consistency) before classifying it.
2. Retains the best degraded candidate (unstructured fallback, then unknown
   quality) while continuing to try structured backends in configured order.
3. Selects the first structured result by priority; stops early in
   non-consensus mode once structured output is obtained.
4. Falls back to the retained degraded candidate only when
   `parser_chain.allow_unstructured_fallback` is true (default). When false,
   no structured result is an explicit `no permissible parser result` error.
5. In `consensus_mode` every configured backend runs; the same ranking picks
   the primary.

`consensus_state` is representation-level only, over structured attempts:
`single_structured_result`, `representation_match`,
`representation_difference`, `not_comparable`, `no_structured_result`.
Unstructured/unknown attempts are excluded — a line-split is not an
independent structured opinion, and two parsers on one capture share the
same underlying evidence. Field-level semantic comparison is BN-024-02+.

## Bounds

- 10 s per-attempt sidecar timeout; 60 s total chain budget.
- ≤ 8 backend attempts per resolved chain; duplicate names deduplicated.
- ≤ 4 MiB `raw_output` and ≤ 4 MiB sidecar response (rejected, never
  truncated into apparent completeness).
- Local line-split retains ≤ 10 000 lines with an explicit `truncated` flag.
- `cli_capture.py` reads device output through a bounded channel loop
  (`--max-bytes`, default 4 MiB) and fails explicitly beyond it.
- The HTTP client is built with the timeout; if construction fails, sidecar
  calls fail as `backend_unavailable` rather than running unbounded.
- Attempt diagnostics carry `parsed_sha256`/`parsed_bytes` instead of
  duplicating full payloads into every status object; the selected payload
  is stored once.

## Confidence semantics

`MultiSourceCapture.confidence` no longer equates parser output format with
observation trust: `structured` → `medium`, everything else → `low`. A
structurally valid parse does not authenticate the device, prove freshness or
completeness, or establish semantic correctness. BN-024-02 formalizes the
trust dimensions.

## Compatibility notes

- `command_pattern` is still sent alongside `command`.
- `ParseResponse.backend_used`/`outcome`/`vendor`/`command` are `Option` —
  old sidecars produce `quality = "unknown"`, not fabricated `structured`.
- `suzieq_native` remains an explicit unsupported-backend failure.
- `consensus_state` values changed from `agreed`/`disagreed`/`single-parser`
  to the representation vocabulary above; consumers must not treat
  `representation_match` as semantic corroboration.

## Tests

- Rust wiremock contract tests in `src/parser_chain.rs`
  (`cargo test -p bonsai --lib parser_chain`) pin request bodies, backend
  dispatch, response validation, quality/outcome labeling, fallback
  shadowing regressions (CR-01), platform-id translation (CR-02),
  adversarial identity/payload cases (CR-03), fallback policy, dedup and
  bounds.
- `tests/test_bn024_parser_contract.py` pins both sidecar apps' contract
  fields, outcome vocabulary and the real Genie API usage.
- Genuine packaged-sidecar execution evidence lives under
  `docs/bn024/evidence/` (see Stage-2 report for image digest and versions).

# BN-024-01 evidence — parser contract fix + differential baseline

Date: 2026-11-24
Branch: `feat/bn024-01-parser-contract`

## What was broken (reproduced by inspection + contract tests)

1. Rust `SidecarParserClient` POSTed `{parser, vendor, command_pattern,
   raw_output}`; the PyATS sidecar's `ParseRequest` requires `command` → every
   sidecar call returned HTTP 422.
2. Internal parser names (`pyats_genie`, `ntc_templates`) were sent verbatim as
   the sidecar `parser` field; the sidecar only accepts `genie`/`textfsm`/`auto`.
3. `ntc_templates` and `suzieq_native` were routed to the native line-split
   sidecar — `suzieq_native` has no implemented backend at all.
4. `ParseResponse` ignored `backend_used`, so Genie/TextFSM structured output
   and line-split fallback were indistinguishable to consumers.
5. `default_priorities()` put `bonsai_native` first; in non-consensus mode the
   chain always stopped at the line-splitter.

## What changed

- `src/parser_chain.rs`: explicit backend dispatch (`bonsai_native` local,
  `pyats_genie`→pyats/`genie`, `ntc_templates`→pyats/`textfsm`,
  `suzieq_native`→explicit unsupported), wire body now sends `command` (plus
  `command_pattern` for older sidecars), response `parser` must echo the wire
  parser (backend-identity check), `backend_used` propagated to
  `ParseAttempt`/`ParseDecision.quality` (`structured` /
  `unstructured_fallback` / `unknown`), 10 s sidecar timeout, structured-first
  default priorities, all-attempt failure error now includes per-attempt
  causes, `native_parse` output self-labels `unstructured_fallback`/`line_split`.
- `src/enrichment/parser_chain_enricher.rs`: capture `confidence` now follows
  parse quality (high/low/medium); `details.parse_quality` added.
- `docker/sidecars/bonsai-native-parser/app.py`: accepts `command` (canonical)
  or `command_pattern`, returns `backend_used: "line_split"` and labeled
  `parsed_json`.
- `docker/sidecars/pyats/app.py`: unchanged — it already defined the correct
  contract; Rust now conforms to it.
- `docs/bn024/parser-contract.md`: wire schema, dispatch table, quality
  semantics, compatibility notes.
- `scripts/bn024/differential_baseline.py` + this evidence directory.

## Executed verification

| Check | Command | Result |
|---|---|---|
| Lib unit tests (parser_chain + enricher) | `cargo test -p bonsai --lib --profile ci parser_chain` (shared lane) | 14/14 pass |
| Vendor registry regression | `cargo test -p bonsai --profile ci --test b07_vendor_registry` | 4/4 pass |
| Python contract tests | `pytest tests/test_bn024_parser_contract.py tests/test_bn024_provenance.py` | 18/18 pass |
| L0 gate | `scripts/ci/gates.sh l0` | PASS |
| L1–L3 gates | `scripts/ci/run-local.sh --trusted --shared-target-dir build-lane --lbug-native lbug-native l1 l2 l3` | see PR body / runs.csv |

New wiremock tests pin: exact request body (`command` field, wire parser
names), pyats/textfsm dispatch, `suzieq_native` unsupported without any HTTP
call, fallback→`unstructured_fallback` labeling, missing `backend_used`→
`unknown`, backend-identity mismatch rejection, structured-first default
ordering, and sidecar-error fallthrough.

## Differential baseline (bounded)

`scripts/bn024/differential_baseline.py --suzieq-src <pinned checkout>` compares
Bonsai `bonsai_native` output against SuzieQ's typed avsc schemas for the same
nossim CLI fixtures. Result (`differential-baseline.json`):

- 9 fixtures evaluated across iosxe/qfx/mx platforms.
- SuzieQ typed-field total: **190**; Bonsai typed-field parity: **0**.
- Bonsai `bonsai_native` parse cost: ~0.01–0.14 ms/fixture (line-split).

This is an honest gap measurement, not a parity claim: no structured parser
(Genie/TextFSM/SuzieQ-derived) ran in this baseline — those backends require
the sidecar environment and are exercised only via the wiremock contract tests
here. Closing the 190-field gap is BN-024-02+ work.

## Remaining parity gaps / not done

- No live sidecar integration run (no Genie/TextFSM install on this host;
  contract verified at the wire boundary only).
- `suzieq_native` remains unimplemented by design.
- Consensus mode still compares exact JSON equality; structured-vs-fallback
  disagreement surfaces as `disagreed` (explicit, intended).
- L4 lab gate not applicable/executed for this slice.

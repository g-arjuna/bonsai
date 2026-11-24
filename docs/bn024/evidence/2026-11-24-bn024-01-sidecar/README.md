# BN-024-01 packaged PyATS sidecar validation (2026-11-24)

Genuine execution evidence for the corrected `docker/sidecars/pyats/app.py`
against the real Genie/TextFSM/ntc-templates libraries — not mocks, AST
tests, or wiremock.

## Artifact

- Image: `bonsai-pyats-sidecar:bn024`
- Image digest: `sha256:6a15831d4dd6c0ea9af5b9edf1c85cfe6699907b4c6a3faf3b1d73ecafa6e482`
- Built from `docker/sidecars/pyats/` at this commit; deps pinned by
  `pyproject.toml`/`poetry.lock` inside the image.

## Actual dependency versions (inside image)

| package | version |
|---|---|
| python | 3.11.18 |
| genie | 26.10 |
| pyats | 26.10 |
| ntc-templates | 9.4.0 |
| textfsm | 2.1.1 |
| fastapi | 0.115.12 |
| uvicorn | 0.34.2 |

## Method

Container `bn024-pyats` published `127.0.0.1:19101->9101`. Requests were
driven over HTTP `/parse` with the wire contract (`parser`, `vendor`,
`command`, `command_pattern`, `raw_output`). Fixture:
self-authored IOS-XE `show ip bgp summary` (2 neighbors), sha256 recorded
in `sidecar-validation.json`.

## Results (see sidecar-validation.json for full payloads)

| case | backend_used | outcome | notes |
|---|---|---|---|
| genie / cisco_iosxe / bgp summary | genie | structured_success | dict w/ bgp_id, vrf |
| textfsm / cisco_iosxe / bgp summary | textfsm | structured_success | list[2] neighbor records |
| genie / cisco_iosxr / same output | genie | structured_empty | valid empty ≠ failure |
| genie / empty raw_output | fallback | invalid_input | explicit, no fabricated parse |
| genie / nokia_srlinux | fallback | unstructured_fallback | vendor unsupported |
| textfsm / nokia_srlinux | fallback | unstructured_fallback | vendor unsupported |
| genie / bogus command | fallback | unstructured_fallback | ParserNotFound → fallback |
| textfsm / bogus command | fallback | unstructured_fallback | ParsingException → fallback |
| genie / unknown vendor "vyos" | fallback | unstructured_fallback | unsupported vendor |
| parser=auto / iosxe | genie | structured_success | auto prefers genie |

Latency: genie ~60–150 ms, textfsm ~10 ms per call (includes library
import amortisation).

## Rust ↔ live sidecar integration

`parser_chain::live_sidecar_tests::live_pyats_sidecar_genie_textfsm_and_fallback`
(ignored by default; run with `BONSAI_LIVE_PYATS_URL`) executes the real
`ParserChain` over HTTP against this image: genie → `structured`,
ntc_templates → `structured` (array payload), unsupported command →
`unstructured_fallback`. Result: PASS (36 parser_chain tests, run
2026-11-24 on shared ci lane).

## Limitations (honest)

- Genie running-config (`show running-config`) parsing is **not** covered:
  Genie cli() expects command output, not config; config parsing needs
  Genie's `config` API — out of scope, documented as a gap.
- `suzieq_native` remains explicitly unsupported (no SuzieQ runtime).
- Unsupported NOS/vendor combinations degrade to the sidecar's bounded
  line-split fallback, surfaced as `unstructured_fallback`, never as
  structured facts.
- Sidecar is single-process uvicorn; production deployment/HA is BN-024-03+.

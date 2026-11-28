# IGP fixture provenance

The JSON documents in this directory are **synthetic integration controls**.
They are hand-authored from the FRRouting `ospf6d`/`ospfd` and `isisd` JSON
output contracts inspected in the FRR source tree; they are not genuine live
device captures and must not be represented as operational network evidence.

Fixture contract: `frr_igp_operational_synthetic_control_v1`.
Intended parser: `frr_igp_operational_normalizer` revision
`2026-11-28.initial-v1`.

Live FRR capture was unavailable in the current environment because the
containerlab control-plane daemons were not running. Genuine empty or inactive
protocol populations therefore could not be collected during this increment.

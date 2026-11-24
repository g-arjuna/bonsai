# BN-024-01 genuine differential oracle (2026-11-24)

Replaces the earlier gap-only baseline (`../2026-11-24-bn024-01/`, retained
for history — it measured Avro schema field *counts* and a Python mirror of
line-splitting; that methodology was wrong and is superseded).

## Method — real execution on both sides

- **Reference**: pinned SuzieQ TextFSM templates
  (`suzieq/config/textfsm_templates/*.tfsm` @ `75198b5…`) executed by the
  pinned TextFSM library source (google/textfsm @ `7e4ed59…`), on pinned
  SuzieQ `tests/integration/nossim` fixtures. SuzieQ service-layer
  post-processing is explicitly out of scope (documented limitation).
- **Bonsai**: the packaged `bonsai-pyats-sidecar:bn024` image (the same
  artifact `ParserChain` dispatches to), exercised over HTTP for both the
  `textfsm` (ntc-templates 9.4.0) and `genie` (26.10) backends.
- **Comparison**: per-record typed-field diff on explicit join keys and
  per-field mapping tables. Every count carries its denominator; unmappable
  fields are `not_comparable`, never counted as matches. No aggregate
  "parity %" is published.

## Scope

NOS `iosxe`, fixtures under `tests/integration/nossim/iosxe/16.10.1/C9300`:
`show interfaces`, `show ip arp`, `show ip route vrf all`.

## Headline results (full data: differential-report.json)

- interfaces: 10/10 records joined both backends. Exact matches: ifname,
  mtu, description, macaddr (8/10), type+state (genie). Genuine diffs:
  `speed` format (SuzieQ kbps `1000000` vs ntc `1000Mbps` — unit
  representation, intended difference); `adminState` enum vs Genie bool
  (representation difference).
- ip arp: 3/3 records joined; all 5 mapped fields exact on both backends.
- routes: 40 unique prefixes joined both backends (47 raw SuzieQ records
  dedup to 40 join keys). prefix/protocol/preference/metric/nexthopIps/
  uptime all match except 1 textfsm protocol mismatch (genuine
  unexplained diff — see report sample_mismatches). Genie lacks
  nexthopIps/uptime mappings → `not_comparable`.

## Reproduce

```bash
# pinned checkouts (see docs/bn024/provenance/upstream-sources.json)
PYTHONPATH=<textfsm-checkout> python3 scripts/bn024/differential_baseline.py \
    --suzieq-src <suzieq-checkout> --textfsm-src <textfsm-checkout> \
    --pyats-url http://127.0.0.1:19101 --out report.json
```

Verification run used git checkouts at the pinned revisions in
`/tmp/bn024-verify-git/` (revision-verified by
`scripts/bn024/upstream_provenance.py verify`).

## Resource usage

Sidecar calls ~10–150 ms per fixture; SuzieQ template parse <5 ms.
No meaningful host resource consumption.

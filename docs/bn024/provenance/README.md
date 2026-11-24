# BN-024-00 — Upstream provenance, license and reproducibility

Machine-readable provenance for every external asset BN-024 may reference,
reuse or redistribute. The manifest is
[`upstream-sources.json`](upstream-sources.json); the offline validator and
hash verifier is
[`scripts/bn024/upstream_provenance.py`](../../../scripts/bn024/upstream_provenance.py).

## Pinned sources

| Source | Repository | Immutable revision | License |
|---|---|---|---|
| SuzieQ OSS | `netenglabs/suzieq` | `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad` | Apache-2.0 (LICENSE at revision) |
| NTC-Templates | `networktocode/ntc-templates` | `06fa33389fbcca31cd59aac5fd5cb2ce5c900939` | Apache-2.0 (LICENSE + pyproject) |
| TextFSM | `google/textfsm` | `7e4ed596442867fc961c0788e7cac5bc8d091da9` | Apache-2.0 (LICENSE.txt) |

SuzieQ Enterprise / proprietary material is **out of scope** and must never
be added to this manifest. `suzieq/config/etc/` was deliberately excluded:
it contains upstream test private keys (`cert.pem`, `key.pem`,
`ssh_insecure_key`) that are irrelevant to parser reference and must not be
vendored.

## Reuse decisions (current)

Every entry is `review_status: pending_review`. Nothing is vendored
(`local_destination: null`, `modification_status: not_vendored`). Planned
classifications:

- `suzieq/config/*.yml`, `suzieq/config/schema/*.avsc`,
  `suzieq/poller/worker/services/*.py`, `suzieq/engines/**` —
  **behavioral reference** for the restricted extraction DSL and Rust
  normalizers. Any actual copying or translation is
  `modified_translation_reuse` and requires `approved_reuse` review plus
  Apache-2.0 notice retention *before* vendoring.
- `suzieq/config/textfsm_templates/`, `tests/**`, `samples/` —
  **local-only oracle material** pending per-asset review. Upstream test
  fixtures may embed device captures whose redistribution rights are not
  established; until reviewed they stay out of the public repository. A
  fixture that cannot be redistributed is replaced by a locally acquired
  oracle input with a documented acquisition + hashing procedure.
- NTC-Templates / TextFSM — license evidence pinned; templates are used
  today via the Python sidecar. Rust-side reuse is a separate BN-024-03+
  decision.

Rust translation does **not** remove Apache-2.0 obligations: adapted code
remains a derivative work. This manifest records intent, not legal
approval — asset-specific `approved_reuse` sign-off is an owner decision.

## Commands

Validate the manifest (offline, CI-safe):

```bash
python3 scripts/bn024/upstream_provenance.py validate \
  docs/bn024/provenance/upstream-sources.json
```

Verify content hashes against local checkouts of the pinned revisions:

```bash
# layout: <dir>/suzieq, <dir>/ntc-templates, <dir>/textfsm
python3 scripts/bn024/upstream_provenance.py verify \
  docs/bn024/provenance/upstream-sources.json <checkout-dir>
```

Reproduce a fresh SuzieQ checkout at the pinned revision:

```bash
curl -sL -o suzieq.tar.gz \
  https://codeload.github.com/netenglabs/suzieq/tar.gz/75198b5585ebef91f0659b6b1ec3c964cc2cd1ad
tar xzf suzieq.tar.gz
```

## Verification status

- `validate`: PASS (114 entries, 2026-11-24).
- `verify`: PASS against the pinned SuzieQ tarball and pinned
  NTC-Templates/TextFSM license/metadata fetches (114/114 hashes).
- pytest: `tests/test_bn024_provenance.py` — 14 tests incl. negative
  fixtures (missing notice, unapproved reuse, malformed hashes, tampered
  checkout).

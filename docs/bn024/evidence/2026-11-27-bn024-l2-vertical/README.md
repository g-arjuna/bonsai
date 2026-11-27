# BN-024 / PR #95 Linux L2 vertical-slice final evidence

Date: 2026-11-27. Branch/worktree: `feat/bn024-l2-vertical` /
`bonsai-netintel-l2slice`.

## Frozen implementation identity

- Integration base: `2a8c29f6d89a35476b5b5aa24c3b413bd0eb46b1`
- Tested implementation SHA: `2a3b6be1495b4f3c7d5e3712bc9f0858f5f76367`
- Implementation commits since review:
  - `648fe0096ab8f1e0173fa4145eedfb30cbe12a9e` — lifecycle, analysis,
    compatibility, provenance and operational presentation corrections.
  - `2c002a3bd934ec1d00d337d8bd177a43e7d0a3a5` — precise missing capture-slot
    validation ordering.
  - `2a3b6be1495b4f3c7d5e3712bc9f0858f5f76367` — restored the public bounded
    stream helper required by the existing capture contract tests. This final
    commit changed only `scripts/frr_interface_capture.py`; the canonical gate
    was nevertheless rerun against the exact resulting SHA.
- Documentation/evidence commits after this file is committed are packaging
  commits only; they do not change the tested implementation.

## Final correction disposition

- A1 positive analytical freshness: corrected. Current operational analysis
  uses an explicit bounded observation-age policy without weakening the shared
  fail-closed freshness default or inventing source validity windows.
  `authorized_api_produces_positive_operational_neighbor_analysis` passes
  through the authorized API using the production envelope constructor and
  persisted evidence; it is a positive neighbor correlation, not merely an
  `l2_assessment` object or unresolved-prerequisite response.
- A2 stale last-good exclusion: corrected. Current analysis consumes eligible
  current/partial-current family evidence at an explicit analysis time, keeps
  stale last-good evidence inspectable, accounts for newer acquisition failure,
  and labels historical interpretation separately.
  `transport_failure_excludes_stale_last_good_from_current_operational_analysis`,
  `newer_interface_evidence_does_not_refresh_older_retained_vlan_evidence` and
  `observation_older_than_operational_freshness_window_is_not_current_analysis`
  pass.
- A3 single transactional family-lifecycle decision: corrected. Merged family
  state is selected once in the persistence transaction and reused for the
  stored observation, acquisition state and latest-attempt representation;
  older completion order cannot overwrite newer per-family attempt metadata.
  `concurrent_persistence_admits_newer_epoch_regardless_of_completion_order`
  and restart consistency regressions pass.
- A4 transport failure per-family eligibility: corrected. Complete transport
  failure degrades current eligibility for retained families without deleting
  historical evidence; newer useful partial evidence can replace older partial
  evidence under the documented lifecycle policy.
- A5 PR #89 upgrade compatibility: corrected. A genuine prior interface-only
  stored observation remains readable, can be replaced by a multifamily
  observation, survives restart and does not fabricate the new families as
  empty. `pr89_interface_only_observation_remains_readable_replaceable_and_restart_safe`
  passes.
- A6 semantic/provenance bindings: corrected. Retained coverage, command scope,
  fact observation time, raw-output digest, trust class, retention accounting
  and positive-empty consistency are validated; report metadata is derived
  from validated state where it duplicated authoritative lifecycle state.
  `tampered_family_coverage_and_raw_output_are_rejected` passes.
- A7 independent mandatory release evidence: corrected. The Rust production
  constructor parses and cross-checks the bounded version JSON against the
  reported release/profile envelope instead of trusting duplicate metadata.
  `production_constructor_rejects_contradictory_mandatory_version_evidence`
  passes. The approved acquisition plan remains authoritative for admitted
  command slots; missing slots are reported precisely.
- A8 temporal metadata: corrected. RFC-3339/epoch consistency, command order,
  per-command interval containment, exact command slots and outcome/output
  combinations are validated. Replay ingestion time is not allowed to renew
  historical observation freshness; missing/not-attempted commands are not
  represented as captured empty output.

## Stage B operational consistency

- Retention truncation is explicit: source completeness, emitted/validated /
  retained/omitted accounting and retained-population truncation remain
  distinct. `per_family_retention_truncation_is_explicit_and_does_not_starve_l2_families`
  passes. The bounded 64-fact family retention cap was not increased.
- The authorized operational query path uses bounded asynchronous execution
  and does not convert nested analyzer failure into a misleading available
  response.
- DeviceDrawer distinguishes acquisition attempt, retained observation time,
  current eligibility/staleness, positive-empty and partial/truncated evidence
  without a broad UI redesign.

## Exact verification evidence

### Focused consolidated regression

Log: `focused-2c002a3.log`

Command:

```sh
scripts/ci/build-lane.sh --dir /home/arjuna/.cache/bonsai-m6d-lane \
  --lbug-native /home/arjuna/.cache/bonsai-m6d-lbug-native -- \
  env CARGO_BUILD_JOBS=2 cargo test --profile ci --offline --lib \
  --test bn02404_native_interface_api --no-fail-fast
```

Result:

- compile: 8m 11s
- bonsai lib: 872 passed, 0 failed, 8 ignored in 112.04s
- `bn02404_native_interface_api`: 22 passed, 0 failed, 1 explicitly ignored
  live-gated test in 27.14s
- The ignored test is `operator_collects_live_frr_shadow_evidence_through_production_api`;
  it is not counted as a pass.

This focused run was for implementation SHA `2c002a3`. The subsequent SHA
`2a3b6be` changed only the Python capture helper compatibility function; the
complete canonical validation below was run against exact final SHA `2a3b6be`.

### Canonical L0–L3, final implementation SHA

Log: `canonical-l0-l3-2a3b6be.log`

Command:

```sh
/usr/bin/time -v scripts/ci/build-lane.sh \
  --dir /home/arjuna/.cache/bonsai-m6d-lane \
  --lbug-native /home/arjuna/.cache/bonsai-m6d-lbug-native -- \
  env CARGO_BUILD_JOBS=2 \
      PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome \
      GATE_BASE=2a8c29f6d89a35476b5b5aa24c3b413bd0eb46b1 \
      scripts/ci/gates.sh l0 l1 l2 l3
```

Result: `ALL REQUESTED GATES PASSED`

- L0: PASS
- L1: PASS (`cargo check`, UI production build, Python SDK import)
- L2: PASS
  - bonsai lib: 872 passed, 0 failed, 8 ignored in 104.38s
  - rbmp-core: 53 passed; rbmp regression target: 20 passed;
    rbmp-rib: 42 passed plus 17 passed/2 ignored in additional units;
    rbmp-store: 1 passed; `bn006_epoch_resync`: 2 passed; doc-tests passed
  - 35 integration targets passed, including
    `bn02404_native_interface_api`: 22 passed, 0 failed, 1 explicitly ignored
    live-gated test in 28.18s
- L3: PASS
  - cross-component Rust targets passed, including routing workflow, release
    gates, deployment/scope hardening, native NOS adapter, reconnect
    projection, inventory browser and topology browser acceptance
  - Python suites: 79 passed and 318 passed, 1 skipped, 1 deselected
- Wall time: 27m 32.27s
- Maximum resident set: 6,884,292 KiB
- Exact command output, warnings, test counts and timing are retained in the
  canonical log.

### Prior failed gate retained for audit

Log: `canonical-l0-l3-2c002a3.log`

The first canonical run for `2c002a3` passed L0/L1/L2 but failed L3 because
the Python capture helper no longer exposed the pre-existing `read_bounded`
compatibility API used by two bounded-stream contract tests. The helper was
restored in `2a3b6be`; no assertion was weakened. The final full gate above
was then rerun once against the exact corrected SHA.

### Environment record

`environment-preflight.txt` records repository/worktree state, disk, Rust
toolchain, sccache state and memory immediately after the final gate. Build
timing rows were appended to `docs/build-times/runs.csv`.

## Live qualification boundary

Live multifamily SSH acquisition and L4 remain **NOT_EXECUTED**.

Bounded assessment outcome: no already authorized accessible lab credential
plus pinned SSH host-key/trust anchor is available in this workspace for the
prior FRR target. The earlier ephemeral credential is unavailable. No
credential reset, device write, device modification or transport-verification
weakening was performed.

Required owner intervention if live qualification is desired: provide or
authorize creation of a lab credential and pinned host-key trust anchor for an
authorized FRR lab target, then permit execution of the read-only multifamily
capture. Offline integration and canonical L0–L3 success must not be
interpreted as live production qualification.

## MERGE_READY scope

PR #95 is MERGE_READY for the declared bounded shadow implementation:

- fixed-command, read-only, verified-SSH acquisition seam;
- standalone FRR/Linux exact artifact applicability;
- interface, ARP/ND, MAC forwarding and bridge VLAN shadow evidence;
- per-family lifecycle, positive-empty coverage, bounded retention and
  restart-safe transactional persistence;
- authorized operator query and conservative operational L2 assessment;
- no authoritative graph projection;
- no live production qualification claim.

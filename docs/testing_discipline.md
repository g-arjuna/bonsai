# Testing Discipline

Sprint 2 closes the token-burn gap by making verification cheap, structured, and reusable.

## The Three Layers

### 1. Wiring checks

Use wiring checks to catch dead-code and missing callsite regressions before a build.

- Script: `scripts/check_wiring.sh`
- Runtime cost: under 10 seconds
- Goal: fail fast when architecture exists in type-shape but not in runtime reality

Typical failures this layer should catch:

- registry exists but callsites still hardcode one implementation
- HTTP route documented but not actually registered
- parser or sidecar implementation exists but is not reachable from production code

### 2. Smoke tests

Use smoke tests for fast subsystem-level validation against a running Bonsai process.

- Directory: `scripts/smoke/`
- Runner: `scripts/smoke/run_all.sh`
- Output: `runtime/driver_results/smoke_<subsystem>.json`
- Target runtime: under 60 seconds per script

Current Sprint 2 smoke set:

- `smoke_synthesizer.sh`
- `smoke_change_detection.sh`
- `smoke_yang_library.sh`
- `smoke_output_adapters.sh`
- `smoke_servicenow_aiops.sh`
- `smoke_signals_syslog.sh`
- `smoke_signals_snmp.sh`

Smoke rules:

- every script must return exit `0` on pass and non-zero on failure
- every script must emit machine-readable JSON
- disabled or intentionally unavailable subsystems should report `skip`, not `fail`
- read-only validation is preferred unless the purpose of the smoke explicitly requires mutation

### 3. End-to-end validation

Use e2e runs for integration claims that depend on real external systems or multi-step workflows.

- Artefact location: `docs/test_results/...`
- Driver examples: output adapters, ServiceNow PDI, YANG sync against real repos
- Requirement: every integration claim needs a dated artefact, not only passing code

## Driver Results Contract

All smoke and driver outputs should be discoverable from:

- filesystem: `runtime/driver_results/*.json`
- API: `GET /api/_test/status`

Expected fields for smoke results:

```json
{
  "driver": "smoke_synthesizer",
  "ts_unix": 1714737600,
  "base_url": "http://127.0.0.1:3000",
  "status": "pass",
  "ok": true,
  "summary": "validated synthesizer endpoints",
  "checks": [
    {"check": "recommendations", "status": "pass"}
  ]
}
```

## Daily Use

Recommended local sequence before declaring subsystem work complete:

1. Run `scripts/check_wiring.sh`
2. Run the relevant smoke script in `scripts/smoke/`
3. Check `curl http://127.0.0.1:3000/api/_test/status`
4. Record or refresh the daily report with `scripts/bv5_daily_check.sh`

## Merge Rule

No new subsystem work should be considered complete unless:

- wiring checks pass
- at least one relevant smoke result exists
- the smoke result is visible through `/api/_test/status`
- external integration claims have a dated e2e artefact

# Gemini CLI Operational Brief

This is the canonical handoff brief for Gemini CLI when it is being used as Bonsai's operational test runner. It is intentionally concrete so a fresh session can execute the smoke and daily-check loop without rediscovering the environment.

## Scope

- Gemini owns operational verification and result capture.
- Codex/Claude owns code changes, architecture changes, and bug fixes.
- Both agents read the same artifacts under `runtime/driver_results/` and `docs/test_results/`.

## Stack Inventory

### Laptop / primary development environment

- Repo root: `/home/arjuna/Desktop/bonsai`
- Main Bonsai runtime on this machine: Docker Compose service `bonsai-core` from `docker-compose.yml`
- Default ContainerLab network on this machine: `bonsai-mgmt`
- Primary runtime config: repo-local `bonsai.toml`
- HTTP API: `http://127.0.0.1:3000`
- gRPC API: `http://[::1]:50051`
- Metrics endpoint: `http://127.0.0.1:9090/metrics` when enabled
- Runtime directory: `runtime/`
- Logs: `runtime/logs/` and any console output from the Windows launcher
- Archive: `runtime/archive/`
- Driver results: `runtime/driver_results/`
- Local graph DB: `bonsai.db`, `bonsai.db.wal`, and runtime-local graph artifacts

### Cloud spike environment

- Install root: `/opt/bonsai`
- Archive mount: `/mnt/bonsai-archive`
- Cloud deploy script: `scripts/cloud/deploy.sh`
- Cloud daily sync: `scripts/cloud/daily_sync.sh`
- Cloud config: `/opt/bonsai/bonsai.toml`
- Cloud HTTP API: `http://<cloud-vm>:3000`
- Cloud archive path: `/mnt/bonsai-archive/archive`
- Cloud snapshots: `/mnt/bonsai-archive/snapshots`

### Labs

- DC lab: `lab/dc/dc-evpn-srv6.clab.yml`
- SP lab: `lab/sp/sp-mpls-srte.clab.yml`
- Cloud DC lab: `lab/cloud-dc-6node.yml`
- Fast iteration lab: `lab/fast-iteration/multivendor.clab.yml`
- Lab health check: `scripts/check_lab.sh --topology dc|sp|all`
- Ubuntu laptop default: use the distributed core/collector path on `bonsai-mgmt`; external services stay constant while the active ContainerLab topology changes.

Use credentials only from local `bonsai.toml`, the credential vault, or environment variables. Do not copy credentials into result docs or commit them anywhere.

### External infrastructure

- External infra compose file: `docker/compose-external.yml`
- NetBox API/UI: `http://localhost:8000`
- Splunk Web: `http://localhost:8100`
- Splunk HEC: `https://localhost:8088/services/collector`
- Elasticsearch: `http://localhost:9200`
- Kibana: `http://localhost:5601`
- Prometheus: `http://localhost:9093`
- Grafana: `http://localhost:3001`
- ServiceNow PDI: `SNOW_INSTANCE_URL`, `SNOW_USERNAME`, `SNOW_PASSWORD`

## Test Commands And Ownership

### Smoke tests

- `scripts/smoke/run_all.sh`
- `scripts/smoke/smoke_synthesizer.sh`
- `scripts/smoke/smoke_change_detection.sh`
- `scripts/smoke/smoke_yang_library.sh`
- `scripts/smoke/smoke_output_adapters.sh`
- `scripts/smoke/smoke_servicenow_aiops.sh`
- `scripts/smoke/smoke_signals_syslog.sh`
- `scripts/smoke/smoke_signals_snmp.sh`

Expected output:

- Per-smoke JSON: `runtime/driver_results/smoke_<subsystem>.json`
- Aggregate visibility: `GET /api/_test/status`

### Daily check

- `scripts/bv5_daily_check.sh`
- Markdown artifact: `docs/test_results/daily_runs/<YYYY-MM-DD>.md`
- Structured artifact: `runtime/driver_results/daily.json`
- Cloud sync verification: `scripts/cloud/daily_sync_check.sh`
- Cloud sync artifact: `runtime/driver_results/cloud_sync_check.json`

### End-to-end tests

- `scripts/sprint5_preflight.sh --check`
- `scripts/e2e_output_adapters_test.sh --adapter prometheus`
- `scripts/e2e_output_adapters_test.sh --adapter splunk`
- `scripts/e2e_output_adapters_test.sh --adapter elastic`
- `scripts/e2e_servicenow_pdi_test.sh`
- `scripts/e2e_compose_test.sh`
- `scripts/e2e_containerlab_test.sh`
- `scripts/e2e_netbox_enricher_test.sh`
- `scripts/e2e_path_validation_test.sh`

Expected output:

- Dated markdown results under `docs/test_results/`
- Temporary logs usually under `/tmp/`

Adapter restart note:

- On this machine, adapter E2E runs should restart Docker Compose service `bonsai-core` by default, not a Windows, WSL, or ad-hoc host process.

### Chaos runner operational checks

- `bash scripts/chaos_runner.sh --status`
- `bash scripts/chaos_runner.sh --ensure-running`
- `bash scripts/chaos_runner.sh --stop`

Primary plan:

- `chaos_plans/always_on_dc.yaml`

## Result Locations

- Smoke JSON: `runtime/driver_results/smoke_<subsystem>.json`
- Daily JSON: `runtime/driver_results/daily.json`
- Daily markdown: `docs/test_results/daily_runs/<YYYY-MM-DD>.md`
- Output-adapter e2e: `docs/test_results/e2e_output_adapters/`
- Cloud spike reports: `docs/test_results/cloud_spike/`
- Chaos truth: `runtime/chaos_log.jsonl` and `chaos_runs/*/injections.csv`

## Branch Policy For Gemini Test Runs

- Preferred branch for recurring test commits: `test-results/gemini`
- Acceptable per-run branch: `gemini/daily-<YYYY-MM-DD>`
- Code branches and test-result branches should stay separate.
- Commit only generated test artifacts, not source changes.
- If no commit is needed, Gemini may still write local artifacts and stop there.

Suggested commit subjects:

- `test: daily check 2026-05-11`
- `test: smoke suite 2026-05-11`
- `test: splunk e2e 2026-05-11`

## Result Format Contract

Smoke and daily results must be machine-readable first and human-readable second.

### Smoke result schema

```json
{
  "driver": "smoke_synthesizer",
  "ts_unix": 1746823200,
  "base_url": "http://127.0.0.1:3000",
  "status": "pass",
  "ok": true,
  "summary": "validated readiness and synthesizer recommendation endpoints for 172.100.103.15:57400",
  "checks": [
    {
      "name": "gnmi_readiness",
      "check": "gnmi_readiness",
      "status": "pass",
      "ok": true
    }
  ],
  "environment": {
    "bonsai_version": "v23-cv2",
    "git_sha": "abc1234",
    "lab": "lab/dc/dc-evpn-srv6.clab.yml"
  }
}
```

### Daily result schema

```json
{
  "driver": "daily_check",
  "ts_unix": 1746910000,
  "base_url": "http://127.0.0.1:3000",
  "status": "pass",
  "ok": true,
  "summary": "daily check complete; archive=pass driver_results=pass chaos=pass lab=pass",
  "checks": [
    {"name": "bonsai_status", "status": "pass", "ok": true},
    {"name": "driver_results", "status": "pass", "ok": true},
    {"name": "archive_verification", "status": "pass", "ok": true},
    {"name": "chaos_runner_status", "status": "pass", "ok": true},
    {"name": "lab_health", "status": "pass", "ok": true}
  ],
  "artifacts": {
    "markdown_report": "docs/test_results/daily_runs/2026-05-11.md"
  }
}
```

Rules:

- `status` is one of `pass`, `fail`, or `skip`.
- `ok` is `true` only when the top-level result is a pass.
- Each check entry should include `name`, `status`, and `ok`.
- Include short summaries with enough detail for a follow-on coding agent to triage without re-running the test immediately.
- Never include secrets in any JSON or markdown artifact.

## Failure Decision Tree

- Smoke fails: write/update `runtime/driver_results/smoke_<name>.json`, record the command, and stop short of code changes.
- Daily check fails: write `runtime/driver_results/daily.json`, write the markdown report, and highlight the failing subsystem.
- E2E fails: capture the exact command, error text, environment state, and relevant log excerpts under `docs/test_results/...`.
- Lab unhealthy: run `scripts/check_lab.sh`, record the degraded nodes or sessions, and only attempt documented restarts.
- Chaos runner inactive: run `bash scripts/chaos_runner.sh --ensure-running`, then record whether a restart was needed.

## What Gemini Must Not Do

- Modify files under `src/`, `python/`, or `ui/`
- Change runtime config unless explicitly asked
- Rotate or rewrite chaos plans during a routine verification run
- Commit credentials, tokens, passwords, or copied config secrets
- Claim a subsystem is validated without a corresponding artifact

## Pre-Task Handoff

Use [docs/gemini_task_template.md](/home/arjuna/Desktop/bonsai/docs/gemini_task_template.md) for task-specific instructions.

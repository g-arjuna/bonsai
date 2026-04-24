# Distributed Validation

This note captures the T2-4 live validation for Bonsai's distributed
collector/core runtime. It is intentionally safe to share: generated configs,
certificates, and copied lab credentials stay under ignored `runtime/`
directories and are not reproduced here.

## 2026-04-22 Live Lab Run

Environment:

- Bonsai core/API ran on Windows from `target/release/bonsai.exe`.
- Bonsai collector ran as a second Windows process from the same binary.
- ContainerLab and the lab devices ran in WSL.
- The collector connected directly from Windows to the WSL-backed lab gNMI
  endpoints on port `57400`.
- The collector/core ingest endpoint used mTLS and tonic zstd compression.

Run directory:

```text
runtime/distributed-validation/run-20260422-102045
```

Lab targets reached by the collector:

- `172.100.102.11:57400`
- `172.100.102.12:57400`
- `172.100.102.13:57400`
- `172.100.102.21:57400`

Validation sequence:

1. Generated a short-lived lab CA, core server certificate, and collector
   client certificate under the run directory.
2. Started collector mode first with the core intentionally offline.
3. Confirmed live telemetry continued to arrive from all four lab targets and
   accumulated in the disk-backed collector queue.
4. Started core mode on `127.0.0.1:50061` with mTLS enabled.
5. Confirmed the collector connected with `compression="zstd"` and
   `mtls=true`.
6. Confirmed the collector replayed queued records and continued forwarding
   live telemetry.
7. Confirmed the core accepted ingest updates and wrote graph interface data
   for SR Linux and IOS-XRd targets.
8. Ran a forced wrong-client-certificate smoke test against the same core.

Observed results:

| Check | Result |
| --- | --- |
| Lab gNMI reachability | 4/4 targets reachable from Windows |
| Peak outage queue | 1,474 records / 346,991 bytes before replay |
| Final queue file | 1,046 bytes after replay/compaction |
| Delivered batches | 421 |
| Delivered records | 3,314 |
| Core accept events | 422 |
| zstd estimate | 92.93% reduction, 14.14:1 ratio over 1,000 sampled messages |
| Valid mTLS collector | Connected and delivered records |
| Wrong-CA collector | 0 delivered batches, 0 core accept events, queue grew to 288,166 bytes |

Important log evidence:

- Collector outage behavior: repeated `collector forwarder disconnected` while
  core was down, with queue status increasing.
- Collector replay behavior: `collector forwarder connected to core` followed
  by `collector queue batch delivered`.
- Compression: `collector ingest compression estimate` reported zstd reduction.
- Core ingest: core logged `gRPC API and telemetry ingest server listening`
  with `ingest_compression="zstd"` and `mtls=true`.
- Graph writes: core logged `interface written` for SR Linux targets and the
  IOS-XRd target.
- Bad cert smoke: bad collector logged `core telemetry ingest stream failed`;
  the core logged no accepted telemetry for `validation-bad-cert`.

## Reproducibility Notes

Use Windows for Bonsai processes and WSL for lab/containerlab:

- Windows owns Rust builds, Bonsai core, Bonsai collector, API, metrics, and UI.
- WSL owns ContainerLab and lab-side network devices.
- If a sandboxed shell cannot reach `172.100.102.x:57400`, rerun the validation
  from normal/elevated Windows PowerShell. The user-visible Windows
  `Test-NetConnection` result is the source of truth for direct host reachability.

Before running:

```powershell
cargo build --release
Test-NetConnection 172.100.102.11 -Port 57400
Test-NetConnection 172.100.102.12 -Port 57400
Test-NetConnection 172.100.102.13 -Port 57400
Test-NetConnection 172.100.102.21 -Port 57400
```

Generate isolated runtime configs under `runtime/distributed-validation/<run>/`
instead of editing the real `bonsai.toml`. The core config should use
`runtime.mode = "core"`, a loopback `api_addr`, and `[runtime.tls]` server
identity. The collector config should use `runtime.mode = "collector"`, the
same core endpoint as `https://127.0.0.1:<port>`, `[runtime.tls]` client
identity, and a run-local `[collector.queue]` path.

The negative mTLS smoke must force an actual ingest RPC. A collector with no
targets or no queued records may only create a lazy channel and is not enough to
prove rejection. Use real targets or a pre-populated queue so the bad collector
attempts `TelemetryIngest`.

## Scope Boundary

This run validates the distributed transport slice: live lab telemetry,
disk-backed outage queue, replay, zstd compression, mTLS, and graph ingestion.

It does not validate the remediation/healing loop or archive parity. Those are
separate backlog validations because they exercise different subsystems.

---

# Sprint 2 Multi-Collector Validation Scenarios

Five scenarios to validate the integrated collector-core architecture (T1-3 v6).
Each scenario describes the setup, reproduction steps, expected outcome, and
status. Failures become specific follow-up items.

## Prerequisites

- Two-collector compose profile running with mTLS (after T0-2 is done):
  ```bash
  scripts/generate_compose_tls.sh
  cp .env.example .env  # fill in BONSAI_VAULT_PASSPHRASE
  scripts/seed_lab_creds.sh
  docker compose --profile two-collector up -d
  ```
- At least three lab devices added across two sites, e.g.:
  - `srl-spine1` and `srl-leaf1` → site `dc-lab`, role `spine`/`leaf`
  - `srl-leaf2` and `xrd-pe1` → site `dc-london`, role `leaf`/`pe`
- Assignment rules configured:
  ```
  POST /api/assignment/rules
  { "rules": [
      {"match_site": "dc-lab",    "collector_id": "collector-1", "priority": 10},
      {"match_site": "dc-london", "collector_id": "collector-2", "priority": 10}
  ]}
  ```

---

## Scenario 1 — Route-driven automatic assignment

**What**: Devices added with no `collector_id` are automatically assigned by routing rules.

**Steps**:
1. Remove any explicit `collector_id` from all devices via `PATCH /api/onboarding/devices`.
2. `GET /api/assignment/status` — all devices should appear under `unassigned_devices`.
3. `POST /api/assignment/rules` with the site-to-collector mapping above.
4. `GET /api/assignment/status` — unassigned list should now be empty.
5. `GET /api/onboarding/devices` — verify each device has the expected `collector_id`.
6. SSH into `bonsai-collector-1` container logs — confirm subscriptions for dc-lab devices only.
7. SSH into `bonsai-collector-2` container logs — confirm subscriptions for dc-london devices only.

**Expected**: Devices land on the correct collector based purely on their `site` field. No manual `collector_id` assignment needed.

**Status**: Not yet run. Pending lab availability.

---

## Scenario 2 — Collector crash → device transition to unassigned → recovery

**What**: When a collector disconnects, its devices become unassigned (or re-evaluated via rules). When the collector restarts, devices are re-assigned via the registration full-sync.

**Steps**:
1. Start with Scenario 1 in a healthy state.
2. `docker compose --profile two-collector stop bonsai-collector-1`
3. Within 30 seconds, `GET /api/assignment/status` — `collector-1` should no longer appear in `active_collectors`.
4. `GET /api/onboarding/devices` — dc-lab devices should have `collector_id = null` OR have been re-assigned to `collector-2` if a rule matches.
5. Check `bonsai-collector-2` logs — confirm no spurious subscriptions for dc-lab devices (collector-2 only takes them if rules assign them there).
6. `docker compose --profile two-collector start bonsai-collector-1`
7. After reconnection, `GET /api/assignment/status` — `collector-1` reappears in `active_collectors`.
8. `GET /api/onboarding/devices` — dc-lab devices re-assigned to `collector-1`.
9. Verify telemetry from dc-lab devices resumes in core graph.

**Expected**: Collector crash produces a clean unassign/re-assign cycle with no lost devices. Telemetry gap is bounded by the reconnect delay.

**Status**: Not yet run.

---

## Scenario 3 — Core unreachable → collector queues → drain on reconnect

**What**: Collector accumulates telemetry in its disk queue when core is unreachable, then drains on reconnect with no data loss.

**Steps**:
1. `docker compose --profile two-collector stop bonsai-core`
2. Wait 60 seconds — collector logs should show repeated `collector forwarder disconnected` + queue growth.
3. `docker exec bonsai-collector-1 cat /app/runtime/collector-queue/queue.ack` — note the growing file.
4. `docker compose --profile two-collector start bonsai-core`
5. Watch collector logs — expect `collector forwarder connected to core` then `collector queue batch delivered`.
6. `GET /api/readiness` on core — verify graph events are present and timestamps span the outage window.
7. `docker exec bonsai-collector-1 /app/tls/...` (or hit the diagnostic endpoint if T1-2 enabled): `GET http://bonsai-collector-1:9091/api/collector/status` — queue depth should be 0.

**Expected**: No telemetry lost. Queue depth returns to 0. Core event timestamps show the full outage window covered.

**Status**: Partially covered by the 2026-04-22 run above (single collector). Needs re-run with two-collector compose.

---

## Scenario 4 — Credential rotation

**What**: Operator rotates the `lab-admin` vault alias; new credentials are delivered to the collector on the next assignment update without service interruption.

**Steps**:
1. `scripts/seed_lab_creds.sh` — update `lab-admin` with new password (use a test device with a known-good new password).
2. `GET /api/credentials` — confirm `lab-admin` shows a new `updated_at` timestamp.
3. Trigger a re-assignment by modifying and re-saving one dc-lab device (e.g. toggle `enabled` twice).
4. Check `bonsai-collector-1` logs — expect `assignment update received` with the new credentials.
5. Verify the collector continues receiving telemetry from the affected device (no subscription drop).

**Expected**: Credential rotation is non-disruptive. gNMI connection re-establishes automatically with new credentials within one assignment cycle.

**Status**: Not yet run. Requires credential rotation to be wired through `CollectorManager.unregister_collector` re-evaluation path.

---

## Scenario 5 — Site reassignment (device moves between collectors)

**What**: Moving a device from one site to another triggers a collector switch: the old collector unsubscribes, the new collector subscribes, data continuity is verifiable.

**Steps**:
1. Start with Scenario 1 in a healthy state. `srl-leaf2` is in `dc-london`, assigned to `collector-2`.
2. `PATCH /api/onboarding/devices` — change `srl-leaf2` site to `dc-lab`.
3. `GET /api/assignment/status` — `srl-leaf2` should now show under `collector-1`.
4. Check `bonsai-collector-2` logs — `subscriber stopped` for `srl-leaf2`'s address.
5. Check `bonsai-collector-1` logs — `subscriber started` for `srl-leaf2`'s address.
6. `GET /api/topology` — `srl-leaf2` still visible in graph; interface state not stale.

**Expected**: Automatic reassignment fires within seconds of the site change. Data continuity maintained — no duplicate events, no gap wider than the assignment propagation delay.

**Status**: Not yet run. Depends on T1-1 site-change re-evaluation path.

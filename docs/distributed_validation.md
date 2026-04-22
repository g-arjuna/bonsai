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

# eBPF Scoping Document — Bonsai DV1 D4-T2

*Authored: 2026-05-16. Timeboxed 1-week spike. References BONSAI_CONSOLIDATED_BACKLOG_DV1.md §D4-T2.*

---

## 1. What eBPF Unlocks That gNMI Doesn't

gNMI subscriptions are vendor-implemented on the network device. They are rich in protocol state (BGP RIB, BFD session, interface counters at 10–30s granularity) but have three hard limits:

| Gap | gNMI limitation | eBPF solution |
|---|---|---|
| **Sub-second interface counters** | Nokia SRL: minimum 1s sample interval; most vendors 10–30s | eBPF perf-buffer from `net_dev_queue_xmit` gives per-packet precision |
| **Kernel-layer drop counters** | Not exposed via gNMI on any vendor | `kfree_skb` tracepoint exposes drop reason at netdev layer |
| **TCP connection state transitions** | gNMI streams don't reach the kernel TCP stack of the bonsai host itself | `tcp_state_change` kprobe on bonsai host monitors internal connections (e.g., gRPC subscriber reconnects) |
| **Scheduler / latency jitter** | Not available | `sched_switch` tracepoint exposes runqueue latency on the bonsai host, useful for detecting overload |

The primary value proposition in bonsai's context: **drop counters and sub-second interface telemetry** for the bonsai host's own NICs, providing a ground-truth check against gNMI-reported counters from the managed devices.

---

## 2. Tooling Landscape (2026)

| Tool | Language | Maturity | Notes |
|---|---|---|---|
| **libbpf** | C | Production | Canonical CO-RE approach; portable across kernel versions |
| **aya** | Rust | 1.0 (2025) | Rust-native, no LLVM dep at runtime; used in Cilium Rust components |
| **bcc** | Python/C | Mature | Development/scripting; not suitable for production embedding |
| **Cilium ebpf-go** | Go | Production | Go-native; not relevant for Rust codebase |

**Recommendation for bonsai**: `aya` (Rust). Reasons:
- Bonsai is a Rust binary; eBPF programs and userspace loader share the same `Cargo.toml`.
- `aya` handles CO-RE (Compile Once – Run Everywhere) via `btf` — programs compiled against one kernel run on others with matching BTF.
- No `libbpf-sys` C dependency chain in production builds; `aya` is pure Rust for the loader.
- `aya-bpf` crate provides `#[map]`, `#[kprobe]`, `#[tracepoint]` proc-macros that integrate cleanly.

---

## 3. Resource Footprint

Based on published benchmarks (Cilium 2025, Pixie 2024) and spike measurements:

| Resource | Typical value | Bonsai-specific |
|---|---|---|
| **Kernel verifier time** | 1–50 ms per program load | Acceptable at startup; not on hot path |
| **eBPF map memory** | 1–4 MB per map (configurable) | Single `PerfEventArray` for drop counters: ~256 KB |
| **CPU overhead** | 0.5–2% per core for high-frequency tracepoints | `kfree_skb` fires only on drops; near-zero in nominal operation |
| **Ring buffer** | 64 KB default; tunable | Sufficient for drop events at any realistic rate |

Verdict: **negligible footprint** for the target use case (drop counter collection at drop-time, not polling).

---

## 4. Integration Surface

Two integration options:

### Option A — Standalone collector kind (recommended for DV1/DV2)

Add `CollectorKind::Ebpf` alongside the existing `CollectorKind::Gnmi`. The eBPF collector:
- Loads on startup if `[ebpf] enabled = true` in `bonsai.toml`
- Publishes `TelemetryEvent::EbpfDropCounter { if_name, drop_reason, count }` onto `InProcessBus`
- Bonsai processes it identically to gNMI telemetry

Changes required:
- `src/ingest.rs`: add `EbpfCollector` struct implementing the collector trait
- `src/config.rs`: add `[ebpf]` section
- `src/lib.rs`: expose `ebpf` feature flag
- New crate member: `experiments/ebpf_spike_20260516/` (proof-of-concept first, then promote to `src/ebpf/`)

### Option B — Telemetry enrichment (DV3+)

Feed eBPF drop counters as enrichment signals on existing device nodes in the graph. This requires the archive to have accumulated enough history to make enrichment meaningful. Defer to DV3.

---

## 5. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| **Kernel version drift** | Medium | CO-RE + BTF support required (Linux ≥5.8); lab uses Ubuntu 22.04 (5.15) and 24.04 (6.8) — both supported |
| **GPL license constraint** | Low | eBPF programs must be GPL-licensed; bonsai's loader (`aya` userspace) is MIT-licensed. The GPL applies only to the eBPF bytecode object, not the loader. Standard practice. |
| **Verifier complexity limit** | Low | Drop counter probe is <20 instructions; well within the 1M instruction limit |
| **Capability requirement** | Medium | Requires `CAP_BPF` (Linux ≥5.8) or `CAP_SYS_ADMIN`. Docker/K8s needs `securityContext.capabilities.add: [BPF]`. Document in Helm chart. |
| **Mac dev environment** | Informational | eBPF is Linux-only. Mac builds compile the loader but skip the eBPF program load. `cfg(target_os = "linux")` gates the actual load. Already used in `preflight_disk_check`. |

---

## 6. Proof-of-Concept — Interface Drop Counter

Located at `experiments/ebpf_spike_20260516/`. The proof:

1. **eBPF program** (`src/drop_counter.bpf.c` or `src/drop_counter.rs` with `aya-bpf`): attaches to `kfree_skb` tracepoint, extracts interface index and drop reason, increments a per-`(ifindex, reason)` `HashMap` map.
2. **Userspace loader** (`src/main.rs`): loads the program, polls the map every 5s, prints per-interface drop counts to stdout.
3. **Integration stub** (`src/bonsai_bridge.rs`): converts map entries into `TelemetryEvent::EbpfDropCounter` values and sends them on a `tokio::sync::mpsc` channel — matching the interface bonsai's `InProcessBus` expects.

See `experiments/ebpf_spike_20260516/README.md` for build instructions (Ubuntu only).

---

## 7. Recommendation for DV2

**Adopt, scoped to drop counters and host-NIC telemetry.**

Rationale:
- The gap it fills (sub-second drop counters) is real and not addressable via gNMI.
- The implementation risk is low (well-understood tracepoint, minimal verifier complexity).
- The `aya` toolchain integrates naturally with the Rust codebase.
- The GPL constraint is standard and non-blocking.

**Scope for DV2**: promote `experiments/ebpf_spike_20260516/` into `src/ebpf/`. Add `CollectorKind::Ebpf`. Wire into `InProcessBus`. Add `CAP_BPF` to Helm chart security context. Estimated: 3 days.

**Defer to DV3**: enrichment integration (Option B), cross-kernel-version CI, sched_switch jitter detection.

---

*References: Cilium eBPF docs (2025), aya-rs/aya GitHub (v0.13, 2025), Pixie perf benchmarks (2024), Linux kernel docs kfree_skb tracepoint, arxiv 2603.09675 (TSAD eval framework — unrelated but referenced in D5).*

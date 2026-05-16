# eBPF Spike — Interface Drop Counter Proof-of-Concept

*DV1 D4-T2 spike. Linux (Ubuntu ≥22.04) only. See `docs/research/ebpf_scoping_20260516.md` for full analysis.*

## What This Does

Attaches to the kernel `kfree_skb` tracepoint. For every dropped packet, records `(interface_index, drop_reason)` in a BPF HashMap. Userspace polls the map every 5 seconds and prints per-interface drop counts. An integration stub formats events for bonsai's `InProcessBus`.

## Prerequisites (Ubuntu)

```bash
# Kernel BTF support (required for CO-RE)
ls /sys/kernel/btf/vmlinux   # must exist; present on Ubuntu 22.04+

# Rust + aya toolchain
rustup toolchain install stable
rustup target add bpfel-unknown-none   # eBPF target
cargo install bpf-linker               # links eBPF programs

# bpftool (for manual inspection, optional)
sudo apt-get install -y linux-tools-$(uname -r)
```

## Build and Run

```bash
# From this directory (Ubuntu only)
cargo build --release 2>&1

# Run (requires CAP_BPF or sudo)
sudo ./target/release/ebpf_spike_drop_counter

# Expected output (every 5s):
# [eth0] drops: 0  (reason: not_specified=0)
# [eth1] drops: 3  (reason: no_socket=2, ip_csum=1)
```

## Structure

```
Cargo.toml              workspace with two members: loader + ebpf-program
src/
  main.rs               userspace loader (aya, tokio)
  bonsai_bridge.rs      converts map events → TelemetryEvent stub
ebpf-program/
  Cargo.toml            #![no_std] crate for the eBPF bytecode
  src/
    main.rs             kfree_skb tracepoint handler (aya-bpf)
```

## Capability Note

Loading eBPF programs requires `CAP_BPF` (Linux ≥5.8). In Kubernetes:
```yaml
securityContext:
  capabilities:
    add: ["BPF"]
```
The Helm chart at `deploy/helm/bonsai/` has a commented-out block for this.

## Mac

The loader compiles on Mac but will not load (Linux kernel required). The `bonsai_bridge.rs` stub compiles on all platforms — it's gated with `#[cfg(target_os = "linux")]` on the actual load call.

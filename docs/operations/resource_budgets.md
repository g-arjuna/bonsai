# Resource Budgets — CV5 T4-2

Per-environment resource declarations and their mapping to bonsai resource profiles.
Read alongside `docs/resource_profiles.md` (governor loop mechanics and metric reference).

## Environment Declarations

### Laptop (primary development)

| Resource | Allocation | Notes |
|----------|-----------|-------|
| RAM | 16–32 GB (variable) | Shared with WSL2, Docker, IDE. Effective headroom ~8–12 GB. |
| Storage | 30 GB for bonsai runtime | `runtime/` on WSL2 filesystem |
| CPU | 6–8 cores (variable) | Shared with Windows host |
| Network | Local only | No egress billing |
| Lab overhead | ~8 GB (8× SR Linux @1 GB) | DC lab; leaves ~4 GB for bonsai stack |

**Expected profile at runtime**: `Medium` (6–13 GB RAM bracket) or `Large` (14–29 GB).

```
Medium: memory_budget=1 GB, lru_cache=256 MB, batch=256, bus=8192, rate=10K eps
Large:  memory_budget=2 GB, lru_cache=512 MB, batch=512, bus=16384, rate=50K eps
```

**Note**: If the laptop reports `Small` (2–5 GB effective RAM after WSL overhead),
the governor will restrict bonsai to 512 MB RSS and 2K eps. This is tight for an 8-node
lab — consider closing background applications or setting `profile = "medium"` manually
in bonsai.toml.

---

### OCI Always Free ARM (cloud primary)

| Resource | Allocation | Notes |
|----------|-----------|-------|
| RAM | 24 GB | Fixed — OCI Always Free ARM shape (VM.Standard.A1.Flex 4×OCPU) |
| Storage | 200 GB block volume | OCI default; runtime uses ~10 GB for archive growth |
| CPU | 4 vCPU (ARM Ampere) | Shared with lab nodes |
| Network | 10 TB/month egress free | Within always-free limit for bonsai's usage |
| Lab overhead | ~12 GB (6× SR Linux @2 GB cap) | `memory: 2048m` per node in cloud-dc-6node.yml |

**Effective bonsai headroom**: ~9 GB (24 GB − 12 GB lab − 3 GB Docker/OS overhead)

**Expected profile at runtime**: `Medium` (6–13 GB bracket, ~9 GB headroom).

```
Medium: memory_budget=1 GB, lru_cache=256 MB, batch=256, bus=8192, rate=10K eps
```

When SP lab replaces DC on the cloud (9 nodes: 5 SRL @2 GB + 2 FRR @0.5 GB + 2 FRR @0.5 GB),
lab overhead rises to ~12 GB — same Medium profile applies.

---

### Second Cloud (if obtained)

| Resource | Allocation | Notes |
|----------|-----------|-------|
| RAM | 8–16 GB (TBD) | GCP/AWS/Azure Always Free or trial |
| Storage | 30–100 GB (TBD) | Platform dependent |
| CPU | 2–4 vCPU | Platform dependent |

**Expected profile**: `Small` (2–5 GB) or `Medium` (6–13 GB) depending on platform.

---

## Profile Quick Reference

Derived from `src/resource_profile.rs` — these are the actual compiled values:

| Profile | RAM bracket | memory_budget | lru_cache | batch_size | bus_capacity | rate_budget |
|---------|-------------|--------------|-----------|------------|-------------|-------------|
| Tiny | 0–1 GB | 256 MB | 64 MB | 64 | 2048 | 500 eps |
| Small | 2–5 GB | 512 MB | 128 MB | 128 | 4096 | 2K eps |
| Medium | 6–13 GB | 1 GB | 256 MB | 256 | 8192 | 10K eps |
| Large | 14–29 GB | 2 GB | 512 MB | 512 | 16384 | 50K eps |
| XLarge | 30 GB+ | 4 GB | 1 GB | 1024 | 32768 | 200K eps |

### Boundary behavior (C4-N4 finding)

`from_ram()` floors at GB boundaries. VMs near a boundary get the lower profile:

| Effective RAM | Profile assigned | Concern |
|--------------|-----------------|---------|
| 1.9 GB | Tiny (256 MB budget) | Tight for any lab |
| 2.0 GB | Small (512 MB budget) | — |
| 5.9 GB | Small (512 MB budget) | Under-provisioned for 6-node lab |
| 6.0 GB | Medium (1 GB budget) | — |
| 13.9 GB | Medium (1 GB budget) | Under-provisioned for large labs |
| 14.0 GB | Large (2 GB budget) | — |

**Recommendation**: if your VM reports RAM within 200 MB of a boundary (e.g., Docker reports
5.8 GB available), add `profile = "medium"` to bonsai.toml to override the auto-detected
profile:

```toml
[storage]
profile = "medium"   # override auto-detection when near a GB boundary
```

---

## Storage Budget

| Directory | Laptop | OCI cloud | Notes |
|-----------|--------|-----------|-------|
| `runtime/archive/` | ~100 MB/day | ~80 MB/day | Parquet files; 60-min rotation (T1-3 CV4) |
| `runtime/logs/` | ~5 MB/day | ~5 MB/day | Daily check + chaos runner logs |
| `runtime/driver_results/` | ~1 MB/day | ~1 MB/day | JSON artefacts |
| `bonsai.db` + `.wal` | ~500 MB steady-state | ~300 MB steady-state | LadybugDB graph |
| Docker images | ~10 GB | ~15 GB | SR Linux + FRR + sidecar images |

**30-day archive at current chaos cadence**: ~3 GB laptop, ~2.5 GB cloud.

GNN training trigger condition (30 days + 500 injections) → ~3–6 GB total archive across
both environments. Well within storage budgets.

---

## Checking Your Environment

```bash
# See what profile bonsai detected at startup
curl -s http://127.0.0.1:3000/api/governance/state | python3 -m json.tool | grep -E "profile|memory_budget"

# Check current RSS vs budget
curl -s http://127.0.0.1:3000/metrics | grep bonsai_rss_bytes

# Check governor action flags
curl -s http://127.0.0.1:3000/api/governance/state | python3 -m json.tool
```

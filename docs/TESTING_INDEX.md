# Bonsai Testing Guide Index

This document provides an overview of all Bonsai testing guides and helps you select the appropriate test suite for your needs.

## Testing Guides by Scope

### 0. EV1 ML Pipeline Testing (EV1_UBUNTU_TESTING_GUIDE.md)
**Location**: `docs/EV1_UBUNTU_TESTING_GUIDE.md`

**Purpose**: End-to-end validation of EV1 ML Intelligence features — STGNN, Parquet pipeline, semantic embeddings, BonPy MLOps UI, and rule management.

**Scope**:
- Device onboarding: PyATS/Genie automated + manual API methods
- gNMI, syslog, SNMP multi-source correlation baseline
- Python sidecar startup, graceful shutdown, health/Prometheus endpoints
- ML job engine (APScheduler): schedule management, manual triggers, cancel, SSE progress
- Parquet export pipeline: incremental export, quality gate, validator, catalog
- STGNN training: NCT pretrain → supervised finetune → conformal calibration → model registry
- STGNN live inference: snapshot buffer, GNN inference write-back, attention weights to graph
- Semantic embeddings: syslog + config embedding workers, similarity search, clustering
- BonPy MLOps console: all 8 routes — Dashboard, Jobs, Models, Exports, GNN, Embeddings, Rules, Detections
- DB-backed rule management: enable/disable, parameter overrides, shadow mode, syslog rule from UI
- Playbook DB migration, creation, and execution tracking
- Change management integration: change-correlated detections, CW-GNN validation
- End-to-end ML fault cycle: fault → detection → GNN anomaly → investigation → remediation
- Final validation scorecard (automated pass/fail script)

**Prerequisites**:
- Ubuntu host with Docker 24+, ContainerLab ≥0.54, Python 3.12+
- Python ML deps: `sentence-transformers torch torch-geometric apscheduler pyarrow scikit-learn hdbscan`
- cargo build --release + ui builds (including ui-bonpy)

**Source parts** (auto-merged):
- `ev1/ev1-testing-part1.md` — Infra, onboarding, core signals (Phases 0–8)
- `ev1/ev1-testing-part2.md` — ML pipeline, GNN, embeddings (Phases 9–16)
- `ev1/ev1-testing-part3.md` — BonPy UI, rules, E2E (Phases 17–22)

**Regenerate**: `python3 ev1/merge_ev1_testing.py`

**Run when**:
- After any EV1 ML code changes
- Before Ubuntu ops handoff
- For full regression testing including ML pipeline

---

### 1. Signal Receiver Testing (UBUNTU_TESTING_GUIDE.md)
**Location**: `lab/signal-test-lab/UBUNTU_TESTING_GUIDE.md`

**Purpose**: End-to-end validation of signal receivers and multi-source correlation in the signal-test-lab ContainerLab topology.

**Scope**:
- gNMI telemetry (interface counters, BGP state, IS-IS adjacencies)
- Syslog receiver (UDP from SRL nodes)
- SNMP trap receiver (v1/v2c/v3 from SRL nodes)
- BMP receiver (BGP monitoring from FRR router)
- NetFlow/IPFIX receiver (v5/v9 from linux-host1)
- OTLP HTTP receiver (OpenTelemetry traces)
- Multi-source correlation (gNMI + syslog + SNMP + BMP)
- HostEndpoint discovery via LLDP
- Settings API (dynamic receiver configuration)
- Collector health monitoring
- Fault injection round-trip (detections → incidents)

**Prerequisites**:
- Ubuntu host with Docker and ContainerLab
- cmake and build tools for Rust compilation
- ContainerLab topology deployment

**Topology**: 8 nodes (7 Nokia SRL + 1 Linux) in `lab/signal-test-lab/signal-test.clab.yml`

**Run when**:
- After implementing new receiver types
- After changes to signal parsing logic
- After multi-source correlation changes
- Before releasing new signal ingestion features

---

### 2. HA (High Availability) Testing (HA_TESTING.md)
**Location**: `docs/HA_TESTING.md`

**Purpose**: Validation of distributed HA cluster behavior with etcd-based leader election and config replication.

**Scope**:
- etcd-based leader election
- Config replication via etcd
- Leader failover and re-election
- Collector re-homing on leader change
- Leader-aware routing (only leader accepts collectors)
- Network partition handling
- Graceful shutdown and leadership resignation

**Prerequisites**:
- etcd cluster (3 nodes recommended for production, 1 for testing)
- Multiple bonsai core instances (at least 2 for failover)
- Network connectivity between all nodes
- cmake and build tools for Rust compilation

**Topology**: Multi-node bonsai core cluster + etcd cluster

**Run when**:
- After implementing HA features (leader election, config replication)
- After changes to HA coordinator logic
- Before deploying HA to production
- After etcd client library upgrades

---

## Testing Workflow

### For Signal Receiver Development
1. Run Phase 0-2 (build + ContainerLab deployment) from UBUNTU_TESTING_GUIDE.md
2. Run specific phase(s) for the receiver you modified:
   - gNMI: Phase 4
   - Syslog: Phase 5
   - SNMP: Phase 6
   - BMP: Phase 7
   - NetFlow: Phase 8
   - OTLP: Phase 9
3. Run Phase 10 (multi-source correlation) if you changed correlation logic
4. Run Phase 15 (fault injection round-trip) for end-to-end validation

### For HA Development
1. Follow HA_TESTING.md to set up etcd cluster
2. Configure multiple bonsai instances in cluster mode
3. Run Test 1-6 from HA_TESTING.md:
   - Test 1: Leader election
   - Test 2: Config replication
   - Test 3: Leader failover
   - Test 4: Collector re-homing
   - Test 5: Leader-aware routing
   - Test 6: Network partition
4. Monitor metrics during tests

### For Full Validation
Run both test suites sequentially:
1. First, run UBUNTU_TESTING_GUIDE.md to validate signal ingestion
2. Then, run HA_TESTING.md to validate cluster behavior

---

## Test Environment Matrix

| Test Suite | Requires ContainerLab | Requires etcd | Ubuntu Only | Compile Required |
|-----------|---------------------|---------------|------------|------------------|
| UBUNTU_TESTING_GUIDE.md | Yes | No | Yes | Yes |
| HA_TESTING.md | No | Yes | Yes | Yes |

---

## Quick Reference

### Signal Receiver Test Commands
```bash
cd /opt/bonsai
# Build
cargo build --release

# Deploy topology
sudo containerlab deploy --topo lab/signal-test-lab/signal-test.clab.yml --reconfigure

# Start bonsai
export BONSAI_VAULT_PASSPHRASE="bonsai-signal-test-pass"
nohup ./target/release/bonsai --config docker/configs/signal-test.toml > logs/bonsai-signal-test.log 2>&1 &

# Follow UBUNTU_TESTING_GUIDE.md for detailed test steps
```

### HA Test Commands
```bash
cd /opt/bonsai
# Build
cargo build --release

# Start etcd (using Docker Compose)
docker-compose up -d

# Start bonsai nodes
export BONSAI_HA_MODE=cluster
export BONSAI_NODE_ID=node-1
export BONSAI_ETCD_ENDPOINTS=http://etcd-1:2379,http://etcd-2:2379,http://etcd-3:2379
./target/release/bonsai --config bonsai.toml

# Follow HA_TESTING.md for detailed test steps
```

---

## Known Issues

### UBUNTU_TESTING_GUIDE.md
- Requires cmake for lbug compilation (not available on Mac)
- ContainerLab topology requires Docker and Linux
- Some tests require manual verification in UI

### HA_TESTING.md
- Requires etcd cluster setup
- Multiple bonsai instances need different ports
- Proto code regeneration required after proto changes
- Actual collector re-homing requires Python collector changes (not yet implemented)

---

## Reporting Test Results

When reporting test results, include:
- Which test suite you ran (UBUNTU_TESTING_GUIDE.md or HA_TESTING.md)
- Which specific test(s) failed
- Logs from the failing test
- Environment details (Ubuntu version, Docker version, etcd version)
- Commit hash of the code under test

---

## Related Documentation

- `docs/testing_discipline.md` - General testing philosophy and practices
- `docs/EV1_UBUNTU_TESTING_GUIDE.md` - EV1 ML pipeline testing (current)
- `docs/HA_READINESS.md` - HA architecture design (if exists)
- `DECISIONS.md` - Architectural decision records

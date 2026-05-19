# Core HA Readiness (G3) - Requirements and Limitations

## Current Implementation (First-Pass Foundation)

### What's Implemented
- **HACoordinator module** (`src/ha_coordinator.rs`):
  - `HAMode` enum (Standalone/Cluster)
  - `LeaderState` enum (Leader/Follower/Electing)
  - `ConfigReplicator` for config change notification
  - Basic election loop that assumes leadership in standalone mode

- **Configuration**:
  - `BONSAI_HA_MODE` env var to enable cluster mode
  - `BONSAI_NODE_ID` env var for node identification
  - HA coordinator spawned in `server_startup.rs`

### What's Missing (Requires External Infrastructure)

## 1. Distributed Leader Election

### Current State
```rust
// Placeholder - always becomes leader
pub async fn start_election(&self) {
    match &self.mode {
        HAMode::Standalone => {
            self.set_state(LeaderState::Leader).await;
        }
        HAMode::Cluster { node_id } => {
            // TODO: Implement etcd-based leader election
            self.set_state(LeaderState::Leader).await; // Temporary
        }
    }
}
```

### Requirements for Full Implementation
- **etcd cluster**: External etcd cluster (3+ nodes) for distributed consensus
- **etcd client library**: Add `etcd-client` crate to Cargo.toml
- **Election protocol**:
  - Use etcd's lease-based election (key: `/bonsai/leader`)
  - Heartbeat to maintain lease
  - Watch for leader changes
  - Automatic re-election on leader failure

### Implementation Steps
1. Add dependency: `etcd-client = "0.12"`
2. Implement etcd election in `HACoordinator::start_election()`:
   ```rust
   let client = etcd_client::Client::connect([etcd_endpoints], None).await?;
   let lease = client.lease_grant(10, None).await?;
   let election = client.election(lease.id());
   let campaign = election.campaign(node_id).await?;
   // Watch for leader changes
   ```
3. Add etcd endpoint configuration to `config.rs`
4. Handle leader transitions (demote to follower, promote to leader)

## 2. Config Replication

### Current State
```rust
pub async fn publish_change(&self, change: ConfigChange) {
    // TODO: Publish to replication channel (etcd, Kafka, etc.)
    tracing::debug!("config change published for replication");
}

pub async fn apply_change(&self, change: ConfigChange) -> anyhow::Result<()> {
    // TODO: Apply to local SQLite store
    tracing::debug!("applying replicated config change");
    Ok(())
}
```

### Requirements for Full Implementation
- **Replication channel**: etcd, Kafka, or NATS for config change streaming
- **Change serialization**: ConfigChange already structured
- **Conflict resolution**: Last-write-wins or vector clocks
- **Idempotency**: Ensure changes can be safely re-applied

### Implementation Steps
1. Choose replication channel (recommend etcd for consistency with leader election)
2. Implement `publish_change()` to write to etcd key `/bonsai/config/<table>/<key>`
3. Implement `apply_change()` to read from etcd and apply to SQLite
4. Add SQLite store watcher to receive and apply changes from other nodes
5. Add conflict detection and resolution logic

## 3. Collector Re-homing

### Current State
- **Not implemented** - no collector re-homing logic exists

### Requirements for Full Implementation
When a leader fails and a new leader is elected:
1. Collectors connected to old leader must reconnect to new leader
2. Device assignments must be re-established
3. Assignment rules must be consistent across nodes
4. Graceful handoff with minimal disruption

### Implementation Steps
1. Add leader endpoint to gRPC API: `/api/ha/leader`
2. Modify collectors to:
   - Periodically check leader endpoint
   - Reconnect on leader change
   - Resync device assignments
3. Add assignment rule replication via ConfigReplicator
4. Add collector state tracking in SQLite for failover
5. Implement graceful drain of old leader before demotion

## 4. Data Consistency

### Requirements
- **Graph DB replication**: KuzuDB doesn't support multi-master
- **Options**:
  - Leader-only writes (collectors write to leader only)
  - Read replicas (followers serve read-only queries)
  - External replication (Debezium, custom CDC)

### Implementation Steps
1. Route all collector telemetry to leader node
2. Implement query proxy for read replicas
3. Add leader-aware routing in gRPC API
4. Consider graph DB replication strategy (future work)

## Configuration Required

```toml
# bonsai.toml
[ha]
mode = "cluster"  # or "standalone"
node_id = "bonsai-1"

[ha.etcd]
endpoints = ["etcd-1:2379", "etcd-2:2379", "etcd-3:2379"]
election_ttl_secs = 10
config_prefix = "/bonsai/config"

[ha.replication]
enabled = true
channel = "etcd"  # or "kafka", "nats"
```

## Environment Variables

```bash
BONSAI_HA_MODE=cluster
BONSAI_NODE_ID=bonsai-1
BONSAI_ETCD_ENDPOINTS=etcd-1:2379,etcd-2:2379,etcd-3:2379
```

## Summary

**What's complete**: Foundation structures and configuration hooks

**What's missing**:
1. etcd client integration for leader election
2. Config replication protocol implementation
3. Collector re-homing logic
4. Graph DB replication strategy
5. Leader-aware routing

**Estimated effort**: 2-3 weeks of focused development
- Week 1: etcd integration and leader election
- Week 2: Config replication and conflict resolution
- Week 3: Collector re-homing and failover testing

**External dependencies**:
- etcd cluster (3+ nodes)
- Network connectivity between nodes
- Load balancer for client routing

**Recommended approach**:
1. Start with leader election using etcd
2. Add config replication before collector re-homing
3. Implement collector re-homing last (requires collector changes)
4. Test thoroughly with simulated failures

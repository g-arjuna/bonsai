# HA (High Availability) Testing Guide

This document describes how to test and validate Bonsai's HA failover behavior in a multi-node cluster deployment.

## Prerequisites

- etcd cluster (3 nodes recommended for production, 1 node for testing)
- Multiple bonsai core instances (at least 2 for failover testing)
- Network connectivity between all nodes
- etcd client accessible from all bonsai instances

## Setup

### 1. Configure etcd Cluster

Start an etcd cluster (using Docker Compose for testing):

```yaml
# docker-compose.yml
version: '3.8'
services:
  etcd-1:
    image: quay.io/coreos/etcd:v3.5.9
    command: etcd --name etcd-1 \
      --data-dir /etcd-data \
      --listen-client-urls http://0.0.0.0:2379 \
      --advertise-client-urls http://etcd-1:2379 \
      --listen-peer-urls http://0.0.0.0:2380 \
      --initial-advertise-peer-urls http://etcd-1:2380 \
      --initial-cluster etcd-1=http://etcd-1:2380,etcd-2=http://etcd-2:2380,etcd-3=http://etcd-3:2380 \
      --initial-cluster-token etcd-cluster \
      --initial-cluster-state new
    ports:
      - "2379:2379"
      - "2380:2380"

  etcd-2:
    image: quay.io/coreos/etcd:v3.5.9
    command: etcd --name etcd-2 \
      --data-dir /etcd-data \
      --listen-client-urls http://0.0.0.0:2379 \
      --advertise-client-urls http://etcd-2:2379 \
      --listen-peer-urls http://0.0.0.0:2380 \
      --initial-advertise-peer-urls http://etcd-2:2380 \
      --initial-cluster etcd-1=http://etcd-1:2380,etcd-2=http://etcd-2:2380,etcd-3=http://etcd-3:2380 \
      --initial-cluster-token etcd-cluster \
      --initial-cluster-state new
    ports:
      - "2479:2379"
      - "2480:2380"

  etcd-3:
    image: quay.io/coreos/etcd:v3.5.9
    command: etcd --name etcd-3 \
      --data-dir /etcd-data \
      --listen-client-urls http://0.0.0.0:2379 \
      --advertise-client-urls http://etcd-3:2379 \
      --listen-peer-urls http://0.0.0.0:2380 \
      --initial-advertise-peer-urls http://etcd-3:2380 \
      --initial-cluster etcd-1=http://etcd-1:2380,etcd-2=http://etcd-2:2380,etcd-3=http://etcd-3:2380 \
      --initial-cluster-token etcd-cluster \
      --initial-cluster-state new
    ports:
      - "2579:2379"
      - "2580:2380"
```

Start etcd:
```bash
docker-compose up -d
```

### 2. Configure Bonsai Nodes

For each bonsai core instance, configure `bonsai.toml`:

```toml
[ha]
mode = "cluster"
node_id = "node-1"  # Unique per node

[ha.etcd]
endpoints = "http://etcd-1:2379,http://etcd-2:2379,http://etcd-3:2379"
election_ttl_secs = 10
config_prefix = "/bonsai/config"
```

Or use environment variables:
```bash
export BONSAI_HA_MODE=cluster
export BONSAI_NODE_ID=node-1
export BONSAI_ETCD_ENDPOINTS=http://etcd-1:2379,http://etcd-2:2379,http://etcd-3:2379
export BONSAI_ETCD_ELECTION_TTL=10
export BONSAI_ETCD_CONFIG_PREFIX=/bonsai/config
```

### 3. Start Bonsai Instances

Start each bonsai core instance on different ports:

```bash
# Node 1
BONSAI_NODE_ID=node-1 BONSAI_HTTP_PORT=3000 BONSAI_GRPC_PORT=50051 ./bonsai

# Node 2
BONSAI_NODE_ID=node-2 BONSAI_HTTP_PORT=3001 BONSAI_GRPC_PORT=50052 ./bonsai

# Node 3
BONSAI_NODE_ID=node-3 BONSAI_HTTP_PORT=3002 BONSAI_GRPC_PORT=50053 ./bonsai
```

## Test Scenarios

### Test 1: Leader Election

**Objective**: Verify that exactly one node becomes leader and the others become followers.

**Steps**:
1. Start all bonsai instances
2. Check logs for "HA mode: cluster, assumed leader" or "became follower of node-X"
3. Use GetLeader RPC to verify leadership state

**Expected Result**:
- Exactly one node reports `is_leader: true`
- Other nodes report `is_leader: false` with `leader_id` pointing to the leader
- Leader ID matches the node that became leader

**Validation Commands**:
```bash
# Using grpcurl (if available)
grpcurl -plaintext localhost:50051 bonsai.v1.BonsaiGraph/GetLeader

# Check logs
grep "HA mode" /var/log/bonsai.log
grep "became follower" /var/log/bonsai.log
```

### Test 2: Config Replication

**Objective**: Verify that config changes replicate from leader to followers via etcd.

**Steps**:
1. Add a device via UI or API on the leader node
2. Check the follower nodes' SQLite stores
3. Verify the device appears on all nodes

**Expected Result**:
- Device added on leader
- Within a few seconds, device appears on all followers
- Config change logged in etcd under `/bonsai/config/devices/<address>`

**Validation Commands**:
```bash
# Add device on leader
curl -X POST http://localhost:3000/api/devices \
  -H "Content-Type: application/json" \
  -d '{"address":"192.0.2.1:57400","hostname":"test-device"}'

# Check etcd
etcdctl get /bonsai/config/devices/192.0.2.1:57400

# Query follower nodes
curl http://localhost:3001/api/devices
curl http://localhost:3002/api/devices
```

### Test 3: Leader Failover

**Objective**: Verify that when the leader fails, a new leader is elected.

**Steps**:
1. Identify the current leader (using GetLeader RPC)
2. Stop the leader process (SIGTERM or kill)
3. Watch the remaining nodes' logs
4. Verify a new leader is elected

**Expected Result**:
- Leader process stops
- Within `election_ttl_secs` (default 10s), a new leader is elected
- No split-brain (only one new leader)
- Collectors reconnect to new leader (if collector re-homing is implemented)

**Validation Commands**:
```bash
# Find leader
grpcurl -plaintext localhost:50051 bonsai.v1.BonsaiGraph/GetLeader

# Stop leader
kill <pid>

# Watch logs on remaining nodes
tail -f /var/log/bonsai.log | grep "leader"

# Verify new leader
grpcurl -plaintext localhost:50052 bonsai.v1.BonsaiGraph/GetLeader
```

### Test 4: Collector Re-homing

**Objective**: Verify that collectors reconnect to the new leader after failover.

**Steps**:
1. Start a collector connected to the current leader
2. Verify collector is registered and receiving assignments
3. Stop the leader
4. Watch collector logs for reconnection attempts
5. Verify collector reconnects to new leader

**Expected Result**:
- Collector queries GetLeader periodically
- On leader change, collector detects new leader
- Collector terminates old RegisterCollector stream
- Collector connects to new leader
- Device assignments resume

**Validation Commands**:
```bash
# Check collector status
curl http://localhost:9090/metrics | grep bonsai_collector_leader_id

# Watch collector logs
tail -f /var/log/collector.log | grep -i "leader\|reconnect"
```

### Test 5: Leader-Aware Routing

**Objective**: Verify that only the leader accepts new collector registrations.

**Steps**:
1. Identify a follower node
2. Attempt to register a collector against the follower
3. Verify registration is rejected with UNAVAILABLE status

**Expected Result**:
- Follower rejects registration
- Status code: UNAVAILABLE (14)
- Error message: "this node is not the leader, collectors should connect to the leader"
- Metric `bonsai_collector_registration_rejected_total{reason="not_leader"}` increments

**Validation Commands**:
```bash
# Attempt registration against follower
grpcurl -plaintext -d '{"collector_id":"test-collector","hostname":"test","protocol_version":1}' \
  localhost:50052 bonsai.v1.BonsaiGraph/RegisterCollector

# Check metrics
curl http://localhost:9090/metrics | grep bonsai_collector_registration_rejected_total
```

### Test 6: Network Partition

**Objective**: Verify behavior when network partition occurs (split-brain prevention).

**Steps**:
1. Block network between leader and etcd cluster
2. Wait for election TTL to expire
3. Verify leader loses leadership
4. Restore network
5. Verify node rejoins cluster as follower

**Expected Result**:
- Leader loses etcd connection
- Lease expires after TTL
- Node transitions to Electing state
- New leader elected among remaining nodes
- When network restored, node rejoins as follower

**Validation Commands**:
```bash
# Block network (using iptables)
sudo iptables -A OUTPUT -p tcp --dport 2379 -j DROP

# Wait 15s, then check logs
tail -f /var/log/bonsai.log

# Restore network
sudo iptables -D OUTPUT -p tcp --dport 2379 -j DROP
```

## Metrics to Monitor

During HA testing, monitor these metrics:

- `bonsai_ha_leader_state` - Current leadership state (0=Follower, 1=Leader, 2=Electing)
- `bonsai_ha_election_won_total` - Number of times this node won election
- `bonsai_ha_election_lost_total` - Number of times this node lost election
- `bonsai_config_change_published_total` - Config changes published to etcd
- `bonsai_config_change_applied_total` - Config changes applied from etcd
- `bonsai_collector_registration_rejected_total` - Collector rejections (by reason)

## Troubleshooting

### No Leader Elected

**Symptoms**: All nodes stuck in Electing state

**Checks**:
1. Verify etcd cluster is healthy: `etcdctl endpoint health`
2. Check etcd connectivity from each node
3. Verify BONSAI_ETCD_ENDPOINTS is correct
4. Check logs for etcd connection errors

### Config Not Replicating

**Symptoms**: Config changes not appearing on followers

**Checks**:
1. Verify config watcher is running on followers
2. Check etcd for config keys: `etcdctl get /bonsai/config --prefix`
3. Verify SQLite store has ConfigReplicator wired
4. Check logs for config watcher errors

### Collector Cannot Register

**Symptoms**: Collector registration fails on all nodes

**Checks**:
1. Verify leader is elected: `GetLeader RPC`
2. Check collector is connecting to leader
3. Verify BONSAI_COLLECTOR_TOKEN matches (if configured)
4. Check logs for registration rejection reasons

## Cleanup

Stop all services:

```bash
# Stop bonsai instances
pkill bonsai

# Stop etcd
docker-compose down
```

## Production Considerations

- Use 3+ etcd nodes for quorum
- Set appropriate `election_ttl_secs` based on network latency
- Monitor etcd cluster health
- Use TLS for etcd connections in production
- Configure BONSAI_COLLECTOR_TOKEN for authentication
- Test failover regularly in staging environment

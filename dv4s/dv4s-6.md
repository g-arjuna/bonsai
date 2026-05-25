## DS-6 — DDoS ML Feature Pipeline: Graph-to-Feature Export for Sidecar

### Analysis

The ML sidecar (`python/bonsai_ml/`, `python/bonsai_sdk/`) already has a GNN pipeline (`bonsai_ml/gnn/`) and feature schema (`bonsai_ml/feature_schema.py`). The embeddings module (`bonsai_ml/embeddings.py`) exists but has no DDoS-specific feature extraction. The sidecar can already call the Bonsai gRPC API and graph query endpoints. What is missing is:

1. A **DDoS feature vector** definition: what graph properties to extract, how to aggregate them into features, and at what temporal granularity.
2. A **baseline comparison** feature: current_value / baseline_p95 ratio for each metric (the `deviation_score` already computed by `TrafficBaseline` nodes in DS-2 T3).
3. A **multi-device temporal feature matrix**: for the campaign detection model, features are not per-device but per (time_window, affected_prefix) aggregated across all devices.
4. A **ground-truth labelling mechanism**: for supervised learning, the sidecar needs to know which `DdosEvent` nodes were confirmed true positives and which were false positives (from operator feedback — DS-7 UI).
5. A **continuous feature export** pipeline: features must be continuously computed and available, not computed on-demand, so the ML model can score in near-real-time.

**This epic is the bridge between the graph-enrichment work (DS-1→DS-5) and eventual ML training/inference. It does NOT include model training itself — that is out of scope for this supplement.**

### Tasks

**T1 — DDoS feature schema definition**

New file `python/bonsai_ml/ddos_feature_schema.py`:

```python
DDOS_FEATURE_SCHEMA = {
    # Per-interface, per-protocol, 60s window
    "interface_pps_ratio": "current_pps / baseline_p95_pps",         # deviation ratio
    "interface_bps_ratio": "current_bps / baseline_p95_bps",
    "tcp_syn_ratio": "syn_packets / total_tcp_packets",               # 1.0 = pure SYN flood
    "udp_amplification_ratio": "amplification_vector_pps / total_pps",
    "icmp_ratio": "icmp_pps / total_pps",
    "new_source_ip_entropy": "unique_src_ips in window / expected_unique_src_ips",
    # Per-device
    "copp_drop_rate": "copp_drop_pps (normalised to device class)",
    "lpts_drop_rate": "lpts_drop_pps (XR-specific, 0 for others)",
    "tcam_pressure": "tcam_utilization_pct / 100",
    "cpu_ratio": "cpu_util_pct / baseline_cpu_p95",
    "acl_deny_rate": "acl_deny_pps / baseline_acl_deny_p95",
    # BGP/BMP features
    "prefix_stability": "1 - (prefix_withdrawals / adj_rib_in_total)",
    "unexpected_asn_score": "1 if unexpected_origin_as event else 0",
    "rtbh_active": "1 if AffectedPrefix.rtbh_applied else 0",
    # Multi-source corroboration score
    "corroboration_source_count": "count(distinct source_types in window)",
    "corroboration_strength": "sum(source weights) per DS-3 T5 confidence model",
    # Temporal features
    "attack_duration_seconds": "now - DdosEvent.attack_start_ns",
    "ramp_rate": "pps_delta / time_delta (attack speed)",
    "burst_pattern": "std_dev(pps_in_window) / mean(pps_in_window)",
}
```

Features are computed at 3 temporal granularities:
- **10s window**: for real-time detection (low-latency).
- **60s window**: for pattern classification (most useful for ML).
- **300s window**: for campaign-level trending (ramp-rate, burst pattern).

**T2 — Feature extraction pipeline**

New file `python/bonsai_ml/ddos_features.py`:

```python
class DdosFeatureExtractor:
    """
    Queries the Bonsai graph API to extract DDoS feature vectors.
    Called by the sidecar on a configurable interval (default 10s).
    """
    
    def extract_device_features(self, device_address: str, window_s: int = 60) -> DdosDeviceFeature:
        """Extract per-device feature vector from graph."""
        # Queries: TrafficBaseline, Interface counters, CoPP stats, LPTS stats
        # Returns: DdosDeviceFeature dataclass
    
    def extract_prefix_features(self, prefix: str, window_s: int = 60) -> DdosPrefixFeature:
        """Extract per-prefix feature vector: aggregates all devices seeing traffic to prefix."""
        # Queries: AppFlow nodes filtered by dst_prefix, AffectedPrefix, DdosRouteEvent
    
    def extract_campaign_feature_matrix(self, window_s: int = 300) -> CampaignFeatureMatrix:
        """
        Extract multi-device feature matrix for campaign detection.
        Returns: (n_devices, n_features) numpy array + device_address index
        """
        # Used by GNN model: each device is a node, features are edge attributes
    
    def compute_source_diversity(self, flows: List[AppFlow]) -> float:
        """Shannon entropy of source IP /24 prefixes → 0=single source, 1=full diversity."""
```

- Uses `GET /api/explorer/query` (NL query endpoint) with pre-built Cypher queries for each feature type.
- Falls back to direct graph queries via `bonsai_sdk.client.py` if NL query latency is too high.
- `DdosDeviceFeature` and `DdosPrefixFeature` are dataclasses registered in `ddos_feature_schema.py`.

**T3 — Continuous feature export**

New sidecar task in `python/collector_engine.py` or new `python/ddos_feature_daemon.py`:

- Runs as a background thread in the sidecar process.
- Every 10s: calls `extract_device_features()` for all tracked devices.
- Writes feature vectors to:
  - In-memory `FeatureCache` (ring buffer, last 30 readings per device) for immediate rule evaluation.
  - Graph: upserts `DdosFeatureSnapshot` property set onto each `Device` node (latest feature vector as JSON blob) for NL query access.
  - Optional TSDB export: if TSDB configured (`[integrations.tsdb]`), push feature time-series for historical trend analysis and model training data collection.
- `GET /api/ddos/features/{device_address}` — returns current feature vector JSON for a device.
- `GET /api/ddos/features/matrix` — returns current campaign feature matrix as JSON.

**T4 — Ground-truth labelling for supervised training**

Extend `DdosEvent` node with labelling fields:
- `operator_verdict: String` — `"true_positive"` / `"false_positive"` / `"indeterminate"`.
- `verdict_note: String` — free-text operator annotation.
- `verdict_at_ns: Int64`.
- `verdict_by: String` — operator ID from auth system.

New API: `POST /api/ddos/events/{id}/verdict` — sets operator verdict. Role: Operator+.

In `python/bonsai_ml/ddos_features.py`, `export_labelled_dataset()`:
- Queries all `DdosEvent` nodes with `operator_verdict IS NOT NULL`.
- For each event: retrieves the feature snapshots from the event window (from graph or TSDB).
- Exports as JSON Lines dataset: `{features: {...}, label: "true_positive", event_id: "..."}`.
- `GET /api/ddos/training-export` — triggers export, returns download link.
- Dataset format is compatible with standard ML training frameworks (scikit-learn, PyTorch, TensorFlow).

**T5 — GNN integration for DDoS campaign detection**

Extend `python/bonsai_ml/gnn/` pipeline for DDoS:

- `DdosGnnDataset`: constructs graph from live Bonsai data for GNN inference.
  - Nodes: Device + Interface + AppFlow nodes visible in current 60s window.
  - Node features: `DdosDeviceFeature` vector.
  - Edges: `CONNECTED_TO`, `CARRIES_FLOW` relationships with flow volume as edge weight.
- `DdosCampaignClassifier`: GNN model (architecture: GCN or GraphSAGE — 2 layers) that takes the above graph and outputs per-device anomaly score + global campaign probability.
- Inference pipeline:
  1. Feature extractor runs every 10s → builds `DdosGnnDataset`.
  2. GNN model forward pass → per-device scores.
  3. If global campaign probability > threshold → call `create_detection(rule_id="ddos_gnn_campaign")`.
  4. Detection includes `features.gnn_device_scores` (per-device anomaly contribution).
- **Training is out of scope for this supplement**. The model weight loading path (`load_checkpoint()`) is a stub that logs a warning if no checkpoint is found — inference is disabled until a trained checkpoint is provided.
- `GET /api/sidecar/ddos/model-status` — reports whether GNN model checkpoint is loaded and ready.

**T6 — Feature drift monitoring**

The baseline (`TrafficBaseline`) will drift over time (legitimate traffic growth, new applications). Feature drift causes false positives.

- `DdosBaselineDriftDetector` in `python/bonsai_ml/ddos_features.py`:
  - Compares current `TrafficBaseline.p95_pps` against rolling 7-day median of `p95_pps` snapshots.
  - If p95 has grown >50% vs 7-day median → emit `ddos_baseline_drift` detection (severity=`info`) to prompt operator to acknowledge the new baseline.
- Baseline snapshots stored as `ConfigItem` records (config_class=traffic_baseline_snapshot) with timestamps.
- `POST /api/ddos/baselines/{device_address}/acknowledge` — operator acknowledges new baseline, resets drift detector reference point.


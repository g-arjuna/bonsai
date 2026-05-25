"""ML feature schema versioning (T7-11).

FeatureSchema records exactly which features (and their order) went into a set of
embeddings or a model checkpoint.  The GNN data loader (T2-3) checks that the
schema hash of the current embedding matches the hash baked into the checkpoint
before training or inference — preventing silent feature drift.

Canonical JSON: sorted keys, utf-8, no trailing newline.  created_at and
schema_hash are excluded from the hash so that re-exporting with a newer
timestamp does not change the hash.
"""
from __future__ import annotations

import hashlib
import json
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any


@dataclass
class FeatureSchema:
    version: str
    algorithm: str
    dimension: int
    hyperparams: dict[str, Any]
    feature_names: list[str]
    created_at_iso: str = field(default_factory=lambda: _now_iso())
    schema_hash: str = field(default="")

    def __post_init__(self) -> None:
        if not self.schema_hash:
            self.schema_hash = self._compute_hash()

    # ── public interface ──────────────────────────────────────────────────────

    def save(self, path: str | Path) -> None:
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(asdict(self), fh, sort_keys=True, indent=2)

    @classmethod
    def load(cls, path: str | Path) -> "FeatureSchema":
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
        obj = cls(**data)
        return obj

    def matches(self, other: "FeatureSchema") -> bool:
        return self.schema_hash == other.schema_hash

    # ── internals ─────────────────────────────────────────────────────────────

    def _compute_hash(self) -> str:
        payload = {
            "version": self.version,
            "algorithm": self.algorithm,
            "dimension": self.dimension,
            "hyperparams": self.hyperparams,
            "feature_names": self.feature_names,
        }
        canonical = json.dumps(payload, sort_keys=True, ensure_ascii=True)
        return hashlib.sha256(canonical.encode()).hexdigest()


def _now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


# ── canonical schema for spectral_v1 embeddings ───────────────────────────────

SPECTRAL_V1_SCHEMA = FeatureSchema(
    version="spectral_v1",
    algorithm="spectral",
    dimension=16,
    hyperparams={
        "n_neighbors": 10,
        "affinity": "nearest_neighbors",
        "random_state": 42,
    },
    feature_names=["graph_position_dim_%d" % i for i in range(16)],
)


# ── EV1-1 T6: 36-dim device feature schema for STGNN ─────────────────────────

DEVICE_V2_FEATURE_NAMES = [
    "degree",
    "vendor_nokia", "vendor_cisco", "vendor_juniper",
    "vendor_arista", "vendor_frr", "vendor_other",
    "role_super_spine", "role_spine", "role_leaf",
    "role_pe", "role_p", "role_rr", "role_ce",
    "role_access", "role_distribution", "role_core", "role_edge", "role_other",
    "embedding_0", "embedding_1", "embedding_2", "embedding_3",
    "cpu_util_pct", "memory_used_pct", "uptime_log_secs", "has_thermal_warning",
    "bgp_session_count", "ospf_neighbor_count", "interface_count",
    "bmp_session_count", "gnn_quality_score",
    "model_enterprise", "model_cloud", "model_sp",
    "is_in_redundancy_group",
]

DEVICE_V2_SCHEMA = FeatureSchema(
    version="device_v2",
    algorithm="handcrafted+spectral",
    dimension=36,
    hyperparams={
        "spectral_dims": 4,
        "operational_dims": 13,
        "vendor_ohe_dims": 6,
        "role_ohe_dims": 12,
        "gnn_model": "stgnn_gatv2_gru",
        "temporal_window": 8,
    },
    feature_names=DEVICE_V2_FEATURE_NAMES,
)

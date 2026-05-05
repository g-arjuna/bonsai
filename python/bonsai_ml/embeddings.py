"""Graph-position embeddings for bonsai Device nodes (T2-1).

Algorithm: spectral embedding (Laplacian eigenmaps via sklearn).
- Reads the physical topology from /api/topology.
- Builds an undirected adjacency matrix from CONNECTED_TO links.
- Runs sklearn SpectralEmbedding to produce dense position vectors.
- Pushes vectors back to the bonsai core via POST /api/graph/embeddings/upsert.

Spectral embedding is the linear-algebra foundation of node2vec: both derive
position from the graph Laplacian.  The upgrade path to biased random-walk
node2vec (p/q parameters) requires the `nodevectors` package; see model card.

Requires: scikit-learn, numpy (both in [project.optional-dependencies.ml]).

CLI:
  python -m bonsai_ml.embeddings --base-url http://localhost:3000 --dim 16
"""
from __future__ import annotations

import argparse
import time
from typing import TYPE_CHECKING

import numpy as np

from .feature_schema import SPECTRAL_V1_SCHEMA, FeatureSchema

if TYPE_CHECKING:
    from bonsai_sdk.client import BonsaiClient


# ── adjacency ─────────────────────────────────────────────────────────────────

def fetch_adjacency(client: "BonsaiClient") -> tuple[list[str], dict[str, list[str]]]:
    """Return (sorted device address list, undirected adjacency dict).

    Reads /api/topology and extracts physical links from the `links` array.
    BGP sessions are not included; they are logical, not positional.
    """
    topo = client._http_json("GET", "/api/topology")
    devices: list[str] = [d["address"] for d in topo.get("devices", [])]
    adj: dict[str, list[str]] = {addr: [] for addr in devices}
    for link in topo.get("links", []):
        src = link.get("src_device", "")
        dst = link.get("dst_device", "")
        if src in adj and dst in adj and src != dst:
            if dst not in adj[src]:
                adj[src].append(dst)
            if src not in adj[dst]:
                adj[dst].append(src)
    return sorted(devices), adj


# ── embedding computation ─────────────────────────────────────────────────────

def compute_spectral_embedding(
    devices: list[str],
    adj: dict[str, list[str]],
    n_components: int = 16,
    n_neighbors: int = 10,
    random_state: int = 42,
) -> dict[str, list[float]]:
    """Compute spectral graph embeddings and return {address: vector}.

    Uses sklearn SpectralEmbedding (nearest-neighbors affinity graph over the
    provided adjacency).  Isolated devices (no links) receive a zero vector.

    For graphs with < n_components+1 nodes the dimension is clamped to
    max(1, n_nodes - 1) so sklearn does not error.
    """
    from sklearn.manifold import SpectralEmbedding  # type: ignore

    n = len(devices)
    if n == 0:
        return {}

    # Build adjacency matrix (float32).
    idx = {addr: i for i, addr in enumerate(devices)}
    A = np.zeros((n, n), dtype=np.float32)
    for src, neighbors in adj.items():
        i = idx.get(src)
        if i is None:
            continue
        for dst in neighbors:
            j = idx.get(dst)
            if j is not None:
                A[i, j] = 1.0
                A[j, i] = 1.0

    dim = min(n_components, n - 1) if n > 1 else 1
    emb = SpectralEmbedding(
        n_components=dim,
        affinity="precomputed",
        random_state=random_state,
    )
    coords = emb.fit_transform(A)  # shape (n, dim)

    # Pad to requested dimension with zeros when dim < n_components.
    if dim < n_components:
        coords = np.pad(coords, ((0, 0), (0, n_components - dim)))

    return {devices[i]: coords[i].tolist() for i in range(n)}


# ── push to bonsai core ───────────────────────────────────────────────────────

def push_embeddings(
    client: "BonsaiClient",
    embeddings: dict[str, list[float]],
    version: str,
    algorithm: str = "spectral",
) -> int:
    """POST embeddings to /api/graph/embeddings/upsert.  Returns record count."""
    now_ns = int(time.time() * 1e9)
    records = [
        {
            "device_address": addr,
            "version": version,
            "algorithm": algorithm,
            "dimension": len(vec),
            "vector": vec,
            "computed_at_ns": now_ns,
        }
        for addr, vec in embeddings.items()
        if vec
    ]
    if records:
        client._http_json("POST", "/api/graph/embeddings/upsert", {"records": records})
    return len(records)


# ── high-level run ────────────────────────────────────────────────────────────

def run_embedding_pipeline(
    client: "BonsaiClient",
    schema: FeatureSchema = SPECTRAL_V1_SCHEMA,
) -> dict[str, object]:
    """End-to-end: fetch topology → compute embeddings → push → return summary."""
    devices, adj = fetch_adjacency(client)
    if not devices:
        return {"devices": 0, "pushed": 0, "version": schema.version}

    embeddings = compute_spectral_embedding(
        devices,
        adj,
        n_components=schema.dimension,
        n_neighbors=schema.hyperparams.get("n_neighbors", 10),
        random_state=schema.hyperparams.get("random_state", 42),
    )
    pushed = push_embeddings(client, embeddings, schema.version, schema.algorithm)
    return {
        "devices": len(devices),
        "pushed": pushed,
        "version": schema.version,
        "schema_hash": schema.schema_hash,
    }


# ── CLI ───────────────────────────────────────────────────────────────────────

def _cli() -> None:
    import json
    from bonsai_sdk.client import BonsaiClient

    parser = argparse.ArgumentParser(description="Compute and push graph embeddings")
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    parser.add_argument("--dim", type=int, default=SPECTRAL_V1_SCHEMA.dimension)
    parser.add_argument("--version", default=SPECTRAL_V1_SCHEMA.version)
    args = parser.parse_args()

    schema = FeatureSchema(
        version=args.version,
        algorithm=SPECTRAL_V1_SCHEMA.algorithm,
        dimension=args.dim,
        hyperparams=SPECTRAL_V1_SCHEMA.hyperparams,
        feature_names=["graph_position_dim_%d" % i for i in range(args.dim)],
    )

    client = BonsaiClient(http_base_url=args.base_url)
    result = run_embedding_pipeline(client, schema)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    _cli()

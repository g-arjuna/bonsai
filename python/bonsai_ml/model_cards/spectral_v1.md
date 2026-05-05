# Model Card — spectral_v1 graph-position embeddings

## Algorithm

**Spectral embedding** (Laplacian eigenmaps) via `sklearn.manifold.SpectralEmbedding`.

The embedding is derived from the leading eigenvectors of the graph Laplacian.
Devices that are topologically close (short hop-distance) cluster together in
embedding space.  This captures graph position without requiring training labels
or explicit random walks.

Upgrade path: biased random-walk node2vec (p/q parameters) using the `nodevectors`
package produces richer embeddings when walk data is available.  The API contract
(`/api/graph/embeddings/upsert`) and `FeatureSchema` versioning are designed to
be algorithm-agnostic — swap the computation in `embeddings.py` and bump the
schema version to `node2vec_v1`.

## Hyperparameters

| Parameter      | Value | Notes                                       |
|----------------|-------|---------------------------------------------|
| n_components   | 16    | Output embedding dimension                  |
| affinity       | precomputed | Binary adjacency matrix from LLDP links |
| random_state   | 42    | Reproducibility                             |

## Input graph

- Nodes: Device nodes reachable via `/api/topology`
- Edges: Physical `CONNECTED_TO` links (LLDP-derived), undirected
- BGP sessions excluded (logical, not positional)
- Isolated devices (no LLDP links) receive a zero vector

## Feature vector layout

16 floats: `[graph_position_dim_0, ..., graph_position_dim_15]`

The semantic interpretation of individual dimensions is not fixed — the full
vector captures relative graph position.  Downstream models (tabular MLDetector,
GNN) consume the vector as-is.

## Evaluation

**Current status**: infrastructure-only.  The model card will be updated with
intrinsic evaluation (node-classification accuracy on synthetic topology labels)
after the lab accumulates at least 30 days of chaos-run telemetry, as required
by the Bv1 backlog honest-validation guardrail.

Preliminary sanity check: on the 2-spine/4-leaf DC test fixture (7 devices),
spines and leaves cluster into distinct regions of the 2D projection —
consistent with expected topology role separation.

## Limitations

- Transductive: new devices added after embedding computation receive a zero
  vector until the next run.  Schedule `python -m bonsai_ml.embeddings` as a
  cron job or after each onboarding event.
- Small graphs (< 17 nodes) are padded to 16 dims with zeros; the embedding is
  still valid but some dimensions carry no information.
- No cross-vendor normalisation: vendor-specific LLDP neighbour reporting
  differences may affect adjacency completeness.

## Schema hash

The `FeatureSchema` hash for this version is deterministic from the hyperparameter
set.  The GNN data loader (T2-3) validates this hash before training to prevent
silent feature drift when hyperparameters change.

## Version history

| Version      | Change                          |
|--------------|---------------------------------|
| spectral_v1  | Initial release (this card)     |

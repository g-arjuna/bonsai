# Path A Model Card — Spectral Graph Embeddings

**Version**: spectral_v1  
**Schema hash**: `16930b3e902e7600028ec87215ba6b5d7949899cc9bb61e4576a85e3d2995f75`  
**Created**: 2026-05-07  
**Status**: Deployed against 8-node DC EVPN lab

---

## Algorithm

**Spectral Embedding** (Laplacian Eigenmaps via `sklearn.manifold.SpectralEmbedding`).

Constructs the normalized graph Laplacian from the physical topology adjacency matrix
and returns the `n_components` eigenvectors corresponding to the smallest non-zero
eigenvalues. These eigenvectors are the graph-position coordinates — nodes with many
shared neighbors end up close in embedding space. Spectral embedding is the
linear-algebra foundation of node2vec; both derive position from the graph Laplacian.

## Hyperparameters

| Parameter | Value | Notes |
|---|---|---|
| `n_components` (dim) | 16 | Padded with zeros when graph has < 17 nodes |
| `affinity` | `precomputed` | Adjacency matrix passed directly; nearest-neighbors step skipped |
| `random_state` | 42 | For reproducibility of eigenvector orientation |

When `n_components ≥ N` (graph nodes), sklearn falls back to `scipy.linalg.eigh`
(dense eigensolver). This is expected and correct at lab scale (8 nodes, 16 dims).
The warning is harmless.

## Input

- **Source**: `GET /api/topology` — `links` array, physical CONNECTED_TO edges only
- **Excluded**: BGP sessions (logical, not positional), MGMT_LINK edges (out-of-band)
- **Graph type**: undirected, unweighted, single connected component (8-node DC Clos)

## Output

- 16-dimensional float32 vector per Device node
- Stored as `embedding` property on each Device node in LadybugDB
- Pushed via `POST /api/graph/embeddings/upsert`
- Feature names: `graph_position_dim_0` … `graph_position_dim_15`

## Dataset

| Field | Value |
|---|---|
| Lab | 8-node DC EVPN SRv6 (2× super-spine, 2× spine, 4× leaf) |
| Vendor | Nokia SR Linux (all nodes) |
| Topology | Clos fabric, full mesh spine-to-leaf |
| Enrichment | NetBox (68 nodes), ServiceNow PDI (92 nodes) |
| Timestamp | 2026-05-07 |

## Evaluation

Spectral embeddings are **unsupervised positional encodings** — no ground-truth labels
exist, so standard classification metrics do not apply. Correctness is validated
structurally:

- Super-spines (2 nodes, degree 4) should cluster together and be far from leaves.
- Leaves (4 nodes, degree 2) should cluster together.
- Spines (2 nodes, degree 4) should sit between super-spines and leaves.

At 8 nodes with a symmetric Clos topology, all 8 devices receive distinct non-zero
embeddings. Zero-padding for dims 8-15 (since N=8 < 16 components) is intentional.

**Upgrade path**: when the graph reaches ≥ 50 nodes, switch affinity from
`precomputed` to `nearest_neighbors` and restore the `n_neighbors=10` parameter.
For biased random-walk embeddings (node2vec p/q), add the `nodevectors` package.

## Limitations

- Embeddings encode **topology position only** — they do not encode enrichment
  properties (NetBox site/role, ServiceNow CI class). Feature vectors for anomaly
  detection (IsolationForest) combine spectral dims with operational counters separately.
- Embeddings are **static** — computed on demand, not continuously updated. Topology
  changes (link up/down) are not reflected until the next run.
- At lab scale (< 17 nodes), all eigenvectors are computed by the dense solver; this
  is O(N³) and acceptable up to a few thousand nodes.
- Orientation of eigenvectors is arbitrary (sign-flipped across runs with different
  random state). Cosine similarity is stable; Euclidean distance is stable within one
  run but not across runs with different `random_state`.

## References

- sklearn SpectralEmbedding: https://scikit-learn.org/stable/modules/generated/sklearn.manifold.SpectralEmbedding.html
- Laplacian Eigenmaps: Belkin & Niyogi (2003), "Laplacian Eigenmaps for Dimensionality Reduction and Data Representation"
- node2vec: Grover & Leskovec (2016) — upgrade target for Path B
- Implementation: `python/bonsai_ml/embeddings.py`, schema in `python/bonsai_ml/feature_schema.py`

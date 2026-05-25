"""Config embedding PCA compression for GNN feature augmentation.

EV1-3 T5: EmbeddingPCA fits a PCA on all DeviceConfigEmbedding vectors
(384 dims from sentence-transformers) and compresses them to 8 dims.
The 8-dim compressed vector augments the GNN device feature vector
(36 → 40 dims after replacing 4 spectral dims).

Rationale: 8 dims for config semantics captures vendor-specific config
clusters (Nokia EVPN, Cisco MPLS, FRR BGP-only) that predict failure modes.

Saved as models/config_embedding_pca.pkl.
Retrained automatically when config embedding count increases by >10%
(triggered by EV1-5 scheduler).
"""
from __future__ import annotations

import json
import logging
import os
import pickle
import time
from pathlib import Path
from typing import Optional

import numpy as np

log = logging.getLogger(__name__)

DEFAULT_MODEL_PATH = "models/config_embedding_pca.pkl"
DEFAULT_N_COMPONENTS = 8
MIN_SAMPLES_TO_FIT = 10
RETRAIN_GROWTH_THRESHOLD = 0.10  # retrain when count grows >10%


class EmbeddingPCA:
    """PCA compressor for DeviceConfigEmbedding vectors.

    Args:
        n_components: Target dimensionality after compression (default 8).
        model_path: Path to save/load the fitted PCA model.
    """

    def __init__(
        self,
        n_components: int = DEFAULT_N_COMPONENTS,
        model_path: str = DEFAULT_MODEL_PATH,
    ) -> None:
        self.n_components = n_components
        self.model_path = Path(model_path)
        self._pca: Optional[object] = None
        self._fitted_on: int = 0  # count of samples at fit time
        self._fitted_at: float = 0.0

    def fit(self, vectors: np.ndarray) -> None:
        """Fit PCA on the provided embedding matrix.

        Args:
            vectors: Float array of shape (N, dim), e.g. (N, 384).
                     Requires N >= MIN_SAMPLES_TO_FIT.
        """
        from sklearn.decomposition import PCA

        n = vectors.shape[0]
        if n < MIN_SAMPLES_TO_FIT:
            raise ValueError(
                f"EmbeddingPCA.fit requires at least {MIN_SAMPLES_TO_FIT} samples, got {n}"
            )
        n_comp = min(self.n_components, n, vectors.shape[1])
        pca = PCA(n_components=n_comp)
        pca.fit(vectors.astype(np.float32))
        self._pca = pca
        self._fitted_on = n
        self._fitted_at = time.time()
        log.info(
            "EmbeddingPCA fitted: %d samples → %d dims (explained_variance_ratio=%.3f)",
            n,
            n_comp,
            float(pca.explained_variance_ratio_.sum()),
        )

    def transform(self, embedding_vector: np.ndarray) -> np.ndarray:
        """Compress one or more embedding vectors to n_components dims.

        Args:
            embedding_vector: Shape (dim,) for a single vector or (N, dim) for batch.

        Returns:
            Compressed array of shape (n_components,) or (N, n_components).
            Returns zero vector if PCA not fitted.
        """
        if self._pca is None:
            single = embedding_vector.ndim == 1
            out = np.zeros(self.n_components, dtype=np.float32)
            return out if single else np.zeros((embedding_vector.shape[0], self.n_components), dtype=np.float32)

        single = embedding_vector.ndim == 1
        arr = embedding_vector.reshape(1, -1) if single else embedding_vector
        compressed = self._pca.transform(arr.astype(np.float32))
        # Pad to n_components if fitted PCA has fewer (edge case: very few samples)
        if compressed.shape[1] < self.n_components:
            pad = np.zeros((compressed.shape[0], self.n_components - compressed.shape[1]), dtype=np.float32)
            compressed = np.concatenate([compressed, pad], axis=1)
        return compressed[0] if single else compressed

    def save(self) -> None:
        """Persist the fitted PCA to disk."""
        if self._pca is None:
            raise RuntimeError("EmbeddingPCA.save called before fit")
        self.model_path.parent.mkdir(parents=True, exist_ok=True)
        meta = {"fitted_on": self._fitted_on, "fitted_at": self._fitted_at, "n_components": self.n_components}
        payload = {"pca": self._pca, "meta": meta}
        with open(self.model_path, "wb") as f:
            pickle.dump(payload, f)
        log.info("EmbeddingPCA saved to %s", self.model_path)

    @classmethod
    def load(cls, model_path: str = DEFAULT_MODEL_PATH) -> "EmbeddingPCA":
        """Load a previously saved EmbeddingPCA from disk."""
        p = Path(model_path)
        if not p.exists():
            raise FileNotFoundError(f"EmbeddingPCA model not found at {p}")
        with open(p, "rb") as f:
            payload = pickle.load(f)
        obj = cls(n_components=payload["meta"]["n_components"], model_path=model_path)
        obj._pca = payload["pca"]
        obj._fitted_on = payload["meta"]["fitted_on"]
        obj._fitted_at = payload["meta"]["fitted_at"]
        log.info(
            "EmbeddingPCA loaded from %s (fitted_on=%d samples)",
            p,
            obj._fitted_on,
        )
        return obj

    def needs_retrain(self, current_count: int) -> bool:
        """Return True if embedding count has grown by >RETRAIN_GROWTH_THRESHOLD."""
        if self._fitted_on == 0:
            return True
        growth = (current_count - self._fitted_on) / self._fitted_on
        return growth > RETRAIN_GROWTH_THRESHOLD


def fit_from_api(
    api_url: str,
    model_path: str = DEFAULT_MODEL_PATH,
    n_components: int = DEFAULT_N_COMPONENTS,
) -> EmbeddingPCA:
    """Fetch all DeviceConfigEmbedding vectors from the API and fit PCA.

    This is the entry point used by the ML job scheduler (EV1-5).
    Triggered automatically after config embedding count increases by >10%.

    Args:
        api_url: Base URL of the Bonsai HTTP API.
        model_path: Where to save the fitted PCA.
        n_components: Target compressed dimensionality.

    Returns:
        Fitted EmbeddingPCA instance (also saved to model_path).
    """
    import urllib.request

    url = f"{api_url.rstrip('/')}/api/ml/embeddings/config-vectors"
    log.info("EmbeddingPCA: fetching config embedding vectors from %s", url)

    try:
        with urllib.request.urlopen(url, timeout=30) as resp:
            data = json.loads(resp.read())
    except Exception as exc:
        log.error("EmbeddingPCA: failed to fetch vectors: %s", exc)
        raise

    vectors_raw = data.get("vectors", [])
    if not vectors_raw:
        raise ValueError("No config embedding vectors returned from API")

    vectors = np.array(vectors_raw, dtype=np.float32)
    log.info("EmbeddingPCA: fitting on %d vectors of dim %d", vectors.shape[0], vectors.shape[1])

    pca = EmbeddingPCA(n_components=n_components, model_path=model_path)
    pca.fit(vectors)
    pca.save()
    return pca


def get_or_load(model_path: str = DEFAULT_MODEL_PATH) -> Optional[EmbeddingPCA]:
    """Load EmbeddingPCA from disk, or return None if not yet fitted.

    Used at GNN inference time: if PCA not available, feature vector falls
    back to zero-padding for the config embedding dims (no crash).
    """
    try:
        return EmbeddingPCA.load(model_path)
    except FileNotFoundError:
        log.debug("EmbeddingPCA not yet fitted — config embedding dims will be zero")
        return None
    except Exception as exc:
        log.warning("EmbeddingPCA load error: %s — config embedding dims will be zero", exc)
        return None

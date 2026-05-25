"""Detection reason clustering via HDBSCAN.

EV1-3 T8: Embeds DetectionEvent.reason strings using TextEmbedder, then
clusters using HDBSCAN (variable cluster count, noise-robust — better than
KMeans for sparse detection streams).

Assigns incident_cluster_id to each detection. Detections in the same cluster
share a likely root cause.

Feeds into BonPy investigation aggregation:
  "7 detections in the last hour all belong to cluster 3 (ISIS adjacency
   instability)."

The Rust DetectionEvent migration adds incident_cluster_id: Option<String>.
This module PATCH-es the column via /api/detections/{id}/cluster.
"""
from __future__ import annotations

import json
import logging
import time
import urllib.request
import urllib.parse
from dataclasses import dataclass
from typing import Optional

import numpy as np

log = logging.getLogger(__name__)

DEFAULT_MIN_CLUSTER_SIZE = 3
DEFAULT_MIN_SAMPLES = 2
DEFAULT_WINDOW_HOURS = 168  # 7 days
CLUSTER_REFRESH_INTERVAL_SECS = 3600  # re-cluster every hour by default


@dataclass
class ClusteringResult:
    """Outcome of a detection clustering run."""
    n_detections: int
    n_clusters: int
    n_noise: int
    cluster_sizes: dict[int, int]
    elapsed_secs: float


class DetectionClusterer:
    """Cluster DetectionEvent reason strings using HDBSCAN on text embeddings.

    Args:
        api_url: Base URL of the Bonsai HTTP API.
        min_cluster_size: HDBSCAN min_cluster_size (minimum detections per cluster).
        min_samples: HDBSCAN min_samples (controls density threshold).
        window_hours: Fetch detections from the last N hours.
    """

    def __init__(
        self,
        api_url: str,
        min_cluster_size: int = DEFAULT_MIN_CLUSTER_SIZE,
        min_samples: int = DEFAULT_MIN_SAMPLES,
        window_hours: int = DEFAULT_WINDOW_HOURS,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.min_cluster_size = min_cluster_size
        self.min_samples = min_samples
        self.window_hours = window_hours

    def run(self) -> Optional[ClusteringResult]:
        """Fetch recent detections, embed reasons, cluster, write back labels.

        Returns:
            ClusteringResult or None if fewer than min_cluster_size detections.
        """
        t0 = time.monotonic()

        detections = self._fetch_detections()
        if not detections:
            log.info("DetectionClusterer: no detections to cluster")
            return None

        reasons = [d.get("reason", "") for d in detections]
        ids = [d.get("id", "") for d in detections]

        if len(detections) < self.min_cluster_size:
            log.info(
                "DetectionClusterer: only %d detections — need at least %d to cluster",
                len(detections),
                self.min_cluster_size,
            )
            return None

        embeddings = self._embed_reasons(reasons)
        if embeddings is None:
            return None

        labels = self._cluster(embeddings)
        self._write_back_labels(ids, labels)

        n_clusters = len(set(labels) - {-1})
        n_noise = int((labels == -1).sum())
        sizes: dict[int, int] = {}
        for lbl in labels:
            if lbl >= 0:
                sizes[int(lbl)] = sizes.get(int(lbl), 0) + 1

        elapsed = time.monotonic() - t0
        log.info(
            "DetectionClusterer: %d detections → %d clusters, %d noise (%.2fs)",
            len(detections),
            n_clusters,
            n_noise,
            elapsed,
        )
        return ClusteringResult(
            n_detections=len(detections),
            n_clusters=n_clusters,
            n_noise=n_noise,
            cluster_sizes=sizes,
            elapsed_secs=elapsed,
        )

    def _fetch_detections(self) -> list[dict]:
        """GET /api/detections?window_hours=N — fetch recent detection events."""
        since_ns = int((time.time() - self.window_hours * 3600) * 1e9)
        url = f"{self.api_url}/api/detections?since_ns={since_ns}&limit=2000"
        try:
            with urllib.request.urlopen(url, timeout=15) as resp:
                data = json.loads(resp.read())
            items = data.get("detections", data) if isinstance(data, dict) else data
            log.debug("DetectionClusterer: fetched %d detections", len(items))
            return items
        except Exception as exc:
            log.warning("DetectionClusterer: failed to fetch detections: %s", exc)
            return []

    def _embed_reasons(self, reasons: list[str]) -> Optional[np.ndarray]:
        """Embed detection reason strings using TextEmbedder."""
        try:
            from bonsai_ml.text_embeddings import TextEmbedder
            embedder = TextEmbedder()
            vecs = embedder.embed_batch(reasons)
            return vecs.astype(np.float32)
        except ImportError:
            log.warning("DetectionClusterer: text_embeddings not available")
            return None
        except Exception as exc:
            log.warning("DetectionClusterer: embedding failed: %s", exc)
            return None

    def _cluster(self, embeddings: np.ndarray) -> np.ndarray:
        """Run HDBSCAN on the embedding matrix. Returns integer label array (-1 = noise)."""
        try:
            import hdbscan
            clusterer = hdbscan.HDBSCAN(
                min_cluster_size=self.min_cluster_size,
                min_samples=self.min_samples,
                metric="euclidean",
                cluster_selection_method="eom",
            )
            labels = clusterer.fit_predict(embeddings)
            return np.array(labels, dtype=int)
        except ImportError:
            log.warning(
                "DetectionClusterer: hdbscan not installed — falling back to MiniBatchKMeans"
            )
            return self._cluster_kmeans(embeddings)
        except Exception as exc:
            log.warning("DetectionClusterer: HDBSCAN failed: %s", exc)
            return np.full(len(embeddings), -1, dtype=int)

    def _cluster_kmeans(self, embeddings: np.ndarray) -> np.ndarray:
        """Fallback KMeans clustering when hdbscan is unavailable."""
        try:
            from sklearn.cluster import MiniBatchKMeans

            n_clusters = max(2, min(20, len(embeddings) // self.min_cluster_size))
            km = MiniBatchKMeans(n_clusters=n_clusters, random_state=42, n_init=3)
            return km.fit_predict(embeddings).astype(int)
        except Exception as exc:
            log.warning("DetectionClusterer: KMeans fallback failed: %s", exc)
            return np.full(len(embeddings), -1, dtype=int)

    def _write_back_labels(self, ids: list[str], labels: np.ndarray) -> None:
        """PATCH incident_cluster_id back to each detection via REST API."""
        success = 0
        for det_id, label in zip(ids, labels):
            if not det_id:
                continue
            cluster_id = None if label < 0 else str(label)
            payload = json.dumps({"incident_cluster_id": cluster_id}).encode()
            url = f"{self.api_url}/api/detections/{urllib.parse.quote(det_id)}/cluster"
            req = urllib.request.Request(
                url,
                data=payload,
                method="PATCH",
                headers={"Content-Type": "application/json"},
            )
            try:
                with urllib.request.urlopen(req, timeout=5):
                    success += 1
            except Exception as exc:
                log.debug("DetectionClusterer: write-back failed for %s: %s", det_id, exc)
        log.debug("DetectionClusterer: wrote labels for %d/%d detections", success, len(ids))


def run_clustering_job(api_url: str, **kwargs) -> Optional[ClusteringResult]:
    """Entry point for the ML job scheduler (EV1-5).

    Called weekly by DetectionClusteringJob scheduled via APScheduler.
    """
    clusterer = DetectionClusterer(api_url=api_url, **kwargs)
    return clusterer.run()

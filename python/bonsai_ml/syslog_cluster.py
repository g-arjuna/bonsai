"""Syslog cluster analysis via EventEmbedding vectors.

EV1-3 T6: Fetches embedded syslog event vectors, clusters them with
MiniBatchKMeans (n=20), assigns cluster IDs back to events, and stores
cluster centroids as SyslogCluster nodes.

Used by investigation runtime to surface: "This syslog is in cluster 7
(BGP notification storms). 15 similar messages seen on 3 devices in 24h."

Refreshed weekly by ML job scheduler (EV1-5).

Run standalone:
    python -m bonsai_ml.syslog_cluster --api-url http://localhost:3000
"""
from __future__ import annotations

import logging
import os
import time
from dataclasses import dataclass, field
from typing import Any, Optional

import numpy as np

log = logging.getLogger(__name__)

DEFAULT_N_CLUSTERS = 20
MIN_SAMPLES_TO_CLUSTER = 100
DEFAULT_API_URL = "http://localhost:3000"
FETCH_LIMIT = 5000


@dataclass
class ClusterResult:
    """Outcome of a clustering run."""
    n_clusters: int = 0
    n_events_clustered: int = 0
    cluster_labels: list[int] = field(default_factory=list)
    centroids: Optional[Any] = None
    inertia: float = 0.0
    skipped: bool = False
    skip_reason: str = ""


@dataclass
class ClusterSummary:
    """Per-cluster summary for storage as SyslogCluster node."""
    cluster_id: int
    label: str
    event_count: int
    top_event_types: list[str] = field(default_factory=list)
    centroid: list[float] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "cluster_id": self.cluster_id,
            "label": self.label,
            "event_count": self.event_count,
            "top_event_types": self.top_event_types,
            "centroid_json": self.centroid,
        }


class SyslogClusterer:
    """Cluster syslog events by embedding similarity.

    Args:
        api_url: Bonsai core API base URL.
        n_clusters: Number of MiniBatchKMeans clusters (default 20).
        min_samples: Minimum embedded events needed before clustering is useful.
    """

    def __init__(
        self,
        api_url: str = DEFAULT_API_URL,
        n_clusters: int = DEFAULT_N_CLUSTERS,
        min_samples: int = MIN_SAMPLES_TO_CLUSTER,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.n_clusters = n_clusters
        self.min_samples = min_samples

    def run(self, lookback_hours: int = 168) -> ClusterResult:
        """Fetch embeddings, cluster, write assignments and centroids.

        Args:
            lookback_hours: How far back to look for embedded events (default 7 days).

        Returns:
            ClusterResult with cluster count, inertia, and event count.
        """
        event_records = self._fetch_embeddings(lookback_hours)

        if len(event_records) < self.min_samples:
            log.info(
                "SyslogClusterer: only %d embedded events (need %d), skipping",
                len(event_records), self.min_samples,
            )
            return ClusterResult(
                skipped=True,
                skip_reason=f"insufficient_samples ({len(event_records)} < {self.min_samples})",
            )

        event_ids = [r["event_id"] for r in event_records]
        event_types = [r.get("event_type") or "" for r in event_records]
        vectors = np.array([r["vector"] for r in event_records], dtype=np.float32)

        result = self._cluster(vectors)
        if result.skipped:
            return result

        self._post_cluster_assignments(event_ids, result.cluster_labels)

        summaries = self._build_summaries(
            cluster_labels=result.cluster_labels,
            event_types=event_types,
            centroids=result.centroids,
            n_clusters=result.n_clusters,
        )
        self._post_cluster_centroids(summaries)

        log.info(
            "SyslogClusterer: clustered %d events into %d clusters (inertia=%.2f)",
            result.n_events_clustered, result.n_clusters, result.inertia,
        )
        return result

    def _fetch_embeddings(self, lookback_hours: int) -> list[dict[str, Any]]:
        try:
            import requests
            since_ns = int((time.time() - lookback_hours * 3600) * 1e9)
            resp = requests.get(
                f"{self.api_url}/api/ml/embeddings/events",
                params={"event_type": "syslog", "since_ns": since_ns, "limit": FETCH_LIMIT},
                timeout=30,
            )
            if resp.ok:
                records = resp.json()
                valid = [
                    r for r in records
                    if r.get("vector") and len(r["vector"]) > 0
                ]
                log.debug("Fetched %d embedding records for clustering", len(valid))
                return valid
        except Exception as exc:
            log.warning("SyslogClusterer: failed to fetch embeddings: %s", exc)
        return []

    def _cluster(self, vectors: np.ndarray) -> ClusterResult:
        try:
            from sklearn.cluster import MiniBatchKMeans
        except ImportError as exc:
            raise RuntimeError(
                "scikit-learn not installed. Run: pip install scikit-learn"
            ) from exc

        n_clusters = min(self.n_clusters, len(vectors) // 5)
        if n_clusters < 2:
            return ClusterResult(
                skipped=True,
                skip_reason=f"not_enough_samples_per_cluster (n_vectors={len(vectors)})",
            )

        kmeans = MiniBatchKMeans(
            n_clusters=n_clusters,
            batch_size=min(1024, len(vectors)),
            random_state=42,
            n_init=3,
        )
        labels = kmeans.fit_predict(vectors)

        return ClusterResult(
            n_clusters=n_clusters,
            n_events_clustered=len(vectors),
            cluster_labels=labels.tolist(),
            centroids=kmeans.cluster_centers_,
            inertia=float(kmeans.inertia_),
        )

    def _build_summaries(
        self,
        cluster_labels: list[int],
        event_types: list[str],
        centroids: Any,
        n_clusters: int,
    ) -> list[ClusterSummary]:
        from collections import Counter

        cluster_event_types: dict[int, list[str]] = {i: [] for i in range(n_clusters)}
        for label, etype in zip(cluster_labels, event_types):
            if 0 <= label < n_clusters and etype:
                cluster_event_types[label].append(etype)

        summaries: list[ClusterSummary] = []
        for cid in range(n_clusters):
            etypes = cluster_event_types[cid]
            top_types = [t for t, _ in Counter(etypes).most_common(5)]
            primary = top_types[0] if top_types else f"cluster_{cid}"
            count = len(etypes)

            centroid = (
                centroids[cid].tolist()
                if centroids is not None and cid < len(centroids)
                else []
            )

            summaries.append(ClusterSummary(
                cluster_id=cid,
                label=f"cluster_{cid}_{primary}",
                event_count=count,
                top_event_types=top_types,
                centroid=centroid,
            ))

        return summaries

    def _post_cluster_assignments(
        self, event_ids: list[str], labels: list[int]
    ) -> None:
        assignments = [
            {"event_id": eid, "cluster_id": label}
            for eid, label in zip(event_ids, labels)
        ]
        try:
            import requests
            resp = requests.patch(
                f"{self.api_url}/api/events/cluster-labels",
                json={"assignments": assignments},
                timeout=30,
            )
            if not resp.ok:
                log.warning(
                    "Failed to post cluster assignments: HTTP %d", resp.status_code
                )
        except Exception as exc:
            log.warning("SyslogClusterer: failed to post assignments: %s", exc)

    def _post_cluster_centroids(self, summaries: list[ClusterSummary]) -> None:
        payload = [s.to_dict() for s in summaries]
        try:
            import requests
            resp = requests.post(
                f"{self.api_url}/api/ml/syslog-clusters",
                json={"clusters": payload},
                timeout=30,
            )
            if not resp.ok:
                log.warning(
                    "Failed to post cluster centroids: HTTP %d", resp.status_code
                )
        except Exception as exc:
            log.warning("SyslogClusterer: failed to post centroids: %s", exc)


if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO)

    parser = argparse.ArgumentParser(description="Bonsai syslog cluster analysis")
    parser.add_argument("--api-url", default=os.environ.get("BONSAI_API_URL", DEFAULT_API_URL))
    parser.add_argument("--n-clusters", type=int, default=DEFAULT_N_CLUSTERS)
    parser.add_argument("--lookback-hours", type=int, default=168)
    args = parser.parse_args()

    clusterer = SyslogClusterer(
        api_url=args.api_url,
        n_clusters=args.n_clusters,
    )
    result = clusterer.run(lookback_hours=args.lookback_hours)
    if result.skipped:
        print(f"Skipped: {result.skip_reason}")
    else:
        print(
            f"Clustered {result.n_events_clustered} events into "
            f"{result.n_clusters} clusters (inertia={result.inertia:.2f})"
        )

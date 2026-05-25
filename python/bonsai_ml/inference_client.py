"""GNN inference write-back client for Bonsai (EV1-4 T4).

Wraps the POST /api/gnn/inference-results and POST /api/gnn/attention
endpoints so the STGNN inference loop can persist results and attention
weights in one call without duplicating HTTP logic.

Also exposes GnnResultsFetcher for querying past inference results via
GET /api/gnn/results.

Typical usage (inside inference_loop.py)::

    client = GnnInferenceClient(base_url="http://localhost:3000")
    client.post_inference_batch(snapshot_ns, model_id, device_scores)
    client.post_attention_batch(attention_rows)
    recent = client.get_results(device_address="10.0.0.1", limit=20)
"""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from typing import Any, Optional

log = logging.getLogger(__name__)


# ── Data classes ──────────────────────────────────────────────────────────────

@dataclass
class DeviceScore:
    """Per-device result from one GNN inference pass."""

    device_address: str
    anomaly_score: float
    threshold: float
    is_anomalous: bool
    uncertainty_margin: Optional[float] = None
    top_contributing_device_1: Optional[str] = None
    top_contributing_device_2: Optional[str] = None
    attention_weight_1: Optional[float] = None
    attention_weight_2: Optional[float] = None


@dataclass
class AttentionRow:
    """One directed attention edge from source to neighbour."""

    inference_result_id: str
    source_device_address: str
    neighbour_device_address: str
    edge_type: str
    attention_weight: float
    snapshot_ns: int


@dataclass
class InferenceWriteResult:
    """Summary returned after writing a batch to Bonsai."""

    written: int
    anomalous: int
    ok: bool
    error: Optional[str] = None


# ── Client ─────────────────────────────────────────────────────────────────────

class GnnInferenceClient:
    """HTTP client for writing GNN inference results to Bonsai core.

    Args:
        base_url: Root URL of the Bonsai HTTP server (no trailing slash).
        timeout:  Per-request timeout in seconds.
        max_retries: Number of retries on transient 5xx / connection errors.
    """

    def __init__(
        self,
        base_url: str = "http://localhost:3000",
        timeout: float = 30.0,
        max_retries: int = 3,
    ) -> None:
        self._base = base_url.rstrip("/")
        self._timeout = timeout
        self._max_retries = max_retries
        self._session: Any = None

    # ── Internal HTTP helpers ─────────────────────────────────────────────────

    def _session_get(self) -> Any:
        if self._session is None:
            try:
                import requests
                s = requests.Session()
                s.headers["Content-Type"] = "application/json"
                self._session = s
            except ImportError as exc:
                raise RuntimeError(
                    "requests library required: pip install requests"
                ) from exc
        return self._session

    def _post(self, path: str, payload: dict) -> dict:
        import requests

        url = f"{self._base}{path}"
        last_exc: Optional[Exception] = None
        for attempt in range(1, self._max_retries + 1):
            try:
                resp = self._session_get().post(url, json=payload, timeout=self._timeout)
                resp.raise_for_status()
                return resp.json()
            except requests.exceptions.RequestException as exc:
                last_exc = exc
                if attempt < self._max_retries:
                    backoff = 2 ** (attempt - 1)
                    log.warning("POST %s attempt %d failed: %s — retrying in %ds", path, attempt, exc, backoff)
                    time.sleep(backoff)
        raise RuntimeError(f"POST {path} failed after {self._max_retries} retries: {last_exc}") from last_exc

    def _get(self, path: str, params: Optional[dict] = None) -> dict:
        import requests

        url = f"{self._base}{path}"
        try:
            resp = self._session_get().get(url, params=params or {}, timeout=self._timeout)
            resp.raise_for_status()
            return resp.json()
        except requests.exceptions.RequestException as exc:
            raise RuntimeError(f"GET {path} failed: {exc}") from exc

    # ── Public API ────────────────────────────────────────────────────────────

    def post_inference_batch(
        self,
        snapshot_ns: int,
        model_id: str,
        scores: list[DeviceScore],
    ) -> InferenceWriteResult:
        """POST /api/gnn/inference-results — write one inference pass.

        Args:
            snapshot_ns: Unix timestamp (ns) of the graph snapshot this
                inference ran on.
            model_id: ModelArtifact.id of the model used.
            scores: Per-device anomaly scores.

        Returns:
            InferenceWriteResult with written/anomalous counts.
        """
        if not scores:
            return InferenceWriteResult(written=0, anomalous=0, ok=True)

        payload = {
            "snapshot_ns": snapshot_ns,
            "model_id": model_id,
            "results": [
                {
                    "device_address": s.device_address,
                    "anomaly_score": s.anomaly_score,
                    "uncertainty_margin": s.uncertainty_margin,
                    "threshold": s.threshold,
                    "is_anomalous": s.is_anomalous,
                    "top_contributing_device_1": s.top_contributing_device_1,
                    "top_contributing_device_2": s.top_contributing_device_2,
                    "attention_weight_1": s.attention_weight_1,
                    "attention_weight_2": s.attention_weight_2,
                }
                for s in scores
            ],
        }
        try:
            data = self._post("/api/gnn/inference-results", payload)
            return InferenceWriteResult(
                written=data.get("written", 0),
                anomalous=data.get("anomalous", 0),
                ok=True,
            )
        except Exception as exc:
            log.error("post_inference_batch failed: %s", exc)
            return InferenceWriteResult(written=0, anomalous=0, ok=False, error=str(exc))

    def post_attention_batch(self, rows: list[AttentionRow]) -> bool:
        """POST /api/gnn/attention — persist attention weight snapshots.

        Args:
            rows: Per-edge attention rows referencing inference_result_ids.

        Returns:
            True on success, False on error.
        """
        if not rows:
            return True

        payload = {
            "snapshots": [
                {
                    "inference_result_id": r.inference_result_id,
                    "source_device_address": r.source_device_address,
                    "neighbour_device_address": r.neighbour_device_address,
                    "edge_type": r.edge_type,
                    "attention_weight": r.attention_weight,
                    "snapshot_ns": r.snapshot_ns,
                }
                for r in rows
            ]
        }
        try:
            self._post("/api/gnn/attention", payload)
            return True
        except Exception as exc:
            log.error("post_attention_batch failed: %s", exc)
            return False

    def get_results(
        self,
        device_address: Optional[str] = None,
        since_ns: Optional[int] = None,
        limit: int = 100,
    ) -> list[dict]:
        """GET /api/gnn/results — query past inference results.

        Args:
            device_address: Filter to a specific device (optional).
            since_ns: Return only results after this ns timestamp.
            limit: Maximum rows to return (server cap: 500).

        Returns:
            List of result dicts with anomaly_score, is_anomalous, etc.
        """
        params: dict = {"limit": limit}
        if device_address:
            params["device_address"] = device_address
        if since_ns is not None:
            params["since_ns"] = since_ns
        try:
            data = self._get("/api/gnn/results", params)
            return data.get("results", [])
        except Exception as exc:
            log.error("get_results failed: %s", exc)
            return []

    def close(self) -> None:
        """Close the underlying requests.Session."""
        if self._session is not None:
            try:
                self._session.close()
            except Exception:
                pass
            self._session = None

    def __enter__(self) -> "GnnInferenceClient":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

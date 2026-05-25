"""STGNN continuous inference loop.

EV1-9 T3: StgnnInferenceLoop registers the `gnn_inference` job with BonsaiJobEngine.

Architecture:
  - Lazy model cache: reloads model only when model file mtime changes.
  - Fetches live graph snapshot via /api/graph/snapshot.
  - Appends to SnapshotStore (rolling T=8 Arrow IPC buffer).
  - If buffer >= 2 snapshots: runs STGNN forward pass.
  - POSTs per-device anomaly scores to /api/gnn/inference-results.
  - POSTs top-5 attention snapshots to /api/gnn/attention.
  - Emits GnnInferenceCompleted ML event.
  - Any exception in steps 1-10 is caught and logged — does NOT crash.
"""
from __future__ import annotations

import logging
import os
import time
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_API_URL = "http://localhost:3000"
DEFAULT_SNAPSHOT_DIR = "runtime/parquet/gnn_snapshots"
DEFAULT_MODEL_DIR = "models"
INFERENCE_INTERVAL_SECS = int(os.environ.get("BONSAI_GNN_INTERVAL_SECS", "300"))
MAX_ANOMALY_SCORE_THRESHOLD = float(os.environ.get("BONSAI_GNN_THRESHOLD", "0.5"))
TOP_K_ATTENTION = 5


class _ModelCache:
    """Lazy-loaded STGNN model with mtime-based reload."""

    def __init__(self, model_dir: str = DEFAULT_MODEL_DIR) -> None:
        self.model_dir = Path(model_dir)
        self._model: Optional[Any] = None
        self._model_path: Optional[Path] = None
        self._mtime: float = 0.0

    def get_model(self) -> Optional[Any]:
        """Return the active STGNN model, reloading if the file changed."""
        model_path = self._find_active_model()
        if model_path is None:
            return None

        try:
            mtime = model_path.stat().st_mtime
        except OSError:
            return self._model

        if self._model is not None and model_path == self._model_path and mtime == self._mtime:
            return self._model

        try:
            import torch
            from bonsai_ml.gnn.stgnn import build_stgnn
            model = build_stgnn()
            state = torch.load(str(model_path), map_location="cpu", weights_only=True)
            model.load_state_dict(state, strict=False)
            model.eval()
            self._model = model
            self._model_path = model_path
            self._mtime = mtime
            log.info("ModelCache: loaded %s (mtime=%s)", model_path.name, mtime)
        except Exception as exc:
            log.warning("ModelCache: could not load model from %s: %s", model_path, exc)

        return self._model

    def _find_active_model(self) -> Optional[Path]:
        """Return newest .pt file in model_dir, or None."""
        candidates = sorted(
            self.model_dir.glob("stgnn_*.pt"),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        if not candidates:
            return None
        return candidates[0]


class StgnnInferenceLoop:
    """Registers and manages the GNN inference job.

    Args:
        api_url: Bonsai core API URL.
        snapshot_dir: Directory for SnapshotStore Arrow IPC files.
        model_dir: Directory to scan for stgnn_*.pt model files.
        threshold: Anomaly classification threshold (default 0.5).
    """

    def __init__(
        self,
        api_url: str = DEFAULT_API_URL,
        snapshot_dir: str = DEFAULT_SNAPSHOT_DIR,
        model_dir: str = DEFAULT_MODEL_DIR,
        threshold: float = MAX_ANOMALY_SCORE_THRESHOLD,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.snapshot_dir = snapshot_dir
        self.threshold = threshold
        self._model_cache = _ModelCache(model_dir)

    def start(self, job_engine: Any) -> None:
        """Register the gnn_inference job with the job engine."""
        job_engine.register_job(
            "gnn_inference",
            self._run_inference,
            trigger_type="interval",
            seconds=INFERENCE_INTERVAL_SECS,
        )
        log.info(
            "InferenceLoop: registered gnn_inference (interval=%ds)", INFERENCE_INTERVAL_SECS
        )

    async def _run_inference(self, reporter: Any = None) -> Optional[dict]:
        """Full inference cycle. Returns result dict or None on failure."""
        try:
            model = self._model_cache.get_model()
            if model is None:
                log.warning("InferenceLoop: no active STGNN model found — skipping inference")
                return None

            snapshot = await self._fetch_graph_snapshot()
            if snapshot is None:
                return None

            hetero = self._to_hetero(snapshot)
            if hetero is None:
                return None

            from bonsai_ml.gnn.snapshot_store import SnapshotStore
            store = SnapshotStore(store_dir=self.snapshot_dir)
            store.write_snapshot(hetero, timestamp_ns=time.time_ns())

            buffer = store.load_buffer()
            if len(buffer) < 2:
                log.debug("InferenceLoop: buffer has %d snapshots — need ≥2", len(buffer))
                return None

            scores, attention = self._run_forward(model, buffer, hetero)
            if scores is None:
                return None

            result = await self._post_results(snapshot, scores, attention)
            self._emit_event(result)
            return result

        except Exception as exc:
            log.error("InferenceLoop: inference failed: %s", exc, exc_info=True)
            self._emit_failure_event(str(exc))
            return None

    async def _fetch_graph_snapshot(self) -> Optional[dict]:
        """GET /api/graph/snapshot — returns the current network snapshot."""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.get(
                    f"{self.api_url}/api/graph/snapshot",
                    timeout=aiohttp.ClientTimeout(total=30),
                ) as resp:
                    if resp.ok:
                        return await resp.json()
                    log.warning("InferenceLoop: snapshot API returned %s", resp.status)
        except Exception as exc:
            log.warning("InferenceLoop: could not fetch snapshot: %s", exc)
        return None

    def _to_hetero(self, snapshot: dict) -> Optional[Any]:
        """Convert API snapshot to PyG HeteroData."""
        try:
            from bonsai_ml.gnn.model import build_hetero_data
            return build_hetero_data(snapshot)
        except Exception as exc:
            log.warning("InferenceLoop: could not build HeteroData: %s", exc)
            return None

    def _run_forward(
        self, model: Any, buffer: list, current: Any
    ) -> tuple[Optional[dict], Optional[list]]:
        """Run STGNN forward pass. Returns (scores_dict, attention_list)."""
        try:
            import torch
            model.eval()
            with torch.no_grad():
                x_dict = {
                    k: current[k].x
                    for k in current.node_types
                    if current[k].x is not None
                }
                edge_dict = {e: current[e].edge_index for e in current.edge_types}
                out = model(x_dict, edge_dict)

            device_ids = getattr(current["device"], "node_ids", [])
            if "device" not in out:
                return None, None

            probs = torch.softmax(out["device"], dim=-1)[:, 1].cpu().numpy()
            scores = {
                dev_id: float(score)
                for dev_id, score in zip(device_ids, probs)
            }

            attention_list = None
            try:
                from bonsai_ml.gnn.stgnn import extract_attention_snapshots
                agg = out.get("_attention", {})
                if agg:
                    snaps = extract_attention_snapshots(agg, scores, current, top_k=TOP_K_ATTENTION)
                    attention_list = [
                        {
                            "device_address": s.node_id,
                            "anomaly_score": s.anomaly_score,
                            "neighbours": [
                                {"address": n.address, "weight": n.attention_weight}
                                for n in s.neighbours
                            ],
                        }
                        for s in snaps
                    ]
            except Exception as exc:
                log.debug("Could not extract attention snapshots: %s", exc)

            return scores, attention_list

        except Exception as exc:
            log.error("InferenceLoop: forward pass failed: %s", exc)
            return None, None

    async def _post_results(
        self, snapshot: dict, scores: dict, attention: Optional[list]
    ) -> dict:
        """POST inference results and attention to API. Returns summary dict."""
        anomalous = {k: v for k, v in scores.items() if v >= self.threshold}
        top_device = max(scores, key=scores.get) if scores else None

        results_payload = {
            "inference_at_ns": time.time_ns(),
            "model_threshold": self.threshold,
            "total_devices": len(scores),
            "anomalous_count": len(anomalous),
            "top_device": top_device,
            "top_score": scores.get(top_device, 0.0) if top_device else 0.0,
            "results": [
                {
                    "device_address": dev,
                    "anomaly_score": score,
                    "is_anomalous": score >= self.threshold,
                }
                for dev, score in scores.items()
            ],
        }

        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.post(
                    f"{self.api_url}/api/gnn/inference-results",
                    json=results_payload,
                    timeout=aiohttp.ClientTimeout(total=10),
                ):
                    pass

                if attention:
                    async with session.post(
                        f"{self.api_url}/api/gnn/attention",
                        json={"snapshots": attention},
                        timeout=aiohttp.ClientTimeout(total=10),
                    ):
                        pass
        except Exception as exc:
            log.debug("InferenceLoop: could not POST results: %s", exc)

        return {
            "total_devices": len(scores),
            "anomalous_count": len(anomalous),
            "top_device": top_device,
            "top_score": scores.get(top_device, 0.0) if top_device else 0.0,
            "inference_at_ns": time.time_ns(),
        }

    def _emit_event(self, result: Optional[dict]) -> None:
        if not result:
            return
        try:
            import requests
            requests.post(
                f"{self.api_url}/api/ml/events/publish",
                json={"event_type": "gnn_inference_complete", "payload": result},
                timeout=2,
            )
        except Exception:
            pass

    def _emit_failure_event(self, error: str) -> None:
        try:
            import requests
            requests.post(
                f"{self.api_url}/api/ml/events/publish",
                json={
                    "event_type": "gnn_inference_failed",
                    "payload": {"error": error, "failed_at_ns": time.time_ns()},
                },
                timeout=2,
            )
        except Exception:
            pass

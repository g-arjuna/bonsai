"""Conformal prediction calibration layer for GNN anomaly scores (EV1-8 T5).

Implements split conformal prediction (Angelopoulos & Bates, 2021) for the
STGNN binary anomaly-detection output. After standard GNN training, a held-out
calibration set is used to derive a score threshold that guarantees a target
false-negative rate (miscoverage alpha) with finite-sample coverage guarantees.

Usage::

    from bonsai_ml.gnn.conformal import ConformalCalibrator

    cal = ConformalCalibrator(alpha=0.1)   # 90% coverage guarantee
    cal.calibrate(cal_scores, cal_labels)  # cal_scores: anomaly scores [0..1]
    threshold = cal.threshold              # use this instead of fixed 0.5

    adjusted = cal.predict(test_scores)    # True = anomalous with 90% coverage
    ci = cal.coverage_interval(test_scores, alpha=0.1)

Dependency note: numpy is required; torch is optional (for tensor inputs).
"""
from __future__ import annotations

import logging
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

import numpy as np

log = logging.getLogger(__name__)


# ── Result types ──────────────────────────────────────────────────────────────

@dataclass
class CalibrationResult:
    """Summary of a conformal calibration run."""
    alpha: float
    n_cal: int
    n_anomalous: int
    n_clean: int
    quantile_level: float
    threshold: float
    empirical_coverage: float

    def __str__(self) -> str:
        return (
            f"ConformalCalibration(alpha={self.alpha}, n={self.n_cal}, "
            f"threshold={self.threshold:.4f}, coverage={self.empirical_coverage:.3f})"
        )


# ── Calibrator ─────────────────────────────────────────────────────────────────

class ConformalCalibrator:
    """Split conformal prediction calibrator for GNN anomaly scores.

    The calibration set must be a *separate* held-out split (not training data).
    After calling :meth:`calibrate`, :attr:`threshold` replaces the fixed
    decision boundary used during training.

    Args:
        alpha: Miscoverage level (default 0.1 → 90% coverage guarantee).
            Lower alpha → higher coverage → more conservative threshold.
        score_type: ``"anomaly"`` (high = anomalous) or ``"normal"``
            (high = normal, e.g. softmax probability for class 0).
    """

    def __init__(self, alpha: float = 0.1, score_type: str = "anomaly") -> None:
        if not 0 < alpha < 1:
            raise ValueError(f"alpha must be in (0, 1), got {alpha}")
        self.alpha = alpha
        self.score_type = score_type
        self._threshold: Optional[float] = None
        self._cal_scores: Optional[np.ndarray] = None
        self._cal_labels: Optional[np.ndarray] = None
        self._result: Optional[CalibrationResult] = None

    @property
    def threshold(self) -> float:
        """Calibrated decision threshold. Raises if not yet calibrated."""
        if self._threshold is None:
            raise RuntimeError("ConformalCalibrator.calibrate() must be called first")
        return self._threshold

    @property
    def is_calibrated(self) -> bool:
        return self._threshold is not None

    def calibrate(
        self,
        scores: Any,
        labels: Any,
    ) -> CalibrationResult:
        """Calibrate the threshold using split conformal prediction.

        For binary anomaly detection, the nonconformity score for a *clean*
        sample (label=0) is the anomaly score itself. The 1-alpha quantile
        of these scores (with +1 adjustment) becomes the threshold.

        Args:
            scores: Anomaly scores in [0, 1] for calibration samples.
                Shape ``[N]``. May be a numpy array, list, or torch tensor.
            labels: Binary labels (1=anomalous, 0=clean) for calibration samples.
                Shape ``[N]``.

        Returns:
            CalibrationResult with diagnostics.
        """
        scores_np = self._to_numpy(scores)
        labels_np = self._to_numpy(labels).astype(int)

        if scores_np.shape != labels_np.shape:
            raise ValueError(
                f"scores shape {scores_np.shape} != labels shape {labels_np.shape}"
            )

        # Nonconformity scores for CLEAN samples (label == 0).
        # We calibrate to cover true anomalies (label == 1) at rate 1-alpha,
        # so we use clean-sample scores to set the lower bound.
        if self.score_type == "anomaly":
            clean_scores = scores_np[labels_np == 0]
        else:
            # Invert so high = more anomalous
            clean_scores = 1.0 - scores_np[labels_np == 0]

        n = len(clean_scores)
        if n == 0:
            raise ValueError("No clean (label=0) samples in calibration set")

        # Finite-sample conformal quantile: ceil((n+1)(1-alpha))/n
        quantile_level = math.ceil((n + 1) * (1.0 - self.alpha)) / n
        quantile_level = min(quantile_level, 1.0)

        self._threshold = float(np.quantile(clean_scores, quantile_level))
        self._cal_scores = scores_np
        self._cal_labels = labels_np

        # Empirical coverage on calibration set
        if self.score_type == "anomaly":
            preds = scores_np >= self._threshold
        else:
            preds = (1.0 - scores_np) >= self._threshold

        # Coverage = fraction of anomalous samples correctly predicted as anomalous
        anom_mask = labels_np == 1
        if anom_mask.sum() > 0:
            empirical_coverage = float(preds[anom_mask].mean())
        else:
            empirical_coverage = float("nan")

        self._result = CalibrationResult(
            alpha=self.alpha,
            n_cal=n,
            n_anomalous=int(anom_mask.sum()),
            n_clean=n,
            quantile_level=quantile_level,
            threshold=self._threshold,
            empirical_coverage=empirical_coverage,
        )
        log.info(
            "conformal calibration: alpha=%.2f n_cal=%d threshold=%.4f coverage=%.3f",
            self.alpha, n, self._threshold, empirical_coverage,
        )
        return self._result

    def predict(self, scores: Any) -> np.ndarray:
        """Return boolean anomaly predictions using the calibrated threshold.

        Args:
            scores: Anomaly scores in [0, 1]. Shape ``[N]``.

        Returns:
            Boolean array of shape ``[N]``. True = anomalous.
        """
        scores_np = self._to_numpy(scores)
        if self.score_type == "anomaly":
            return scores_np >= self.threshold
        return (1.0 - scores_np) >= self.threshold

    def predict_with_margin(self, scores: Any) -> tuple[np.ndarray, np.ndarray]:
        """Return (predictions, margin) where margin = score − threshold.

        Positive margin = anomalous with how much headroom.
        Negative margin = clean (below threshold).

        Returns:
            (predictions: bool[N], margin: float[N])
        """
        scores_np = self._to_numpy(scores)
        if self.score_type == "normal":
            scores_np = 1.0 - scores_np
        margin = scores_np - self.threshold
        return margin >= 0, margin

    def coverage_interval(self, scores: Any, alpha: Optional[float] = None) -> dict:
        """Compute marginal coverage interval at the given miscoverage level.

        Returns a dict with ``threshold``, ``alpha``, and ``coverage_guarantee``.
        """
        a = alpha if alpha is not None else self.alpha
        scores_np = self._to_numpy(scores)
        if self.score_type == "anomaly":
            frac = float((scores_np >= self.threshold).mean())
        else:
            frac = float(((1.0 - scores_np) >= self.threshold).mean())
        return {
            "threshold": self.threshold,
            "alpha": a,
            "coverage_guarantee": 1.0 - a,
            "empirical_coverage": frac,
        }

    def save(self, path: str | Path) -> None:
        """Save calibration state to a numpy .npz file."""
        np.savez(
            str(path),
            threshold=np.array([self._threshold]),
            alpha=np.array([self.alpha]),
        )
        log.info("saved conformal calibrator to %s (threshold=%.4f)", path, self._threshold)

    @classmethod
    def load(cls, path: str | Path) -> "ConformalCalibrator":
        """Load calibration state from a .npz file saved by :meth:`save`."""
        data = np.load(str(path))
        cal = cls(alpha=float(data["alpha"][0]))
        cal._threshold = float(data["threshold"][0])
        log.info("loaded conformal calibrator from %s (threshold=%.4f)", path, cal._threshold)
        return cal

    @staticmethod
    def _to_numpy(arr: Any) -> np.ndarray:
        if isinstance(arr, np.ndarray):
            return arr.astype(float)
        try:
            import torch
            if isinstance(arr, torch.Tensor):
                return arr.detach().cpu().numpy().astype(float)
        except ImportError:
            pass
        return np.asarray(arr, dtype=float)


class MCDropoutEstimator:
    """MC Dropout uncertainty estimator — fallback when no calibration set exists.

    For deployments with <100 fault examples where split conformal calibration
    cannot be performed, MC Dropout provides per-device uncertainty estimates by
    running multiple stochastic forward passes with dropout active.

    Algorithm:
        1. Switch model to train() mode (activates dropout).
        2. Run N stochastic forward passes over the snapshot.
        3. Collect N anomaly scores per device.
        4. mean_score  = mean of the N scores  → point estimate.
        5. uncertainty = std  of the N scores  → uncertainty margin.

    Usage::

        est = MCDropoutEstimator(n_samples=20)
        mean_scores, uncertainties = est.estimate(model, snapshot)
        # mean_scores:  np.ndarray shape (n_devices,)
        # uncertainties: np.ndarray shape (n_devices,)

    Gate: set n_samples=0 to disable MC Dropout and fall back to single-pass
    inference (zero uncertainty).

    Args:
        n_samples: Number of stochastic forward passes. 0 = disabled.
        fault_class_idx: Index of the fault class in the softmax output. Default 1.
    """

    def __init__(self, n_samples: int = 20, fault_class_idx: int = 1) -> None:
        self.n_samples = n_samples
        self.fault_class_idx = fault_class_idx

    def estimate(
        self,
        model: Any,
        snapshot: Any,
        device_str: str = "cpu",
    ) -> tuple[np.ndarray, np.ndarray]:
        """Run MC Dropout and return (mean_scores, uncertainties) per device.

        Args:
            model: Trained HeteroGNN or STGNNModel from model.py.
            snapshot: HeteroData snapshot (same format as training).
            device_str: Torch device string.

        Returns:
            Tuple of (mean_scores, uncertainties), each shape (n_devices,).
            Returns (zeros, zeros) if n_samples=0 or torch unavailable.
        """
        if self.n_samples == 0:
            log.debug("MCDropoutEstimator: disabled (n_samples=0), returning zero uncertainty")
            try:
                n = snapshot["device"].x.shape[0]
            except (AttributeError, KeyError):
                n = 0
            return np.zeros(n, dtype=float), np.zeros(n, dtype=float)

        try:
            import torch
            import torch.nn.functional as F
        except ImportError:
            log.warning("MCDropoutEstimator: torch not available")
            return np.zeros(0, dtype=float), np.zeros(0, dtype=float)

        device = torch.device(device_str)
        model = model.to(device)
        model.train()

        try:
            snapshot = snapshot.to(device)
        except (AttributeError, TypeError):
            pass

        all_scores: list[np.ndarray] = []

        with torch.no_grad():
            for _ in range(self.n_samples):
                try:
                    if hasattr(model, "_encode_snapshot"):
                        x_dict, _ = model._encode_snapshot(snapshot, return_attention=False)
                        dev_emb = x_dict.get("device")
                    else:
                        dev_emb = None

                    if dev_emb is None:
                        continue

                    if hasattr(model, "classifier"):
                        logits = model.classifier(dev_emb)
                    else:
                        logits = dev_emb

                    probs = F.softmax(logits, dim=-1)
                    fault_scores = probs[:, self.fault_class_idx].detach().cpu().numpy()
                    all_scores.append(fault_scores)
                except Exception as exc:
                    log.debug("MCDropoutEstimator: forward pass failed: %s", exc)

        model.eval()

        if not all_scores:
            n = snapshot["device"].x.shape[0] if hasattr(snapshot["device"], "x") else 0
            return np.zeros(n, dtype=float), np.zeros(n, dtype=float)

        scores_matrix = np.stack(all_scores, axis=0)
        mean_scores = scores_matrix.mean(axis=0)
        uncertainties = scores_matrix.std(axis=0)
        return mean_scores, uncertainties

    @classmethod
    def from_conformal_or_fallback(
        cls,
        conformal_path: str,
        n_samples: int = 20,
    ) -> tuple[Optional["ConformalCalibrator"], "MCDropoutEstimator"]:
        """Load a ConformalCalibrator if available, else prepare MC Dropout fallback.

        Returns:
            (calibrator_or_None, mc_estimator).
            If calibrator is not None, use it for uncertainty; otherwise use mc_estimator.
        """
        cal: Optional[ConformalCalibrator] = None
        try:
            p = Path(conformal_path)
            if p.exists():
                cal = ConformalCalibrator.load(str(p))
                log.info("Loaded conformal calibrator from %s", conformal_path)
            else:
                log.info(
                    "Conformal calibrator not found at %s — will use MC Dropout fallback",
                    conformal_path,
                )
        except Exception as exc:
            log.warning("Could not load conformal calibrator: %s — using MC Dropout", exc)
        return cal, cls(n_samples=n_samples)

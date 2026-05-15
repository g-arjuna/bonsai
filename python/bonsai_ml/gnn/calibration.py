"""GNN calibration phase support.

D5-T4 (DV1): Implements the calibration-phase toggle described in the CV5 GNN
philosophy. During calibration, GNN anomaly scores are computed and persisted
to a ``gnn_calibration_scores`` table but do not flow to the Detection table.
The operator reviews the 7-day score distribution and flips to production.

Deployment flow:
  1. bonsai.toml: ``[gnn] inference_mode = "calibration"``
  2. Run bonsai + trained GNN model for ≥7 days.
  3. Call ``CalibrationStore.summary()`` to inspect score distribution.
  4. If distribution looks sane, set ``inference_mode = "production"`` and restart.

This module is pure-Python (no torch dependency at import time). Scores are
stored as simple JSON records in a line-delimited file (``gnn_calibration_scores.ndjson``
in the runtime directory), making them inspectable without any tooling.
"""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any


CALIBRATION_SCORES_FILENAME = "gnn_calibration_scores.ndjson"
PRODUCTION_MIN_SAMPLES_DEFAULT = 1000


@dataclass
class CalibrationScoreRecord:
    """One score record written during calibration phase."""

    timestamp_ns: int
    snapshot_ns: int
    node_id: str
    node_type: str
    anomaly_score: float
    model_tag: str
    metadata: dict = None

    def __post_init__(self):
        if self.metadata is None:
            self.metadata = {}


@dataclass
class CalibrationSummary:
    """Summary statistics over the accumulated calibration score records."""

    num_records: int
    num_nodes: int
    mean_score: float
    p50_score: float
    p90_score: float
    p99_score: float
    max_score: float
    fraction_above_threshold: float
    threshold: float
    ready_for_production: bool
    min_samples_required: int
    notes: str = ""


class CalibrationStore:
    """Reads and writes GNN calibration score records.

    Args:
        runtime_dir: Path to the bonsai runtime directory.
        threshold: Score threshold used in the summary readiness check.
        min_samples: Minimum record count before ``ready_for_production`` is True.
    """

    def __init__(
        self,
        runtime_dir: str | Path = "runtime",
        threshold: float = 0.5,
        min_samples: int = PRODUCTION_MIN_SAMPLES_DEFAULT,
    ) -> None:
        self.path = Path(runtime_dir) / CALIBRATION_SCORES_FILENAME
        self.threshold = threshold
        self.min_samples = min_samples

    def write(self, record: CalibrationScoreRecord) -> None:
        """Append one score record to the calibration store (append-only)."""
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(asdict(record)) + "\n")

    def write_batch(self, records: list[CalibrationScoreRecord]) -> None:
        """Append multiple records in one file open."""
        if not records:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as fh:
            for record in records:
                fh.write(json.dumps(asdict(record)) + "\n")

    def load(self) -> list[CalibrationScoreRecord]:
        """Load all records from the calibration store."""
        if not self.path.exists():
            return []
        records = []
        with self.path.open("r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    d = json.loads(line)
                    records.append(CalibrationScoreRecord(**d))
                except (json.JSONDecodeError, TypeError):
                    continue
        return records

    def summary(self) -> CalibrationSummary:
        """Compute summary statistics over all accumulated score records."""
        records = self.load()
        if not records:
            return CalibrationSummary(
                num_records=0,
                num_nodes=0,
                mean_score=0.0,
                p50_score=0.0,
                p90_score=0.0,
                p99_score=0.0,
                max_score=0.0,
                fraction_above_threshold=0.0,
                threshold=self.threshold,
                ready_for_production=False,
                min_samples_required=self.min_samples,
                notes="No calibration records found.",
            )

        scores = sorted(r.anomaly_score for r in records)
        n = len(scores)
        node_ids = {r.node_id for r in records}

        def percentile(p: float) -> float:
            idx = int(p / 100.0 * (n - 1))
            return scores[min(idx, n - 1)]

        mean_score = sum(scores) / n
        p50 = percentile(50)
        p90 = percentile(90)
        p99 = percentile(99)
        max_score = scores[-1]
        above = sum(1 for s in scores if s >= self.threshold)
        frac = above / n

        ready = n >= self.min_samples
        notes = (
            f"Ready for production transition (≥{self.min_samples} samples collected)."
            if ready
            else f"Not yet ready: {n}/{self.min_samples} samples collected."
        )

        return CalibrationSummary(
            num_records=n,
            num_nodes=len(node_ids),
            mean_score=mean_score,
            p50_score=p50,
            p90_score=p90,
            p99_score=p99,
            max_score=max_score,
            fraction_above_threshold=frac,
            threshold=self.threshold,
            ready_for_production=ready,
            min_samples_required=self.min_samples,
            notes=notes,
        )

    def record_count(self) -> int:
        """Return the number of records without loading all data into memory."""
        if not self.path.exists():
            return 0
        count = 0
        with self.path.open("r", encoding="utf-8") as fh:
            for line in fh:
                if line.strip():
                    count += 1
        return count


def make_calibration_record(
    node_id: str,
    anomaly_score: float,
    model_tag: str,
    node_type: str = "device",
    snapshot_ns: int | None = None,
    metadata: dict[str, Any] | None = None,
) -> CalibrationScoreRecord:
    """Convenience constructor for a calibration record with the current timestamp."""
    now_ns = int(time.time() * 1e9)
    return CalibrationScoreRecord(
        timestamp_ns=now_ns,
        snapshot_ns=snapshot_ns if snapshot_ns is not None else now_ns,
        node_id=node_id,
        node_type=node_type,
        anomaly_score=float(anomaly_score),
        model_tag=model_tag,
        metadata=metadata or {},
    )

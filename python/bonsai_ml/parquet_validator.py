"""Parquet file validator for Bonsai ML training data.

EV1-2 T3: Validates schema, label balance, and detects drift between exports.

Called by ParquetExportJob after every export run. Results stored in the
ParquetExport catalog record as quality_json.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

MIN_LABEL_BALANCE_PCT = 5.0
MAX_LABEL_BALANCE_PCT = 50.0
MIN_ROW_COUNT = 10
LABEL_DRIFT_THRESHOLD = 0.3
FEATURE_DRIFT_PSI_THRESHOLD = 0.2


REQUIRED_COLUMNS_BY_TYPE: dict[str, list[str]] = {
    "anomaly": ["label", "device_address"],
    "remediation": ["label", "action_type"],
}


@dataclass
class ValidationResult:
    """Full validation result for a Parquet file."""
    file_path: str
    export_type: str
    schema_version: str = "device_v2"
    row_count: int = 0
    anomaly_rows: int = 0
    normal_rows: int = 0
    class_balance_pct: float = 0.0
    label_drift_score: float = 0.0
    feature_drift_worst_column: str = ""
    feature_drift_worst_psi: float = 0.0
    missing_columns: list[str] = field(default_factory=list)
    all_null_columns: list[str] = field(default_factory=list)
    quality_passed: bool = False
    failure_reasons: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "file_path": self.file_path,
            "export_type": self.export_type,
            "schema_version": self.schema_version,
            "row_count": self.row_count,
            "anomaly_rows": self.anomaly_rows,
            "normal_rows": self.normal_rows,
            "class_balance_pct": self.class_balance_pct,
            "label_drift_score": self.label_drift_score,
            "feature_drift_worst_column": self.feature_drift_worst_column,
            "feature_drift_worst_psi": self.feature_drift_worst_psi,
            "missing_columns": self.missing_columns,
            "all_null_columns": self.all_null_columns,
            "quality_passed": self.quality_passed,
            "failure_reasons": self.failure_reasons,
        }


class ParquetValidator:
    """Validates Parquet files and detects drift from a reference file."""

    def validate(
        self,
        path: str,
        export_type: str,
        schema_version: str = "device_v2",
        reference_path: Optional[str] = None,
    ) -> ValidationResult:
        """Run all checks on a Parquet file.

        Args:
            path: Filesystem path to the .parquet file.
            export_type: "anomaly" or "remediation" (selects required columns).
            schema_version: Feature schema version string for the record.
            reference_path: Optional previous export to compute drift against.

        Returns:
            ValidationResult with pass/fail and detailed diagnostics.
        """
        result = ValidationResult(
            file_path=path,
            export_type=export_type,
            schema_version=schema_version,
        )

        try:
            df = self._read_parquet(path)
        except Exception as exc:
            result.failure_reasons.append(f"file_unreadable: {exc}")
            return result

        if len(df) == 0:
            result.failure_reasons.append("empty_file")
            return result

        result.row_count = len(df)

        self._check_required_columns(df, export_type, result)

        self._check_null_columns(df, result)

        if "label" in df.columns:
            self._check_label_balance(df, result)

        if reference_path and Path(reference_path).exists():
            try:
                ref_df = self._read_parquet(reference_path)
                self._compute_label_drift(df, ref_df, result)
                self._compute_feature_drift(df, ref_df, result)
            except Exception as exc:
                log.debug("Drift computation failed: %s", exc)

        result.quality_passed = len(result.failure_reasons) == 0
        return result

    def _read_parquet(self, path: str) -> Any:
        try:
            import pyarrow.parquet as pq
            return pq.read_table(path).to_pandas()
        except ImportError:
            import pandas as pd
            return pd.read_parquet(path)

    def _check_required_columns(
        self, df: Any, export_type: str, result: ValidationResult
    ) -> None:
        required = REQUIRED_COLUMNS_BY_TYPE.get(export_type, [])
        for col in required:
            if col not in df.columns:
                result.missing_columns.append(col)
                result.failure_reasons.append(f"missing_column:{col}")

    def _check_null_columns(self, df: Any, result: ValidationResult) -> None:
        for col in df.columns:
            if df[col].isna().all():
                result.all_null_columns.append(col)
                result.failure_reasons.append(f"all_null_column:{col}")

    def _check_label_balance(self, df: Any, result: ValidationResult) -> None:
        try:
            label_counts = df["label"].value_counts()
            total = len(df)
            anomaly = int(label_counts.get(1, label_counts.get(True, 0)))
            normal = int(label_counts.get(0, label_counts.get(False, 0)))
            result.anomaly_rows = anomaly
            result.normal_rows = normal
            balance_pct = (anomaly / total * 100.0) if total > 0 else 0.0
            result.class_balance_pct = round(balance_pct, 2)

            if total < MIN_ROW_COUNT:
                result.failure_reasons.append(f"row_count_too_low:{total}")
            if balance_pct < MIN_LABEL_BALANCE_PCT:
                result.failure_reasons.append(
                    f"label_imbalance_too_low:{balance_pct:.1f}%"
                )
            if balance_pct > MAX_LABEL_BALANCE_PCT:
                result.failure_reasons.append(
                    f"label_imbalance_too_high:{balance_pct:.1f}%"
                )
        except Exception as exc:
            log.debug("Label balance check failed: %s", exc)

    def _compute_label_drift(
        self, current_df: Any, ref_df: Any, result: ValidationResult
    ) -> None:
        """Jensen-Shannon divergence on label distribution."""
        try:
            import numpy as np
            from scipy.spatial.distance import jensenshannon

            def label_dist(df: Any) -> list[float]:
                n = len(df)
                if n == 0:
                    return [0.5, 0.5]
                c = df["label"].value_counts()
                p_pos = float(c.get(1, c.get(True, 0))) / n
                return [1.0 - p_pos, p_pos]

            p = label_dist(current_df)
            q = label_dist(ref_df)
            js = float(jensenshannon(p, q))
            result.label_drift_score = round(js, 4)
            if js > LABEL_DRIFT_THRESHOLD:
                result.failure_reasons.append(f"label_drift_high:{js:.3f}")
        except ImportError:
            pass
        except Exception as exc:
            log.debug("Label drift computation failed: %s", exc)

    def _compute_feature_drift(
        self, current_df: Any, ref_df: Any, result: ValidationResult
    ) -> None:
        """Per-column Population Stability Index (PSI) for numeric features."""
        try:
            import numpy as np

            numeric_cols = [
                col for col in current_df.columns
                if col not in ("label", "device_address", "action_type", "id")
                and current_df[col].dtype in ("float32", "float64", "int32", "int64")
                and col in ref_df.columns
            ]

            worst_psi = 0.0
            worst_col = ""

            for col in numeric_cols:
                try:
                    psi = _compute_psi(
                        current_df[col].dropna().values,
                        ref_df[col].dropna().values,
                    )
                    if psi > worst_psi:
                        worst_psi = psi
                        worst_col = col
                except Exception:
                    continue

            result.feature_drift_worst_column = worst_col
            result.feature_drift_worst_psi = round(worst_psi, 4)

            if worst_psi > FEATURE_DRIFT_PSI_THRESHOLD:
                result.failure_reasons.append(
                    f"feature_drift_high:{worst_col}={worst_psi:.3f}"
                )
        except Exception as exc:
            log.debug("Feature drift computation failed: %s", exc)


def _compute_psi(current: Any, reference: Any, buckets: int = 10) -> float:
    """Compute Population Stability Index between two 1D arrays."""
    import numpy as np

    if len(current) == 0 or len(reference) == 0:
        return 0.0

    bins = np.percentile(reference, [i * 100 / buckets for i in range(1, buckets)])
    bins = np.unique(bins)
    if len(bins) == 0:
        return 0.0

    cur_counts, _ = np.histogram(current, bins=np.concatenate([[-np.inf], bins, [np.inf]]))
    ref_counts, _ = np.histogram(reference, bins=np.concatenate([[-np.inf], bins, [np.inf]]))

    cur_pct = cur_counts / max(1, cur_counts.sum())
    ref_pct = ref_counts / max(1, ref_counts.sum())

    cur_pct = np.where(cur_pct == 0, 1e-6, cur_pct)
    ref_pct = np.where(ref_pct == 0, 1e-6, ref_pct)

    psi = float(np.sum((cur_pct - ref_pct) * np.log(cur_pct / ref_pct)))
    return max(0.0, psi)

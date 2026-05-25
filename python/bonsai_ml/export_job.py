"""Parquet export job with catalog integration for Bonsai ML pipeline.

EV1-2 T2: ParquetExportJob + IncrementalExportJob.

Every export creates a catalog record in the Bonsai DB via /api/ml/exports,
runs the export, validates quality, and updates the record with results.

Usage:
    python -m bonsai_ml.export_job --type anomaly --incremental
    python -m bonsai_ml.export_job --type remediation --full
"""
from __future__ import annotations

import argparse
import logging
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_API_URL = "http://localhost:3000"
DEFAULT_OUTPUT_DIR = "runtime/parquet"


@dataclass
class ExportQualityReport:
    """Quality metrics for a Parquet export run."""
    row_count: int = 0
    anomaly_rows: int = 0
    normal_rows: int = 0
    class_balance_ratio: float = 0.0
    label_drift_score: float = 0.0
    feature_drift_worst_column: str = ""
    feature_drift_worst_psi: float = 0.0
    missing_columns: list[str] = field(default_factory=list)
    schema_version: str = ""
    quality_passed: bool = False
    failure_reasons: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "row_count": self.row_count,
            "anomaly_rows": self.anomaly_rows,
            "normal_rows": self.normal_rows,
            "class_balance_ratio": self.class_balance_ratio,
            "label_drift_score": self.label_drift_score,
            "feature_drift_worst_column": self.feature_drift_worst_column,
            "feature_drift_worst_psi": self.feature_drift_worst_psi,
            "missing_columns": self.missing_columns,
            "schema_version": self.schema_version,
            "quality_passed": self.quality_passed,
            "failure_reasons": self.failure_reasons,
        }


@dataclass
class ExportResult:
    """Outcome of a single export job run."""
    export_id: str
    export_type: str
    output_path: str
    row_count: int
    quality_passed: bool
    quality_report: ExportQualityReport
    started_at_ns: int = 0
    completed_at_ns: int = 0
    error: str = ""


class ParquetExportJob:
    """Runs a full Parquet export, creates catalog records, validates quality.

    Args:
        api_url: Base URL of the Bonsai core API.
        output_dir: Root directory for parquet output. Subdirectories are
            created per export_type.
    """

    def __init__(
        self,
        api_url: str = DEFAULT_API_URL,
        output_dir: str = DEFAULT_OUTPUT_DIR,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.output_dir = Path(output_dir)

    def run(
        self,
        export_type: str,
        since_ns: Optional[int] = None,
        until_ns: Optional[int] = None,
        extra_label: str = "",
    ) -> ExportResult:
        """Execute a full export for the given type.

        Args:
            export_type: "anomaly" or "remediation".
            since_ns: Start of time window (nanoseconds). None = all history.
            until_ns: End of time window. None = current time.
            extra_label: Optional label suffix for the output filename.

        Returns:
            ExportResult with path, row count, and quality report.
        """
        import requests

        started_ns = time.time_ns()
        if until_ns is None:
            until_ns = started_ns

        catalog_record = self._create_catalog_record(
            export_type=export_type,
            since_ns=since_ns,
            until_ns=until_ns,
        )
        export_id = catalog_record.get("id", f"local-{started_ns}")

        output_path = self._build_output_path(export_type, started_ns, extra_label)

        log.info(
            "ParquetExportJob: starting %s export (id=%s) → %s",
            export_type, export_id, output_path,
        )

        error = ""
        row_count = 0
        quality_report = ExportQualityReport()

        try:
            row_count = self._run_export(
                export_type=export_type,
                output_path=output_path,
                since_ns=since_ns,
                until_ns=until_ns,
            )

            from .parquet_validator import ParquetValidator
            validator = ParquetValidator()
            quality_report = validator.validate(str(output_path), export_type)

            log.info(
                "ParquetExportJob: %s export complete — %d rows, quality_passed=%s",
                export_type, row_count, quality_report.quality_passed,
            )

        except Exception as exc:
            error = str(exc)
            log.error("ParquetExportJob: export failed: %s", error)

        completed_ns = time.time_ns()

        self._update_catalog_record(
            export_id=export_id,
            output_path=str(output_path),
            row_count=row_count,
            anomaly_rows=quality_report.anomaly_rows,
            normal_rows=quality_report.normal_rows,
            status="completed" if not error else "failed",
            error_message=error,
            quality_report=quality_report,
        )

        return ExportResult(
            export_id=export_id,
            export_type=export_type,
            output_path=str(output_path),
            row_count=row_count,
            quality_passed=quality_report.quality_passed,
            quality_report=quality_report,
            started_at_ns=started_ns,
            completed_at_ns=completed_ns,
            error=error,
        )

    def _run_export(
        self,
        export_type: str,
        output_path: Path,
        since_ns: Optional[int],
        until_ns: Optional[int],
    ) -> int:
        """Call bonsai_sdk training export functions and write Parquet file."""
        output_path.parent.mkdir(parents=True, exist_ok=True)

        try:
            from bonsai_sdk.training import (
                export_training_set,
                export_remediation_training_set,
            )
        except ImportError:
            log.warning("bonsai_sdk.training not available; writing empty parquet")
            self._write_empty_parquet(output_path, export_type)
            return 0

        if export_type == "anomaly":
            df = export_training_set(
                api_url=self.api_url,
                since_ns=since_ns,
                until_ns=until_ns,
            )
        elif export_type == "remediation":
            df = export_remediation_training_set(
                api_url=self.api_url,
                since_ns=since_ns,
                until_ns=until_ns,
            )
        else:
            raise ValueError(f"Unknown export_type: {export_type!r}")

        try:
            import pyarrow as pa
            import pyarrow.parquet as pq
            table = pa.Table.from_pandas(df)
            pq.write_table(table, str(output_path))
        except ImportError:
            df.to_parquet(str(output_path), index=False)

        return len(df)

    def _write_empty_parquet(self, output_path: Path, export_type: str) -> None:
        try:
            import pandas as pd
            pd.DataFrame().to_parquet(str(output_path), index=False)
        except ImportError:
            output_path.touch()

    def _build_output_path(self, export_type: str, ts_ns: int, label: str) -> Path:
        from datetime import datetime, timezone
        ts = datetime.fromtimestamp(ts_ns / 1e9, tz=timezone.utc)
        ts_str = ts.strftime("%Y-%m-%dT%H-%M-%SZ")
        suffix = f"_{label}" if label else ""
        filename = f"{ts_str}{suffix}.parquet"
        return self.output_dir / export_type / filename

    def _create_catalog_record(
        self,
        export_type: str,
        since_ns: Optional[int],
        until_ns: Optional[int],
    ) -> dict[str, Any]:
        try:
            import requests
            resp = requests.post(
                f"{self.api_url}/api/ml/exports",
                json={
                    "export_type": export_type,
                    "status": "running",
                    "since_ns": since_ns,
                    "until_ns": until_ns,
                },
                timeout=10,
            )
            if resp.ok:
                return resp.json()
        except Exception as exc:
            log.debug("Could not create catalog record: %s", exc)
        return {"id": f"local-{time.time_ns()}"}

    def _update_catalog_record(
        self,
        export_id: str,
        output_path: str,
        row_count: int,
        anomaly_rows: int,
        normal_rows: int,
        status: str,
        error_message: str,
        quality_report: ExportQualityReport,
    ) -> None:
        try:
            import requests
            requests.patch(
                f"{self.api_url}/api/ml/exports/{export_id}",
                json={
                    "output_path": output_path,
                    "row_count": row_count,
                    "anomaly_rows": anomaly_rows,
                    "normal_rows": normal_rows,
                    "status": status,
                    "error_message": error_message,
                    "quality_json": quality_report.to_dict(),
                },
                timeout=10,
            )
        except Exception as exc:
            log.debug("Could not update catalog record: %s", exc)


class IncrementalExportJob(ParquetExportJob):
    """Variant that reads the last export timestamp from the catalog and
    exports only the delta since that point.

    Falls back to a full export if no previous export record exists.
    """

    def run_incremental(self, export_type: str) -> ExportResult:
        """Run an incremental export — only data since the last successful export."""
        last_until_ns = self._get_last_export_until_ns(export_type)

        if last_until_ns is None:
            log.info(
                "IncrementalExportJob: no previous %s export found, running full export",
                export_type,
            )
            return self.run(export_type=export_type, extra_label="full_initial")

        until_ns = time.time_ns()
        log.info(
            "IncrementalExportJob: %s incremental export since %d",
            export_type, last_until_ns,
        )
        return self.run(
            export_type=export_type,
            since_ns=last_until_ns,
            until_ns=until_ns,
            extra_label="incremental",
        )

    def _get_last_export_until_ns(self, export_type: str) -> Optional[int]:
        try:
            import requests
            resp = requests.get(
                f"{self.api_url}/api/ml/exports",
                params={"export_type": export_type, "status": "completed", "limit": 1},
                timeout=10,
            )
            if resp.ok:
                records = resp.json()
                if records:
                    return records[0].get("until_ns")
        except Exception as exc:
            log.debug("Could not fetch last export record: %s", exc)
        return None


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)

    parser = argparse.ArgumentParser(description="Bonsai Parquet export job")
    parser.add_argument("--type", choices=["anomaly", "remediation"], default="anomaly")
    parser.add_argument("--incremental", action="store_true")
    parser.add_argument("--api-url", default=os.environ.get("BONSAI_API_URL", DEFAULT_API_URL))
    parser.add_argument("--output-dir", default=DEFAULT_OUTPUT_DIR)
    args = parser.parse_args()

    if args.incremental:
        job: ParquetExportJob = IncrementalExportJob(
            api_url=args.api_url, output_dir=args.output_dir
        )
        result = job.run_incremental(export_type=args.type)
    else:
        job = ParquetExportJob(api_url=args.api_url, output_dir=args.output_dir)
        result = job.run(export_type=args.type)

    print(f"Export complete: {result.output_path} ({result.row_count} rows, quality={result.quality_passed})")
    if result.error:
        print(f"Error: {result.error}")

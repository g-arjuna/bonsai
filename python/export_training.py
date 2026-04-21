"""Export training data from a running bonsai instance to Parquet.

Usage:
    # Model A — anomaly detector (DetectionEvents + normal windows)
    python export_training.py --output data/training.parquet

    # Model C — remediation classifier (Remediation nodes joined to DetectionEvents)
    python export_training.py --mode remediation --output data/remediation.parquet

    # Check graph readiness before exporting for training
    python ../scripts/check_training_readiness.py

    # With date filter:
    python export_training.py --since 2026-04-01 --output data/training.parquet
"""
from __future__ import annotations

import argparse
import datetime
import os
import sys

from bonsai_sdk.client import BonsaiClient
from bonsai_sdk.training import export_training_set, export_remediation_training_set


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--output",  default="data/training.parquet")
    ap.add_argument("--api",     default="[::1]:50051")
    ap.add_argument("--mode",    choices=["anomaly", "remediation"], default="anomaly",
                    help="Which training set to export (default: anomaly)")
    ap.add_argument("--since",   default=None,
                    help="ISO date (YYYY-MM-DD) to start export from; default: all time")
    args = ap.parse_args()

    since_ns = 0
    if args.since:
        dt = datetime.datetime.fromisoformat(args.since).replace(
            tzinfo=datetime.timezone.utc
        )
        since_ns = int(dt.timestamp() * 1e9)

    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)

    with BonsaiClient(args.api) as client:
        if args.mode == "remediation":
            n = export_remediation_training_set(client, args.output, since_ns=since_ns)
        else:
            n = export_training_set(client, args.output, since_ns=since_ns)

    if n == 0:
        print("No data exported — run bonsai and inject some faults first.")
        sys.exit(1)
    print(f"Exported {n} rows ({args.mode}) to {args.output}")


if __name__ == "__main__":
    main()

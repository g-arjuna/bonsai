"""Train Model A — IsolationForest anomaly detector.

Usage:
    # 1. Export training data while bonsai is running (or has run):
    python export_training.py --output data/training.parquet

    # 2. Train the model:
    python train_anomaly.py --input data/training.parquet --output models/anomaly_v1.joblib

    # 3. Register in engine.py:
    from bonsai_sdk.ml_detector import MLDetector
    detectors.append(MLDetector("ml_anomaly_v1", "models/anomaly_v1.joblib", threshold=0.6))

Requires: scikit-learn, pandas, pyarrow, joblib
"""
from __future__ import annotations

import argparse
import os
import sys

import joblib
import numpy as np
import pandas as pd
import pyarrow.parquet as pq
from sklearn.ensemble import IsolationForest
from sklearn.metrics import classification_report
from sklearn.model_selection import train_test_split

from bonsai_sdk.ml_detector import NUMERIC_FEATURES, OPER_STATUS_ENCODING, EVENT_TYPE_ENCODING


def load_features(parquet_path: str) -> tuple[np.ndarray, np.ndarray]:
    """Load Parquet, build feature matrix X and label vector y."""
    df = pq.read_table(parquet_path).to_pandas()
    print(f"Loaded {len(df)} rows ({df['label'].sum()} anomalies, "
          f"{(df['label'] == 0).sum()} normal)")

    # Encode categorical columns.
    df["oper_status_enc"] = df["oper_status"].map(OPER_STATUS_ENCODING).fillna(-1)
    df["event_type_enc"]  = df["event_type"].map(EVENT_TYPE_ENCODING).fillna(0)
    df["occurred_at_s"]   = pd.to_numeric(df["occurred_at_ns"], errors="coerce").fillna(0) / 1e9

    feature_cols = [
        "peer_count_total",
        "peer_count_established",
        "recent_flap_count",
        "occurred_at_s",
        "oper_status_enc",
        "event_type_enc",
    ]
    X = df[feature_cols].fillna(0).astype(np.float32).values
    y = df["label"].astype(int).values
    return X, y


def train(X_train: np.ndarray, contamination: float) -> IsolationForest:
    model = IsolationForest(
        n_estimators=200,
        contamination=contamination,
        random_state=42,
        n_jobs=-1,
    )
    model.fit(X_train)
    return model


def evaluate(model: IsolationForest, X: np.ndarray, y: np.ndarray) -> None:
    # IsolationForest.predict returns 1 (normal) or -1 (anomaly).
    raw = model.predict(X)
    y_pred = (raw == -1).astype(int)   # -1 → anomaly → label=1
    print("\nEvaluation on held-out set:")
    print(classification_report(y, y_pred, target_names=["normal", "anomaly"], zero_division=0))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input",       required=True, help="Path to training Parquet file")
    ap.add_argument("--output",      default="models/anomaly_v1.joblib")
    ap.add_argument("--contamination", type=float, default=0.1,
                    help="Fraction of anomalies in training data (default 0.1)")
    ap.add_argument("--eval", action="store_true", help="Evaluate on 20%% held-out split")
    args = ap.parse_args()

    if not os.path.exists(args.input):
        print(f"ERROR: input file not found: {args.input}", file=sys.stderr)
        sys.exit(1)

    X, y = load_features(args.input)
    if len(X) < 10:
        print("ERROR: need at least 10 rows to train", file=sys.stderr)
        sys.exit(1)

    if args.eval and len(X) >= 50:
        X_train, X_test, y_train, y_test = train_test_split(
            X, y, test_size=0.2, random_state=42, stratify=y if y.sum() > 1 else None
        )
    else:
        X_train, X_test, y_train, y_test = X, X, y, y

    print(f"Training IsolationForest on {len(X_train)} samples "
          f"(contamination={args.contamination})...")
    model = train(X_train, args.contamination)

    if args.eval:
        evaluate(model, X_test, y_test)

    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    joblib.dump(model, args.output)
    print(f"\nModel saved to {args.output}")
    print("Register in engine.py:")
    print(f'  MLDetector("ml_anomaly_v1", "{args.output}", threshold=0.6)')


if __name__ == "__main__":
    main()

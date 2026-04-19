"""Train Model C — remediation action classifier (GradientBoostingClassifier).

Predicts which action will succeed for a given detection feature vector.

Usage:
    # 1. Export remediation training data:
    python export_training.py --mode remediation --output data/remediation.parquet

    # 2. Train:
    python train_remediation.py --input data/remediation.parquet --output models/remediation_v1.joblib

    # 3. Register in RemediationExecutor:
    from bonsai_sdk.ml_remediation import MLRemediationSelector
    selector = MLRemediationSelector.load("models/remediation_v1.joblib")

Requires: scikit-learn, joblib, pyarrow, pandas, numpy

NOTE: Model C needs enough labelled Remediation nodes to be useful.
      Minimum ~50 successful remediations across ≥2 action types.
      If you don't have that yet, skip training and the selector will
      fall back to catalog-based selection automatically.
"""
from __future__ import annotations

import argparse
import os
import sys

import joblib
import numpy as np
import pandas as pd
import pyarrow.parquet as pq
from sklearn.ensemble import GradientBoostingClassifier
from sklearn.metrics import classification_report
from sklearn.model_selection import train_test_split
from sklearn.preprocessing import LabelEncoder

from bonsai_sdk.ml_detector import OPER_STATUS_ENCODING, EVENT_TYPE_ENCODING


def load_remediation_features(parquet_path: str) -> tuple[np.ndarray, np.ndarray, list[str], list[str]]:
    """Load Parquet, build feature matrix X, label vector y, action_classes list."""
    df = pq.read_table(parquet_path).to_pandas()
    print(f"Loaded {len(df)} remediation rows")
    print(f"Status distribution:\n{df['status'].value_counts()}")
    print(f"Action distribution:\n{df['action'].value_counts()}")

    # Encode categoricals shared with Model A.
    df["oper_status_enc"] = df["oper_status"].map(OPER_STATUS_ENCODING).fillna(-1)
    df["event_type_enc"]  = df["event_type"].map(EVENT_TYPE_ENCODING).fillna(0)
    df["occurred_at_s"]   = pd.to_numeric(df["occurred_at_ns"], errors="coerce").fillna(0) / 1e9

    # One-hot encode candidate actions.
    action_classes = sorted(df["action"].dropna().unique().tolist())
    for a in action_classes:
        df[f"action_{a}"] = (df["action"] == a).astype(np.float32)

    base_cols = [
        "peer_count_total", "peer_count_established", "recent_flap_count",
        "occurred_at_s", "oper_status_enc", "event_type_enc",
    ]
    action_cols = [f"action_{a}" for a in action_classes]
    feature_cols = base_cols + action_cols

    X = np.asarray(df[feature_cols].fillna(0).astype(np.float32))
    y = np.asarray(df["status"].fillna("skipped"), dtype=object)
    return X, y, action_classes, feature_cols


def train(X_train: np.ndarray, y_train: np.ndarray) -> GradientBoostingClassifier:
    model = GradientBoostingClassifier(
        n_estimators=100,
        max_depth=3,
        learning_rate=0.1,
        random_state=42,
    )
    model.fit(X_train, y_train)
    return model


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input",  required=True, help="Remediation Parquet from export_training.py")
    ap.add_argument("--output", default="models/remediation_v1.joblib")
    ap.add_argument("--eval",   action="store_true", help="Evaluate on 20%% held-out split")
    ap.add_argument("--min-rows", type=int, default=20,
                    help="Minimum rows required to proceed (default 20)")
    args = ap.parse_args()

    if not os.path.exists(args.input):
        print(f"ERROR: input not found: {args.input}", file=sys.stderr)
        sys.exit(1)

    X, y, action_classes, feature_cols = load_remediation_features(args.input)

    if len(X) < args.min_rows:
        print(f"Only {len(X)} rows — need at least {args.min_rows} to train Model C.")
        print("Keep running bonsai + auto-remediation to accumulate more data.")
        sys.exit(1)

    unique_statuses = np.unique(y)
    if len(unique_statuses) < 2:
        print(f"Only one status class ({unique_statuses}) — need at least 2 to train.")
        sys.exit(1)

    if args.eval and len(X) >= 40:
        X_train, X_test, y_train, y_test = train_test_split(
            X, y, test_size=0.2, random_state=42
        )
    else:
        X_train, X_test, y_train, y_test = X, X, y, y

    print(f"\nTraining GradientBoostingClassifier on {len(X_train)} samples...")
    model = train(X_train, y_train)

    if args.eval:
        y_pred = model.predict(X_test)
        print("\nEvaluation on held-out set:")
        print(classification_report(y_test, y_pred, zero_division=0))

    # Save model + action_classes together so MLRemediationSelector can load both.
    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    joblib.dump({"model": model, "action_classes": action_classes}, args.output)
    print(f"\nModel saved to {args.output}")
    print(f"Action classes: {action_classes}")
    print("Load with:")
    print(f'  MLRemediationSelector.load("{args.output}")')


if __name__ == "__main__":
    main()

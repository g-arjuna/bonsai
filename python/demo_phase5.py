#!/usr/bin/env python3
"""Phase 5 demo — rules + ML anomaly detection running in parallel.

Run with bonsai already started:
    python python/demo_phase5.py

ML models are loaded automatically from models/ if present:
  models/anomaly_v1.joblib      → MLDetector (Model A, IsolationForest)
  models/remediation_v1.joblib  → MLRemediationSelector (Model C, GBT)

If no models are found the demo runs in rules-only mode (identical to Phase 4).

── Training workflow (before ML is active) ──────────────────────────────────
  # 1. Run Phase 4 demo for a while, inject faults, accumulate data
  # 2. Export training data:
  python python/export_training.py --output data/training.parquet
  python python/export_training.py --mode remediation --output data/remediation.parquet
  # 3. Train models:
  python python/train_anomaly.py --input data/training.parquet --eval
  python python/train_remediation.py --input data/remediation.parquet --eval
  # 4. Restart this demo — models are picked up automatically

── Injecting faults to demonstrate ML catching things rules miss ─────────────
  # Gradual packet loss (netem) — rules fire only at threshold, ML fires earlier:
  clab tools netem set <topology> <node> <iface> --loss 1    # 1% — rules silent
  clab tools netem set <topology> <node> <iface> --loss 5    # 5% — rules may fire
  clab tools netem set <topology> <node> <iface> --loss 0    # restore

  # Hard BGP disable:
  ssh admin@<ip> 'sr_cli "set / network-instance default protocols bgp neighbor <peer> admin-state disable"'
"""
import json
import os
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from bonsai_sdk import BonsaiClient, RuleEngine, RemediationExecutor
from bonsai_sdk.detection import Detection

# ANSI colours
_C = {
    "red":    "\033[31m",
    "yellow": "\033[33m",
    "green":  "\033[32m",
    "cyan":   "\033[36m",
    "bold":   "\033[1m",
    "reset":  "\033[0m",
}

_SEVERITY_COLOR = {"info": "", "warn": _C["yellow"], "critical": _C["red"]}

# Counters for final summary
_detection_counts: dict[str, int] = defaultdict(int)   # rule_id → count
_ml_count   = 0
_rule_count = 0


def _is_ml(detection: Detection) -> bool:
    return detection.rule_id.startswith("ml_")


def on_detection(detection: Detection, client: BonsaiClient, executor: RemediationExecutor) -> None:
    global _ml_count, _rule_count
    ts    = time.strftime("%H:%M:%S")
    color = _SEVERITY_COLOR.get(detection.severity, "")
    reset = _C["reset"]

    if _is_ml(detection):
        _ml_count += 1
        tag = f"{_C['cyan']}[ML]{reset}"
    else:
        _rule_count += 1
        tag = "[RULE]"

    _detection_counts[detection.rule_id] += 1

    print(
        f"\n{color}{_C['bold']}[{ts}] DETECTION [{detection.severity.upper()}]{reset} "
        f"{tag} {detection.rule_id}"
        f"\n  device:  {detection.features.device_address}"
        f"\n  reason:  {detection.reason}"
    )
    if detection.features.peer_address:
        print(f"  peer:    {detection.features.peer_address}")
    if _is_ml(detection):
        print(f"  source:  ML model (anomaly score embedded in reason)")

    if detection.auto_remediate:
        print(f"  action:  {detection.remediation_action} (auto-heal enabled)")
    else:
        print(f"  action:  log-only")

    # Write DetectionEvent to graph
    try:
        resp = client.create_detection(
            device_address=detection.features.device_address,
            rule_id=detection.rule_id,
            severity=detection.severity,
            features_json=detection.features.to_json(),
            fired_at_ns=detection.features.occurred_at_ns or int(time.time() * 1e9),
            state_change_event_id=detection.features.state_change_event_id,
        )
        detection_id = resp.id
        print(f"  graph:   DetectionEvent written (id={detection_id[:8]}...)")
    except Exception as exc:
        print(f"  graph:   failed to write DetectionEvent: {exc}")
        return

    executor.handle(detection, detection_id)


def on_remediation(action: str, status: str, detail: dict) -> None:
    ts    = time.strftime("%H:%M:%S")
    color = _C["green"] if status == "success" else _C["yellow"]
    reset = _C["reset"]
    detail_str = json.dumps(detail) if detail else ""
    print(
        f"{color}[{ts}] REMEDIATION {action} -> {status}{reset}"
        + (f"  {detail_str}" if detail_str else "")
    )


def _print_summary(client: BonsaiClient) -> None:
    print(f"\n{_C['bold']}=== Phase 5 session summary ==={_C['reset']}")
    print(f"  Rule-based detections : {_rule_count}")
    print(f"  ML detections         : {_ml_count}")
    if _detection_counts:
        print("  Breakdown by rule:")
        for rule_id, count in sorted(_detection_counts.items(), key=lambda x: -x[1]):
            tag = f"{_C['cyan']}[ML]{_C['reset']}" if rule_id.startswith("ml_") else "    "
            print(f"    {tag} {rule_id:40s} {count}")

    try:
        rows = client.query(
            "MATCH (n:DetectionEvent) RETURN n.rule_id, count(n) ORDER BY count(n) DESC"
        )
        if rows:
            print("\n  All-time graph totals:")
            for row in rows:
                print(f"    {row[0]:40s} {row[1]}")
    except Exception:
        pass


def _load_ml_remediation_selector(model_dir: str):
    path = os.path.join(model_dir, "remediation_v1.joblib")
    if not os.path.exists(path):
        return None
    try:
        from bonsai_sdk.ml_remediation import MLRemediationSelector
        sel = MLRemediationSelector.load(path)
        print(f"  ML remediation selector loaded from {path}")
        return sel
    except Exception as exc:
        print(f"  WARNING: failed to load remediation model: {exc}")
        return None


def main() -> None:
    dry_run   = os.environ.get("BONSAI_DRY_RUN", "0") == "1"
    addr      = os.environ.get("BONSAI_ADDR", "[::1]:50051")
    model_dir = os.environ.get("BONSAI_MODEL_DIR", "models")

    print(f"{_C['bold']}Bonsai Phase 5 demo{_C['reset']} — rules + ML anomaly detection")
    print(f"  API:       {addr}")
    print(f"  Model dir: {model_dir}")
    if dry_run:
        print(f"  {_C['yellow']}DRY-RUN mode: no gNMI Set will be sent{_C['reset']}")
    print()

    with BonsaiClient(addr) as client:
        ml_selector = _load_ml_remediation_selector(model_dir)

        executor = RemediationExecutor(
            client=client,
            on_remediation=on_remediation,
            ml_selector=ml_selector,
        )

        def detection_callback(detection: Detection) -> None:
            on_detection(detection, client, executor)

        engine = RuleEngine(
            client=client,
            on_detection=detection_callback,
            dry_run=dry_run,
            model_dir=model_dir,
        )
        engine.start()

        ml_rules = [r for r in engine._rules if r.rule_id.startswith("ml_")]
        if ml_rules:
            print(f"\n{_C['cyan']}ML active:{_C['reset']} {[r.rule_id for r in ml_rules]}")
        else:
            print(f"\n{_C['yellow']}Rules-only mode{_C['reset']} — train a model to activate ML")
            print("  Run: python python/export_training.py && python python/train_anomaly.py --input data/training.parquet")

        print("\nWaiting for events... (Ctrl-C to stop)\n")

        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            print("\nStopping...")
            engine.stop()

        _print_summary(client)


if __name__ == "__main__":
    main()

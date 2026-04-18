#!/usr/bin/env python3
"""Phase 4 demo — live detection and remediation.

Run with bonsai already started:
    python python/demo_phase4.py

To trigger detections:
  BGP session down:
    On SRL:  ssh admin@<node-ip> 'sr_cli "set / network-instance default protocols bgp neighbor <peer> admin-state disable"'
    Via clab: clab tools netem set bonsai-p4 srl-spine1 e1-1 --delay 10000ms  # effectively kills BGP

  Interface down:
    ssh admin@172.100.102.11 'sr_cli "set / interface ethernet-1/1 admin-state disable"'

  See live graph after:
    python python/example.py
"""
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from bonsai_sdk import BonsaiClient, RuleEngine, RemediationExecutor
from bonsai_sdk.detection import Detection


def on_detection(detection: Detection, client: BonsaiClient, executor: RemediationExecutor) -> None:
    ts = time.strftime("%H:%M:%S")
    severity_color = {"info": "", "warn": "\033[33m", "critical": "\033[31m"}.get(detection.severity, "")
    reset = "\033[0m"
    print(
        f"\n{severity_color}[{ts}] DETECTION [{detection.severity.upper()}] {detection.rule_id}{reset}"
        f"\n  device:  {detection.features.device_address}"
        f"\n  reason:  {detection.reason}"
    )
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
        )
        detection_id = resp.id
        print(f"  graph:   DetectionEvent written (id={detection_id[:8]}...)")
    except Exception as exc:
        print(f"  graph:   failed to write DetectionEvent: {exc}")
        return

    # Execute remediation (circuit breaker + dry-run checks are inside executor)
    executor.handle(detection, detection_id)


def on_remediation(action: str, status: str, detail: dict) -> None:
    ts = time.strftime("%H:%M:%S")
    color  = "\033[32m" if status == "success" else "\033[33m"
    reset  = "\033[0m"
    detail_str = json.dumps(detail) if detail else ""
    print(f"{color}[{ts}] REMEDIATION {action} → {status}{reset}" + (f"  {detail_str}" if detail_str else ""))


def main():
    dry_run = os.environ.get("BONSAI_DRY_RUN", "0") == "1"
    addr    = os.environ.get("BONSAI_ADDR", "[::1]:50051")

    print(f"Bonsai Phase 4 demo — connecting to {addr}")
    if dry_run:
        print("DRY-RUN mode: detections will be logged but no gNMI Set will be sent")
    print("Waiting for events... (Ctrl-C to stop)\n")

    with BonsaiClient(addr) as client:
        executor = RemediationExecutor(
            client=client,
            on_remediation=on_remediation,
        )

        def detection_callback(detection: Detection) -> None:
            on_detection(detection, client, executor)

        engine = RuleEngine(
            client=client,
            on_detection=detection_callback,
            dry_run=dry_run,
        )
        engine.start()

        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            print("\nStopping...")
            engine.stop()

        # Final graph summary
        print("\n=== Graph summary ===")
        rows = client.query("MATCH (n:DetectionEvent) RETURN n.rule_id, n.severity, n.fired_at")
        for row in rows:
            print(f"  {row}")


if __name__ == "__main__":
    main()

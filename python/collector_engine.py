#!/usr/bin/env python3
"""Collector Rule Engine Sidecar.

Runs alongside a Bonsai collector (Rust) to evaluate local rules.
Detections are persisted to the local collector graph AND forwarded to the core.

Usage:
    export BONSAI_COLLECTOR_ID="collector-alpha"
    export BONSAI_CORE_ADDR="core-host:50051"
    export BONSAI_LOCAL_ADDR="localhost:50052"
    python python/collector_engine.py
"""
import os
import queue
import sys
import threading
import time
from pathlib import Path
from typing import Generator

sys.path.insert(0, str(Path(__file__).parent))

from bonsai_sdk import BonsaiClient, RuleEngine
from bonsai_sdk.detection import Detection
from generated import bonsai_service_pb2 as pb

# Queue for forwarding detections to core
forward_queue = queue.Queue(maxsize=1000)

def detection_ingest_generator() -> Generator[pb.DetectionEventIngest, None, None]:
    collector_id = os.environ.get("BONSAI_COLLECTOR_ID", "unknown-collector")
    while True:
        try:
            detection = forward_queue.get(timeout=1.0)
            yield pb.DetectionEventIngest(
                collector_id=collector_id,
                device_address=detection.features.device_address,
                rule_id=detection.rule_id,
                severity=detection.severity,
                reason=detection.reason,
                features_json=detection.features.to_json(),
                fired_at_ns=detection.features.occurred_at_ns or int(time.time() * 1e9),
                state_change_event_id=detection.features.state_change_event_id or "",
                auto_remediate=detection.auto_remediate,
                remediation_action=detection.remediation_action,
            )
        except queue.Empty:
            # Check for shutdown here if needed
            continue

def core_forwarder_thread(core_addr: str):
    print(f"[collector-engine] core forwarder starting, core={core_addr}")
    while True:
        try:
            with BonsaiClient(core_addr) as client:
                print(f"[collector-engine] connected to core at {core_addr}")
                client.detection_ingest(detection_ingest_generator())
        except Exception as exc:
            print(f"[collector-engine] core connection error: {exc}")
            time.sleep(5)

def on_detection(detection: Detection, local_client: BonsaiClient) -> None:
    ts = time.strftime("%H:%M:%S")
    print(f"[{ts}] LOCAL DETECTION: {detection.rule_id} on {detection.features.device_address}")
    
    # 1. Persist to LOCAL collector graph
    try:
        local_client.create_detection(
            device_address=detection.features.device_address,
            rule_id=detection.rule_id,
            severity=detection.severity,
            features_json=detection.features.to_json(),
            fired_at_ns=detection.features.occurred_at_ns or int(time.time() * 1e9),
            state_change_event_id=detection.features.state_change_event_id,
        )
    except Exception as exc:
        print(f"[collector-engine] failed to write to local graph: {exc}")

    # 2. Queue for FORWARDING to core
    try:
        forward_queue.put_nowait(detection)
    except queue.Full:
        print(f"[collector-engine] warning: forward queue full, dropping detection {detection.rule_id}")

def main():
    core_addr  = os.environ.get("BONSAI_CORE_ADDR", "[::1]:50051")
    local_addr = os.environ.get("BONSAI_LOCAL_ADDR", "localhost:50052")
    
    print(f"Bonsai Collector Rule Engine")
    print(f"  local collector: {local_addr}")
    print(f"  core ingest:     {core_addr}")

    # Start core forwarder in background
    threading.Thread(target=core_forwarder_thread, args=(core_addr,), daemon=True).start()

    # Connect to LOCAL collector to stream events and query local graph
    while True:
        try:
            with BonsaiClient(local_addr) as local_client:
                print(f"[collector-engine] connected to local collector at {local_addr}")
                
                def callback(d: Detection):
                    on_detection(d, local_client)

                engine = RuleEngine(
                    client=local_client,
                    on_detection=callback,
                    run_scope="local",
                )
                engine.start()
                
                while True:
                    time.sleep(1)
        except Exception as exc:
            print(f"[collector-engine] local collector connection error: {exc}")
            time.sleep(5)

if __name__ == "__main__":
    main()

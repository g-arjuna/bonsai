#!/usr/bin/env python3
"""Collector Rule Engine Sidecar.

Runs alongside a Bonsai collector (Rust) to evaluate local rules.
Detections are persisted to the local collector graph AND forwarded to the core.

CV7 T4-3: this sidecar registers itself with the local bonsai over gRPC
(`RegisterSidecar`) at startup and emits a heartbeat every 15 seconds
(`SidecarHeartbeat`). The registration surfaces at `/api/sidecars` and on the
bonpy UI so the operator knows the sidecar is bound. See
`docs/architecture/sidecars.md`.

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

# Sidecar metadata reported to bonsai via RegisterSidecar.
SIDECAR_KIND     = "rules"
SIDECAR_VERSION  = "0.1.0"       # bump when the wire-visible behaviour changes
HEARTBEAT_PERIOD = 15.0          # seconds — matches src/sidecar_registry.rs

# Running counters surfaced via SidecarHeartbeat. Updated under the GIL — no
# explicit lock needed for monotonically-increasing int reads/writes.
_metrics = {"events_in_total": 0, "detections_out_total": 0}

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
            source_event_ids=detection.effective_source_event_ids,
        )
        _metrics["detections_out_total"] += 1
    except Exception as exc:
        print(f"[collector-engine] failed to write to local graph: {exc}")

    # 2. Queue for FORWARDING to core
    try:
        forward_queue.put_nowait(detection)
    except queue.Full:
        print(f"[collector-engine] warning: forward queue full, dropping detection {detection.rule_id}")


def _gather_capabilities() -> list[str]:
    """Best-effort list of rule_ids this sidecar will fire. Used as the
    `capabilities` field on RegisterSidecar so the bonpy UI knows what to
    expect."""
    caps: list[str] = []
    try:
        from bonsai_sdk.rules.bgp import BGP_RULES
        from bonsai_sdk.rules.bfd import BFD_RULES
        from bonsai_sdk.rules.config import CONFIG_RULES
        from bonsai_sdk.rules.interface import INTERFACE_RULES
        from bonsai_sdk.rules.optical import OPTICAL_RULES
        from bonsai_sdk.rules.rack import RACK_RULES
        from bonsai_sdk.rules.snmp import SNMP_RULES
        from bonsai_sdk.rules.streaming import STREAMING_RULES
        from bonsai_sdk.rules.syslog import SYSLOG_RULES
        from bonsai_sdk.rules.topology import TOPOLOGY_RULES
        for rule in BGP_RULES + BFD_RULES + CONFIG_RULES + INTERFACE_RULES + OPTICAL_RULES + RACK_RULES + SNMP_RULES + STREAMING_RULES + SYSLOG_RULES:
            caps.append(rule.rule_id)
        # TOPOLOGY_RULES is a class with class-level evaluate_topology; expose its rule_id explicitly.
        caps.append("topology_edge_lost")
    except Exception as exc:
        print(f"[collector-engine] warning: failed to enumerate capabilities: {exc}")
    return caps


def _heartbeat_loop(
    client: BonsaiClient,
    sidecar_id_holder: dict,
    register_kwargs: dict,
    engine_holder: dict,
) -> None:
    """Periodic heartbeat thread. If bonsai responds reregister_required=True
    (e.g. bonsai restarted and lost its registry), call RegisterSidecar again
    and update the stored sidecar_id."""
    while True:
        time.sleep(HEARTBEAT_PERIOD)
        sidecar_id = sidecar_id_holder.get("id")
        if not sidecar_id:
            continue
        engine = engine_holder.get("engine")
        events_in = getattr(engine, "events_received_total", 0) if engine else 0
        try:
            still_known = client.sidecar_heartbeat(
                sidecar_id=sidecar_id,
                events_in_total=events_in,
                detections_out_total=_metrics["detections_out_total"],
                status_json="",
            )
            if not still_known:
                print("[collector-engine] bonsai lost our sidecar_id — re-registering")
                new_id = client.register_sidecar(**register_kwargs)
                sidecar_id_holder["id"] = new_id
        except Exception as exc:
            print(f"[collector-engine] heartbeat failed: {exc} (will retry in {HEARTBEAT_PERIOD}s)")

def main():
    core_addr     = os.environ.get("BONSAI_CORE_ADDR", "[::1]:50051")
    local_addr    = os.environ.get("BONSAI_LOCAL_ADDR", "localhost:50052")
    sidecar_name  = os.environ.get("BONSAI_COLLECTOR_ID", "rules-local")

    print(f"Bonsai Collector Rule Engine")
    print(f"  local collector: {local_addr}")
    print(f"  core ingest:     {core_addr}")
    print(f"  sidecar name:    {sidecar_name} (kind={SIDECAR_KIND})")

    # Start core forwarder in background
    threading.Thread(target=core_forwarder_thread, args=(core_addr,), daemon=True).start()

    # Connect to LOCAL collector to stream events and query local graph
    while True:
        try:
            with BonsaiClient(local_addr) as local_client:
                print(f"[collector-engine] connected to local collector at {local_addr}")

                # CV7 T4-3: register this sidecar with bonsai so its presence
                # is a first-class fact at /api/sidecars and on the bonpy UI.
                register_kwargs = dict(
                    name=sidecar_name,
                    kind=SIDECAR_KIND,
                    version=SIDECAR_VERSION,
                    capabilities=_gather_capabilities(),
                    address=local_addr,
                )
                sidecar_id_holder: dict = {"id": None}
                engine_holder: dict = {"engine": None}
                try:
                    sidecar_id_holder["id"] = local_client.register_sidecar(**register_kwargs)
                    print(f"[collector-engine] registered as sidecar {sidecar_id_holder['id']}")
                except Exception as exc:
                    # Failure to register is NOT fatal — the sidecar still runs
                    # rules; visibility is lost but detection continues. The
                    # heartbeat loop will retry registration via re-register.
                    print(f"[collector-engine] WARNING: RegisterSidecar failed: {exc}")

                threading.Thread(
                    target=_heartbeat_loop,
                    args=(local_client, sidecar_id_holder, register_kwargs, engine_holder),
                    daemon=True,
                    name="bonsai-sidecar-heartbeat",
                ).start()

                def callback(d: Detection):
                    on_detection(d, local_client)

                engine = RuleEngine(
                    client=local_client,
                    on_detection=callback,
                    run_scope="local",
                )
                engine_holder["engine"] = engine
                engine.start()

                while True:
                    time.sleep(1)
        except Exception as exc:
            print(f"[collector-engine] local collector connection error: {exc}")
            time.sleep(5)

if __name__ == "__main__":
    main()

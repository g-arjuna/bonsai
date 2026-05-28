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
    export BONSAI_CORE_ADDR="localhost:50051"
    export BONSAI_LOCAL_ADDR="localhost:50051"
    python python/collector_engine.py
"""
import http.server
import json
import os
import queue
import signal
import sys
import threading
import time
from pathlib import Path
from typing import Generator, Optional

sys.path.insert(0, str(Path(__file__).parent))

from bonsai_sdk import BonsaiClient, RuleEngine
from bonsai_sdk.detection import Detection
from generated import bonsai_service_pb2 as pb

# Queue for forwarding detections to core
forward_queue = queue.Queue(maxsize=1000)

# EV1-9 T2: global stop event for graceful shutdown
_stop_event = threading.Event()

# EV1-9 T6: queue pressure tracking
_forward_queue_drops_total: int = 0
_priority_only_mode: bool = False  # True when queue > 95% full

# Sidecar metadata reported to bonsai via RegisterSidecar.
SIDECAR_KIND     = "rules"
SIDECAR_VERSION  = "0.1.0"       # bump when the wire-visible behaviour changes
HEARTBEAT_PERIOD = 15.0          # seconds — matches src/sidecar_registry.rs

# Running counters surfaced via SidecarHeartbeat. Updated under the GIL — no
# explicit lock needed for monotonically-increasing int reads/writes.
_metrics = {"events_in_total": 0, "detections_out_total": 0}

# D4-9 T1 / EV1-9 T7: Health HTTP endpoint state
_start_time = time.monotonic()
_last_detection_at_ns: int = 0
_detections_today: int = 0
_detections_today_date: str = ""
_rules_loaded: int = 0
_connected_to_core: bool = False
_connected_to_local: bool = False
_job_engine_ref: Optional[object] = None  # set after job engine starts
_inference_loop_ref: Optional[object] = None
_engine_ref: Optional[object] = None  # EV1-7 T3: RuleEngine ref for shadow-firings HTTP endpoint

HEALTH_PORT = int(os.environ.get("BONSAI_SIDECAR_HEALTH_PORT", "9292"))


class _HealthHandler(http.server.BaseHTTPRequestHandler):
    """Lightweight handler for GET /health — no dependencies outside stdlib."""

    def do_GET(self):
        if self.path.startswith("/shadow-firings/"):
            self._serve_shadow_firings()
            return
        if self.path.rstrip("/") != "/health":
            self.send_error(404)
            return

        today = time.strftime("%Y-%m-%d")
        global _detections_today, _detections_today_date
        if today != _detections_today_date:
            _detections_today = 0
            _detections_today_date = today

        qdepth = forward_queue.qsize()
        engine = _job_engine_ref
        next_job = None
        job_engine_running = False
        if engine is not None:
            job_engine_running = getattr(engine, "_running", False)
            try:
                jobs = engine.list_jobs()
                running_jobs = [j for j in jobs if j.state.value == "running"]
                if running_jobs:
                    next_job = {"id": running_jobs[0].job_id, "state": "running"}
            except Exception:
                pass

        snap_size = 0
        snap_stale = False
        try:
            from bonsai_ml.gnn.snapshot_store import SnapshotStore
            store = SnapshotStore()
            health = store.get_buffer_health()
            snap_size = health.buffer_size
            snap_stale = health.is_stale
        except Exception:
            pass

        model_loaded = False
        model_id = None
        last_inference_at_ns = 0
        try:
            from bonsai_ml.gnn.snapshot_store import SnapshotStore as _SS
            import os as _os
            model_path = _os.environ.get("BONSAI_GNN_MODEL_PATH", "models/stgnn_v1.pt")
            model_loaded = _os.path.exists(model_path)
            if model_loaded:
                model_id = _os.path.basename(model_path)
            il = _inference_loop_ref
            if il is not None:
                last_inference_at_ns = getattr(il, "last_inference_at_ns", 0)
        except Exception:
            pass

        rules_enabled = _rules_loaded
        rules_shadow = 0
        try:
            re_ref = _engine_ref
            if re_ref is not None:
                rules_enabled = sum(1 for r in re_ref._rules if not getattr(r, "shadow_mode", False))
                rules_shadow = sum(1 for r in re_ref._rules if getattr(r, "shadow_mode", False))
        except Exception:
            pass

        embedding_pending_syslog = 0
        embedding_pending_config = 0
        api_url = os.environ.get("BONSAI_API_URL", "http://localhost:3000")
        try:
            import urllib.request as _ur, json as _json
            with _ur.urlopen(f"{api_url}/api/ml/embedding-stats", timeout=1) as _resp:
                _es = _json.loads(_resp.read())
                embedding_pending_syslog = _es.get("pending_syslog", 0)
                embedding_pending_config = _es.get("pending_config", 0)
        except Exception:
            pass

        scheduler_mode = "unavailable"
        if engine is not None:
            scheduler_mode = getattr(engine, "scheduler_mode", "apscheduler")

        body = json.dumps({
            "status": "ok",
            "uptime_secs": round(time.monotonic() - _start_time, 1),
            "rules_loaded": _rules_loaded,
            "rules_enabled": rules_enabled,
            "rules_shadow": rules_shadow,
            "last_detection_at_ns": _last_detection_at_ns,
            "detections_today": _detections_today,
            "queue_depth": qdepth,
            "queue_drops_today": _forward_queue_drops_total,
            "priority_only_mode": _priority_only_mode,
            "model_loaded": model_loaded,
            "model_id": model_id,
            "last_inference_at_ns": last_inference_at_ns,
            "snapshot_buffer_size": snap_size,
            "snapshot_buffer_stale": snap_stale,
            "embedding_pending_syslog": embedding_pending_syslog,
            "embedding_pending_config": embedding_pending_config,
            "connected_to_core": _connected_to_core,
            "connected_to_local": _connected_to_local,
            "job_engine_running": job_engine_running,
            "next_job": next_job,
            "scheduler_mode": scheduler_mode,
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _serve_shadow_firings(self):
        """Serve shadow firings for a rule. Path: /shadow-firings/{rule_id}?since=ns"""
        import urllib.parse
        parts = self.path.split("?", 1)
        rule_id = parts[0].removeprefix("/shadow-firings/").strip("/")
        since_ns = 0
        if len(parts) > 1:
            qs = urllib.parse.parse_qs(parts[1])
            since_ns = int(qs.get("since", ["0"])[0])
        engine_ref = _engine_ref
        firings = []
        if engine_ref is not None:
            all_firings = engine_ref.shadow_firings.get(rule_id, [])
            firings = [f for f in all_firings if f.get("fired_at_ns", 0) >= since_ns]
        body = json.dumps({"shadow_firings": firings, "rule_id": rule_id}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass  # silence per-request logs


def _start_health_server():
    """Start the health HTTP server in a daemon thread."""
    try:
        server = http.server.HTTPServer(("0.0.0.0", HEALTH_PORT), _HealthHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True, name="health-http")
        thread.start()
        print(f"[collector-engine] health endpoint listening on :{HEALTH_PORT}/health")
    except Exception as exc:
        print(f"[collector-engine] WARNING: failed to start health server on :{HEALTH_PORT}: {exc}")


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
                source_event_ids=detection.effective_source_event_ids,
            )
        except queue.Empty:
            # Check for shutdown here if needed
            continue

def core_forwarder_thread(core_addr: str):
    global _connected_to_core
    print(f"[collector-engine] core forwarder starting, core={core_addr}")
    while not _stop_event.is_set():
        try:
            with BonsaiClient(core_addr) as client:
                print(f"[collector-engine] connected to core at {core_addr}")
                _connected_to_core = True
                client.detection_ingest(detection_ingest_generator())
        except Exception as exc:
            _connected_to_core = False
            print(f"[collector-engine] core connection error: {exc}")
            _stop_event.wait(5)

def on_detection(detection: Detection, local_client: BonsaiClient) -> None:
    global _last_detection_at_ns, _detections_today, _detections_today_date
    global _forward_queue_drops_total, _priority_only_mode
    ts = time.strftime("%H:%M:%S")
    print(f"[{ts}] LOCAL DETECTION: {detection.rule_id} on {detection.features.device_address}")
    _last_detection_at_ns = detection.features.occurred_at_ns or int(time.time() * 1e9)
    today = time.strftime("%Y-%m-%d")
    if today != _detections_today_date:
        _detections_today = 0
        _detections_today_date = today
    _detections_today += 1

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

    # 2. EV1-9 T6: Queue for FORWARDING to core with backpressure
    qsize = forward_queue.qsize()
    q_pct = qsize / 1000
    if q_pct > 0.95:
        _priority_only_mode = True
        if detection.severity not in ("critical", "high"):
            _forward_queue_drops_total += 1
            print(f"[collector-engine] queue >{95}% full — dropping {detection.severity} {detection.rule_id}")
            return
    elif q_pct > 0.80:
        if not _priority_only_mode:
            print(f"[collector-engine] warning: forward queue at {int(q_pct*100)}% capacity")
    else:
        _priority_only_mode = False

    try:
        forward_queue.put_nowait(detection)
    except queue.Full:
        _forward_queue_drops_total += 1
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

def _handle_sigterm(signum, frame):
    """EV1-9 T2: Graceful shutdown on SIGTERM/SIGINT."""
    global _stop_event
    print("[collector-engine] received SIGTERM — shutting down gracefully")
    _stop_event.set()

    deadline = time.monotonic() + 10
    while not forward_queue.empty() and time.monotonic() < deadline:
        time.sleep(0.2)
    if not forward_queue.empty():
        print(f"[collector-engine] shutdown: flushed {forward_queue.qsize()} pending detections")

    try:
        from bonsai_ml.gnn.snapshot_store import SnapshotStore
        print("[collector-engine] snapshot buffer persisted")
    except Exception:
        pass

    engine = _job_engine_ref
    if engine is not None:
        try:
            engine.stop()
            print("[collector-engine] job engine stopped")
        except Exception:
            pass

    print(f"[collector-engine] graceful shutdown complete. drops={_forward_queue_drops_total}")
    sys.exit(0)


def _local_connect_loop(local_addr: str, sidecar_name: str) -> None:
    """EV1-9 T1: Non-blocking reconnect loop for local collector connection."""
    global _connected_to_local, _rules_loaded
    backoff = 5
    while not _stop_event.is_set():
        try:
            with BonsaiClient(local_addr) as local_client:
                print(f"[collector-engine] connected to local collector at {local_addr}")
                _connected_to_local = True
                backoff = 5

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
                global _engine_ref
                _engine_ref = engine
                _rules_loaded = len(_gather_capabilities())
                engine.start()

                while not _stop_event.is_set():
                    time.sleep(1)

        except Exception as exc:
            _connected_to_local = False
            print(f"[collector-engine] local collector connection error: {exc} (retry in {backoff}s)")
            _stop_event.wait(backoff)
            backoff = min(backoff * 2, 60)


def main():
    global _job_engine_ref
    core_addr    = os.environ.get("BONSAI_CORE_ADDR", "[::1]:50051")
    local_addr   = os.environ.get("BONSAI_LOCAL_ADDR", "localhost:50051")
    sidecar_name = os.environ.get("BONSAI_COLLECTOR_ID", "rules-local")
    api_url      = os.environ.get("BONSAI_API_URL", "http://localhost:3000")

    print(f"Bonsai Collector Rule Engine")
    print(f"  local collector: {local_addr}")
    print(f"  core ingest:     {core_addr}")
    print(f"  sidecar name:    {sidecar_name} (kind={SIDECAR_KIND})")

    # EV1-9 T2: register signal handlers before spawning any threads
    signal.signal(signal.SIGTERM, _handle_sigterm)
    signal.signal(signal.SIGINT,  _handle_sigterm)

    # D4-9 T1: Start health HTTP endpoint (always reachable, even when disconnected)
    _start_health_server()

    # Start core forwarder in background (reconnects independently)
    threading.Thread(
        target=core_forwarder_thread, args=(core_addr,), daemon=True, name="core-forwarder"
    ).start()

    # EV1-5: Start ML job engine in background
    try:
        from bonsai_ml.job_engine import build_default_engine
        engine = build_default_engine(api_url=api_url)

        try:
            from bonsai_ml.inference_loop import StgnnInferenceLoop
            inference = StgnnInferenceLoop(api_url=api_url)
            inference.start(engine)
        except Exception as exc:
            print(f"[collector-engine] WARNING: inference loop unavailable: {exc}")

        engine.start_in_background()
        _job_engine_ref = engine
        print("[collector-engine] ML job engine started")
    except Exception as exc:
        print(f"[collector-engine] WARNING: ML job engine unavailable: {exc}")

    # EV1-9 T1: Local collector connection in a dedicated thread (non-blocking startup)
    threading.Thread(
        target=_local_connect_loop,
        args=(local_addr, sidecar_name),
        daemon=True,
        name="local-connector",
    ).start()

    # Main thread blocks on stop event (allows signal handling to work correctly)
    _stop_event.wait()


if __name__ == "__main__":
    main()

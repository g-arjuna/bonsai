"""Rule engine — consumes StreamEvents and dispatches to all registered Detectors."""
from __future__ import annotations

import os
import threading
import time
from typing import Callable, Optional

from .client import BonsaiClient
from .detection import Detection, Detector, Features
from .ml_detector import MLDetector
from .rules.bfd import BFD_RULES
from .rules.bgp import BGP_RULES
from .rules.interface import INTERFACE_RULES, InterfaceErrorSpike, InterfaceHighUtilization
from .rules.topology import TOPOLOGY_RULES

# Model files scanned at startup. Each entry: (filename, rule_id, threshold, severity).
_ML_MODELS = [
    ("anomaly_v1.joblib", "ml_anomaly_v1", 0.6, "warn"),
]


class RuleEngine:
    """
    Runs two loops in background threads:
      1. Event loop: subscribes to StreamEvents, evaluates event-driven rules.
      2. Poll loop: queries graph every 30s for pattern/counter rules and topology diff.

    On detection, calls the registered on_detection callback.

    ML models are loaded from `model_dir` (default "models/") at startup.
    If a model file is absent the engine starts in rules-only mode — no error.
    """

    def __init__(
        self,
        client: BonsaiClient,
        on_detection: Callable[[Detection], None],
        dry_run: bool = False,
        model_dir: str = "models",
    ):
        self._client       = client
        self._on_detection = on_detection
        self._dry_run      = dry_run or os.environ.get("BONSAI_DRY_RUN", "0") == "1"
        self._rules: list[Detector] = BFD_RULES + BGP_RULES + INTERFACE_RULES
        self._stop = threading.Event()
        self._load_ml_detectors(model_dir)

    def _load_ml_detectors(self, model_dir: str) -> None:
        loaded = 0
        for filename, rule_id, threshold, severity in _ML_MODELS:
            path = os.path.join(model_dir, filename)
            if os.path.exists(path):
                try:
                    self._rules.append(MLDetector(rule_id, path, threshold, severity))
                    print(f"[engine] ML detector loaded: {rule_id} from {path}")
                    loaded += 1
                except Exception as exc:
                    print(f"[engine] WARNING: failed to load {path}: {exc}")
        if loaded == 0:
            print(f"[engine] no ML models found in '{model_dir}' — running rules-only mode")

    def start(self) -> None:
        threading.Thread(target=self._event_loop, daemon=True, name="bonsai-event-loop").start()
        threading.Thread(target=self._poll_loop,  daemon=True, name="bonsai-poll-loop").start()

    def stop(self) -> None:
        self._stop.set()

    # ── event-driven loop ─────────────────────────────────────────────────────

    def _event_loop(self) -> None:
        while not self._stop.is_set():
            try:
                for event in self._client.stream_events():
                    if self._stop.is_set():
                        break
                    self._dispatch(event)
            except Exception as exc:
                if not self._stop.is_set():
                    print(f"[engine] stream error: {exc} — reconnecting in 5s")
                    time.sleep(5)

    def _dispatch(self, event) -> None:
        for rule in self._rules:
            try:
                features = rule.extract_features(event, self._client)
                if features is None:
                    continue
                reason = rule.detect(features)
                if reason:
                    self._on_detection(Detection(
                        rule_id=rule.rule_id,
                        severity=rule.severity,
                        features=features,
                        reason=reason,
                        auto_remediate=getattr(rule, "auto_remediate", False),
                        remediation_action=getattr(rule, "remediation_action", ""),
                    ))
            except Exception as exc:
                print(f"[engine] rule {rule.rule_id} error: {exc}")

    # ── poll-based loop ───────────────────────────────────────────────────────

    def _poll_loop(self) -> None:
        while not self._stop.is_set():
            self._stop.wait(30)
            if self._stop.is_set():
                break
            try:
                self._poll_counters()
                self._poll_topology()
            except Exception as exc:
                print(f"[engine] poll error: {exc}")

    def _poll_counters(self) -> None:
        now_ns = time.time_ns()
        for iface in self._client.get_interfaces():
            addr   = iface.device_address
            name   = iface.name

            reason = InterfaceErrorSpike.evaluate_counters(
                addr, name,
                iface.in_errors, iface.out_errors,
                now_ns,
            )
            if reason:
                self._fire_poll_detection("interface_error_spike", "warn", addr, name, reason, now_ns)

            reason = InterfaceHighUtilization.evaluate_counters(
                addr, name,
                iface.in_octets, iface.out_octets,
                now_ns,
            )
            if reason:
                self._fire_poll_detection("interface_high_utilization", "warn", addr, name, reason, now_ns)

    def _poll_topology(self) -> None:
        now_ns = time.time_ns()
        edges  = self._client.get_topology()
        for device_address, if_name, reason in TOPOLOGY_RULES.evaluate_topology(edges, self._client):
            self._fire_poll_detection(
                "topology_edge_lost", "warn",
                device_address, if_name, reason, now_ns,
            )

    def _fire_poll_detection(
        self, rule_id: str, severity: str,
        device_address: str, if_name: str,
        reason: str, occurred_at_ns: int,
    ) -> None:
        import json
        features = Features(
            device_address=device_address,
            event_type="poll",
            detail={"if_name": if_name, "reason": reason},
            if_name=if_name,
            occurred_at_ns=occurred_at_ns,
        )
        self._on_detection(Detection(
            rule_id=rule_id,
            severity=severity,
            features=features,
            reason=reason,
        ))

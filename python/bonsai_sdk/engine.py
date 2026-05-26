"""Rule engine — consumes StreamEvents and dispatches to all registered Detectors."""
from __future__ import annotations

import os
import threading
import time
from typing import Callable, Optional


class DeviceSettleTracker:
    """
    Tracks when Bonsai first receives telemetry from a device and suppresses
    'down' detections during the initial settling window.

    Problem: when a gNMI stream first connects (or reconnects after Bonsai
    restarts), the router sends full current state as an initial sync burst.
    Every interface that happens to be admin-down or oper-down at that moment
    fires an event, as do any BGP peers in non-established state. These are
    not real faults — the network was already in that state before monitoring
    started. Firing detections on them pollutes the incident list.

    The settling window gives the streams time to deliver the full steady-state
    picture before detection rules are allowed to fire 'down' detections.
    Up-transitions (interface coming up, BGP establishing) are NEVER suppressed
    since they are always informational/positive.

    Window: configurable via BONSAI_SETTLE_WINDOW_SECS env var (default 90s).
    """

    _SETTLE_WINDOW_SECS: int = int(os.environ.get("BONSAI_SETTLE_WINDOW_SECS", "90"))

    def __init__(self) -> None:
        self._first_seen: dict[str, float] = {}
        self._lock = threading.Lock()

    def record(self, device_address: str) -> None:
        """Record first telemetry arrival for a device (idempotent)."""
        with self._lock:
            if device_address not in self._first_seen:
                self._first_seen[device_address] = time.monotonic()

    def is_settling(self, device_address: str) -> bool:
        """True if this device is still within its post-connect settling window."""
        with self._lock:
            first = self._first_seen.get(device_address)
        if first is None:
            return False
        return (time.monotonic() - first) < self._SETTLE_WINDOW_SECS

    def settled_at(self, device_address: str) -> Optional[float]:
        """Return the monotonic time when settling will/did complete, or None."""
        with self._lock:
            first = self._first_seen.get(device_address)
        if first is None:
            return None
        return first + self._SETTLE_WINDOW_SECS

from .client import BonsaiClient
from .detection import Detection, Detector, Features
from .ml_detector import MLDetector
from .rules.bfd import BFD_RULES
from .rules.bgp import BGP_RULES
from .rules.config import CONFIG_RULES
from .rules.interface import INTERFACE_RULES, InterfaceErrorSpike, InterfaceHighUtilization
from .rules.optical import OPTICAL_RULES
from .rules.rack import RACK_RULES
from .rules.snmp import SNMP_RULES
from .rules.streaming import STREAMING_RULES, SrlgRiskDetected
from .rules.syslog import SYSLOG_RULES
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
      3. Override poller: every 60s re-fetches DB rule overrides (enable/disable + params).

    On detection, calls the registered on_detection callback.
    Shadow-mode rules fire but route to shadow_firings instead of on_detection.

    ML models are loaded from `model_dir` (default "models/") at startup.
    If a model file is absent the engine starts in rules-only mode — no error.
    """

    def __init__(
        self,
        client: BonsaiClient,
        on_detection: Callable[[Detection], None],
        dry_run: bool = False,
        model_dir: str = "models",
        run_scope: str = "local",
    ):
        self._client       = client
        self._on_detection = on_detection
        self._dry_run      = dry_run or os.environ.get("BONSAI_DRY_RUN", "0") == "1"
        self._run_scope    = run_scope
        self._all_rules: list[Detector] = [
            r for r in (BFD_RULES + BGP_RULES + CONFIG_RULES + INTERFACE_RULES + OPTICAL_RULES + RACK_RULES + SYSLOG_RULES + SNMP_RULES + STREAMING_RULES)
            if r.scope == "hybrid" or r.scope == run_scope
        ]
        self._rules: list[Detector] = list(self._all_rules)
        self._rules_lock = threading.Lock()
        self._stop = threading.Event()
        # CV7 T4-3: monotonic counters surfaced via SidecarHeartbeat.
        self.events_received_total = 0
        # EV1-7 T3: shadow-mode firing log {rule_id: [ShadowFiring, ...]}
        self.shadow_firings: dict[str, list[dict]] = {}
        # Boot-storm suppression: track first-telemetry time per device.
        self._settle_tracker = DeviceSettleTracker()
        self._load_ml_detectors(model_dir)
        # Initial override load (non-fatal)
        try:
            self._apply_rule_overrides()
        except Exception as exc:
            print(f"[engine] rule override load skipped: {exc}")

    def _load_ml_detectors(self, model_dir: str) -> None:
        loaded = 0
        for filename, rule_id, threshold, severity in _ML_MODELS:
            path = os.path.join(model_dir, filename)
            if os.path.exists(path):
                try:
                    detector = MLDetector(rule_id, path, threshold, severity)
                    if detector.scope == "hybrid" or detector.scope == self._run_scope:
                        self._rules.append(detector)
                        print(f"[engine] ML detector loaded: {rule_id} from {path} (scope: {detector.scope})")
                        loaded += 1
                except Exception as exc:
                    print(f"[engine] WARNING: failed to load {path}: {exc}")
        if loaded == 0:
            print(f"[engine] no ML models found for scope '{self._run_scope}' in '{model_dir}' — running rules-only mode")

    def start(self) -> None:
        threading.Thread(target=self._event_loop,    daemon=True, name="bonsai-event-loop").start()
        threading.Thread(target=self._poll_loop,     daemon=True, name="bonsai-poll-loop").start()
        threading.Thread(target=self._override_loop, daemon=True, name="bonsai-override-loop").start()

    def stop(self) -> None:
        self._stop.set()

    # ── event-driven loop ─────────────────────────────────────────────────────

    def _event_loop(self) -> None:
        while not self._stop.is_set():
            try:
                stream = self._client.stream_events()
                for event in stream:
                    if self._stop.is_set():
                        break
                    self.events_received_total += 1
                    self._dispatch(event)
                # stream ended cleanly (server EOF) — reconnect immediately
                if not self._stop.is_set():
                    print("[engine] stream closed by server — reconnecting")
            except Exception as exc:
                if not self._stop.is_set():
                    print(f"[engine] stream error: {exc} -- reconnecting in 5s")
                    time.sleep(5)

    def _dispatch(self, event) -> None:
        # Record first telemetry arrival to start the per-device settling window.
        device_address = getattr(event, "device_address", "")
        if device_address:
            self._settle_tracker.record(device_address)

        with self._rules_lock:
            active_rules = list(self._rules)
        for rule in active_rules:
            try:
                features = rule.extract_features(event, self._client)
                if features is None:
                    continue
                reason = rule.detect(features)
                if reason:
                    # Boot-storm suppression: skip 'down' detections during the
                    # settling window. Up-transitions are never suppressed.
                    if self._settle_tracker.is_settling(device_address) and \
                            getattr(rule, "fires_on_down", False):
                        print(
                            f"[engine] settle-suppressed {rule.rule_id} for "
                            f"{device_address} (window active)"
                        )
                        continue
                    det = Detection(
                        rule_id=rule.rule_id,
                        severity=rule.severity,
                        features=features,
                        reason=reason,
                        auto_remediate=getattr(rule, "auto_remediate", False),
                        remediation_action=getattr(rule, "remediation_action", ""),
                    )
                    self._annotate_change_context(det)
                    if getattr(rule, "shadow_mode", False):
                        self._record_shadow_firing(rule.rule_id, det)
                    else:
                        self._on_detection(det)
            except Exception as exc:
                print(f"[engine] rule {rule.rule_id} error: {exc}")

    def _record_shadow_firing(self, rule_id: str, det: Detection) -> None:
        import time as _time
        entry = {
            "fired_at_ns": _time.time_ns(),
            "device_address": det.features.device_address,
            "reason": det.reason,
            "severity": det.severity,
        }
        if rule_id not in self.shadow_firings:
            self.shadow_firings[rule_id] = []
        self.shadow_firings[rule_id].append(entry)
        # Keep at most 500 recent shadow firings per rule
        if len(self.shadow_firings[rule_id]) > 500:
            self.shadow_firings[rule_id] = self.shadow_firings[rule_id][-500:]

    # ── poll-based loop ───────────────────────────────────────────────────────

    def _poll_loop(self) -> None:
        while not self._stop.is_set():
            self._stop.wait(30)
            if self._stop.is_set():
                break
            try:
                self._poll_counters()
                self._poll_topology()
                self._poll_streaming_graph()
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

    def _poll_streaming_graph(self) -> None:
        now_ns = time.time_ns()
        for device_address, reason in SrlgRiskDetected.evaluate_graph(self._client):
            self._fire_poll_detection(
                "srlg_risk_detected",
                "warn",
                device_address,
                "",
                reason,
                now_ns,
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
        det = Detection(
            rule_id=rule_id,
            severity=severity,
            features=features,
            reason=reason,
        )
        self._annotate_change_context(det)
        self._on_detection(det)

    # ── DB rule override poller ─────────────────────────────────────────────

    def _override_loop(self) -> None:
        while not self._stop.is_set():
            self._stop.wait(60)
            if self._stop.is_set():
                break
            try:
                self._apply_rule_overrides()
            except Exception as exc:
                print(f"[engine] override poll error: {exc}")

    def _apply_rule_overrides(self) -> None:
        """Fetch rule overrides from DB and apply enable/disable + parameter changes."""
        try:
            resp = self._client._http_json("GET", "/api/sidecar/rules")
        except Exception as exc:
            print(f"[engine] could not fetch rule overrides: {exc}")
            return

        rules_data = {r["rule_id"]: r for r in resp.get("rules", [])}

        # Fetch parameters for each rule
        params_data: dict[str, dict] = {}
        for rule_id in rules_data:
            try:
                p = self._client._http_json("GET", f"/api/sidecar/rules/{rule_id}/parameters")
                if p:
                    params_data[rule_id] = p.get("parameters", {})
            except Exception:
                pass

        with self._rules_lock:
            # Build lookup by rule_id for the full set
            all_by_id = {r.rule_id: r for r in self._all_rules}
            new_active: list[Detector] = []
            for rule in self._all_rules:
                override = rules_data.get(rule.rule_id)
                if override is not None and not override.get("enabled", True):
                    continue
                # Apply parameter overrides
                if rule.rule_id in params_data:
                    try:
                        rule.apply_parameters(params_data[rule.rule_id])
                    except Exception as exc:
                        print(f"[engine] param apply failed for {rule.rule_id}: {exc}")
                # Apply shadow mode
                if override is not None:
                    rule.shadow_mode = override.get("shadow_mode", False)
                new_active.append(rule)
            changed = len(new_active) != len(self._rules) or {
                r.rule_id for r in new_active
            } != {r.rule_id for r in self._rules}
            self._rules = new_active
            if changed:
                print(f"[engine] rule overrides applied: {len(new_active)}/{len(self._all_rules)} active")

    # ── change context overlay ────────────────────────────────────────────────

    def _annotate_change_context(self, det: Detection) -> None:
        """Check if the device is in an active change window and annotate the detection."""
        try:
            resp = self._client._http_json(
                "GET",
                f"/api/changes/context/{det.features.device_address}",
            )
            if resp.get("in_change_window"):
                det.change_correlated = True
                det.features.change_correlated = True
                det.features.change_refs = [
                    {"id": c.get("id", ""), "number": c.get("number", ""), "source": c.get("source", "")}
                    for c in resp.get("change_requests", [])
                ]
                change_nums = ", ".join(
                    c.get("number", c.get("id", "")) for c in resp.get("change_requests", [])
                )
                det.reason = f"[DURING CHANGE {change_nums}] {det.reason}"
        except Exception:
            pass  # Non-fatal: if change context is unavailable, detection fires normally

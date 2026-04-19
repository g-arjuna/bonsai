"""Remediation executor with circuit breaker and dry-run support.

Execution is now delegated to PlaybookExecutor, which walks YAML playbook steps.
The circuit breaker, dry-run flag, and auto_remediate whitelist remain here.
"""
from __future__ import annotations

import json
import os
import time
import threading
from collections import defaultdict, deque
from typing import Callable, Optional

from .client import BonsaiClient
from .detection import Detection
from .ml_remediation import MLRemediationSelector
from .playbooks import PlaybookCatalog, PlaybookExecutor

CIRCUIT_BREAKER_WINDOW_S = 600    # 10 minutes
CIRCUIT_BREAKER_MAX      = 5      # max auto-remediations per device in window


class RemediationExecutor:
    """
    Selects a playbook for each Detection, executes it (or skips with reason),
    writes the Remediation node to the graph, and calls on_remediation callback.

    Safety layers (in order):
      1. BONSAI_DRY_RUN=1 — log only, no Set sent
      2. auto_remediate=True must be set on the rule (whitelist)
      3. Circuit breaker — ≥5 remediations for same device in 10 min → halt
    """

    def __init__(
        self,
        client: BonsaiClient,
        on_remediation: Optional[Callable] = None,
        catalog: Optional[PlaybookCatalog] = None,
        ml_selector: Optional["MLRemediationSelector"] = None,
    ) -> None:
        self._client         = client
        self._on_remediation = on_remediation
        self._dry_run        = os.environ.get("BONSAI_DRY_RUN", "0") == "1"
        self._breaker: dict[str, deque[float]] = defaultdict(deque)
        self._lock           = threading.Lock()
        self._catalog        = catalog or PlaybookCatalog()
        self._pb_executor    = PlaybookExecutor(
            catalog=self._catalog,
            client=client,
            on_step=lambda t, d: None,
        )
        # Optional Model C selector — when present, overrides catalog ordering.
        self._ml_selector: Optional[MLRemediationSelector] = ml_selector

    def handle(self, detection: Detection, detection_id: str) -> None:
        device = detection.features.device_address
        now    = time.time()

        if not detection.auto_remediate:
            self._write_remediation(detection_id, "log_only", "skipped",
                                    {"reason": "rule not whitelisted for auto-remediation"}, now)
            return

        if self._dry_run:
            self._write_remediation(detection_id, "log_only", "skipped",
                                    {"reason": "dry-run mode (BONSAI_DRY_RUN=1)"}, now)
            return

        if self._circuit_breaker_tripped(device, now):
            self._write_remediation(detection_id, "log_only", "skipped",
                                    {"reason": f"circuit breaker: >{CIRCUIT_BREAKER_MAX} remediations "
                                               f"for {device} in last {CIRCUIT_BREAKER_WINDOW_S}s"}, now)
            return

        # Look up the device vendor for playbook selection.
        vendor = self._get_vendor(device)

        # Build the candidate playbook list for this detection.
        candidates = self._catalog.for_detection(detection.rule_id, vendor)
        if not candidates:
            self._write_remediation(detection_id, "log_only", "skipped",
                                    {"reason": f"no playbook for rule={detection.rule_id} vendor={vendor}"}, now)
            return

        # ML selector picks the best candidate when loaded and confident;
        # falls back to catalog ordering (first match) when confidence is low.
        playbook = None
        if self._ml_selector is not None:
            candidate_names = [p.get("name", "") for p in candidates]
            chosen = self._ml_selector.select(detection, candidate_names)
            if chosen:
                playbook = next((p for p in candidates if p.get("name") == chosen), None)
        if playbook is None:
            playbook = self._pb_executor.select(detection, vendor)
        if playbook is None:
            self._write_remediation(detection_id, "log_only", "skipped",
                                    {"reason": f"no playbook selected for rule={detection.rule_id}"}, now)
            return

        action = playbook.get("name", "unknown_playbook")
        success, error = self._pb_executor.execute(playbook, detection)
        status  = "success" if success else "failed"
        detail  = {} if success else {"error": error}
        self._record_breaker(device, now)
        self._write_remediation(detection_id, action, status, detail, now)

    # ── helpers ───────────────────────────────────────────────────────────────

    def _get_vendor(self, device_address: str) -> str:
        try:
            devices = self._client.get_devices()
            for d in devices:
                if d.address == device_address:
                    return d.vendor
        except Exception:
            pass
        return ""

    def _write_remediation(
        self, detection_id: str, action: str, status: str,
        detail: dict, attempted_at: float
    ) -> None:
        completed_at_ns = int(time.time() * 1e9)
        attempted_at_ns = int(attempted_at * 1e9)
        try:
            self._client.create_remediation(
                detection_id=detection_id,
                action=action,
                status=status,
                detail_json=json.dumps(detail),
                attempted_at_ns=attempted_at_ns,
                completed_at_ns=completed_at_ns,
            )
            if self._on_remediation:
                self._on_remediation(action, status, detail)
        except Exception as exc:
            print(f"[remediations] failed to write remediation: {exc}")

    def _circuit_breaker_tripped(self, device: str, now: float) -> bool:
        with self._lock:
            dq = self._breaker[device]
            cutoff = now - CIRCUIT_BREAKER_WINDOW_S
            while dq and dq[0] < cutoff:
                dq.popleft()
            return len(dq) >= CIRCUIT_BREAKER_MAX

    def _record_breaker(self, device: str, now: float) -> None:
        with self._lock:
            self._breaker[device].append(now)

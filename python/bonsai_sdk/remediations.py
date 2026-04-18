"""Remediation executor with circuit breaker and dry-run support."""
from __future__ import annotations

import json
import os
import time
import threading
from collections import defaultdict, deque
from typing import Callable

from .client import BonsaiClient
from .detection import Detection

# Vendor-specific gNMI Set paths for BGP soft-clear.
# Key: vendor label as stored on Device.vendor in the graph.
# SRL exposes BGP session reset only as a config admin-state toggle — not as an action/RPC.
# admin-state bounce (disable then enable) is the only gNMI-accessible reset on SRL.
_BGP_ADMIN_STATE_PATH: dict[str, str] = {
    "nokia_srl": "network-instance[name=default]/protocols/bgp/neighbor[peer-address={peer}]/admin-state",
    # XRd: no standard OC gNMI Set path confirmed; skip for now.
}


def _bgp_admin_state_path(vendor: str, peer_address: str) -> str | None:
    template = _BGP_ADMIN_STATE_PATH.get(vendor)
    return template.format(peer=peer_address) if template else None


PLAYBOOKS: dict[str, bool] = {
    "bgp_session_bounce":        True,
    "log_only":                  False,
}

CIRCUIT_BREAKER_WINDOW_S  = 600    # 10 minutes
CIRCUIT_BREAKER_MAX       = 5      # max auto-remediations per device in window


class RemediationExecutor:
    """
    Selects a playbook for each Detection, executes it (or skips with reason),
    writes the Remediation node to the graph, and calls on_remediation callback.

    Safety layers (in order):
      1. BONSAI_DRY_RUN=1 — log only, no Set sent
      2. auto_remediate=True must be set on the rule (whitelist)
      3. Circuit breaker — ≥5 remediations for same device in 10 min → halt
    """

    def __init__(self, client: BonsaiClient, on_remediation: Callable | None = None):
        self._client         = client
        self._on_remediation = on_remediation
        self._dry_run        = os.environ.get("BONSAI_DRY_RUN", "0") == "1"
        self._breaker: dict[str, deque[float]] = defaultdict(deque)
        self._lock = threading.Lock()

    def handle(self, detection: Detection, detection_id: str) -> None:
        device = detection.features.device_address
        action = detection.remediation_action or "log_only"
        now    = time.time()

        # Decide whether to auto-heal or skip
        if not detection.auto_remediate:
            self._write_remediation(detection_id, action, "skipped",
                                    {"reason": "rule not whitelisted for auto-remediation"}, now)
            return

        if self._dry_run:
            self._write_remediation(detection_id, action, "skipped",
                                    {"reason": "dry-run mode (BONSAI_DRY_RUN=1)"}, now)
            return

        if self._circuit_breaker_tripped(device, now):
            self._write_remediation(detection_id, action, "skipped",
                                    {"reason": f"circuit breaker: >{CIRCUIT_BREAKER_MAX} remediations "
                                               f"for {device} in last {CIRCUIT_BREAKER_WINDOW_S}s"}, now)
            return

        success, error = self._execute(detection, action)
        status  = "success" if success else "failed"
        detail  = {} if success else {"error": error}
        self._record_breaker(device, now)
        self._write_remediation(detection_id, action, status, detail, now)

    def _execute(self, detection: Detection, action: str) -> tuple[bool, str]:
        if action == "bgp_session_bounce":
            return self._bgp_session_bounce(detection)
        return False, f"unknown action '{action}'"

    def _bgp_session_bounce(self, detection: Detection) -> tuple[bool, str]:
        """Admin-state disable → enable on the BGP neighbor (SRL only gNMI-accessible reset)."""
        import time
        device = detection.features.device_address
        peer   = detection.features.peer_address
        if not peer:
            return False, "no peer_address in features"
        try:
            devices = self._client.get_devices()
            vendor  = next((d.vendor for d in devices if d.address == device), "")
            path    = _bgp_admin_state_path(vendor, peer)
            if path is None:
                return False, f"no BGP admin-state path defined for vendor '{vendor}'"
            # Step 1 — disable
            resp = self._client.push_remediation(device, path, '"disable"')
            if not resp.success:
                return False, f"disable failed: {resp.error}"
            time.sleep(1)
            # Step 2 — enable
            resp = self._client.push_remediation(device, path, '"enable"')
            if not resp.success:
                return False, f"enable failed: {resp.error}"
            return True, ""
        except Exception as exc:
            return False, str(exc)

    def _write_remediation(
        self, detection_id: str, action: str, status: str,
        detail: dict, attempted_at: float
    ) -> None:
        completed_at_ns = int(time.time() * 1e9)
        attempted_at_ns = int(attempted_at * 1e9)
        try:
            resp = self._client.create_remediation(
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

"""Remediation executor with circuit breaker and dry-run support.

Execution is now delegated to PlaybookExecutor, which walks YAML playbook steps.
The circuit breaker, dry-run flag, and auto_remediate whitelist remain here.
"""
from __future__ import annotations

import json
import os
import time
import threading
import dataclasses
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
      1. TrustState decides whether to propose or execute.
      2. BONSAI_DRY_RUN=1 forces proposal-only behavior.
      3. auto_remediate=True is required before any automatic approval.
      4. Circuit breaker halts automatic approvals for noisy devices.
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
        environment_archetype, site_id = self._trust_context(device)
        rendered_steps = _render_steps(playbook.get("steps", []), detection.features)
        verification = _render_verification(playbook.get("verification"), detection.features)
        if verification:
            rendered_steps.append({"verify_graph": verification})
        steps_json = json.dumps(rendered_steps)
        rollback_steps_json = json.dumps(_render_steps(playbook.get("rollback_steps", []), detection.features))

        try:
            proposal = self._client.create_remediation_proposal(
                detection_id=detection_id,
                playbook_id=action,
                rule_id=detection.rule_id,
                environment_archetype=environment_archetype,
                site_id=site_id,
                steps_json=steps_json,
                rollback_steps_json=rollback_steps_json,
            )
        except Exception as exc:
            self._write_remediation(detection_id, action, "failed",
                                    {"reason": f"failed to create remediation proposal: {exc}"}, now)
            return

        trust_state = proposal.get("trust_state", "approve_each")
        proposal_id = proposal.get("proposal_id", "")
        if trust_state in ("suggest_only", "approve_each") or self._dry_run:
            reason = f"proposal queued under trust_state={trust_state}"
            if self._dry_run:
                reason += " (dry-run forces approval)"
            self._write_remediation(detection_id, action, "pending_approval",
                                    {"proposal_id": proposal_id, "reason": reason}, now)
            return

        if not detection.auto_remediate:
            self._write_remediation(detection_id, action, "pending_approval",
                                    {"proposal_id": proposal_id,
                                     "reason": "rule not whitelisted for automatic approval"}, now)
            return

        if self._circuit_breaker_tripped(device, now):
            self._write_remediation(detection_id, action, "pending_approval",
                                    {"proposal_id": proposal_id,
                                     "reason": f"circuit breaker: >{CIRCUIT_BREAKER_MAX} remediations "
                                               f"for {device} in last {CIRCUIT_BREAKER_WINDOW_S}s"}, now)
            return

        try:
            result = self._client.approve_remediation_proposal(
                proposal_id,
                operator_note=f"auto-approved under trust_state={trust_state}",
            )
            if not result.get("success", False):
                raise RuntimeError(result.get("error") or "automatic proposal approval failed")
            self._record_breaker(device, now)
        except Exception as exc:
            self._write_remediation(detection_id, action, "failed",
                                    {"proposal_id": proposal_id, "error": str(exc)}, now)

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

    def _trust_context(self, device_address: str) -> tuple[str, str]:
        escaped = device_address.replace("\\", "\\\\").replace('"', '\\"')
        try:
            rows = self._client.query(
                f'MATCH (d:Device {{address: "{escaped}"}}) '
                'OPTIONAL MATCH (d)-[:LOCATED_AT]->(s:Site) '
                'OPTIONAL MATCH (s)-[:BELONGS_TO_ENVIRONMENT]->(env:Environment) '
                'RETURN s.id, env.archetype LIMIT 1'
            )
            if rows:
                site_id = rows[0][0] or ""
                archetype = rows[0][1] or ""
                return archetype, site_id
        except Exception:
            pass
        return "", ""

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


def _render_steps(steps: list[dict], features) -> list[dict]:
    rendered: list[dict] = []
    feature_dict = dataclasses.asdict(features)
    for step in steps:
        if "gnmi_set" in step:
            op = step["gnmi_set"]
            rendered.append({
                "gnmi_set": {
                    "path": _format_template(op.get("path", ""), feature_dict),
                    "value": _format_template(op.get("value", ""), feature_dict),
                }
            })
        elif "sleep" in step:
            rendered.append({"sleep": step["sleep"]})
    return rendered


def _format_template(template: str, values: dict) -> str:
    try:
        return template.format(**values)
    except KeyError:
        return template


def _render_verification(verification: dict | None, features) -> dict | None:
    if not verification:
        return None
    cypher = verification.get("expected_graph_state") or verification.get("cypher")
    if not cypher:
        return None
    values = dataclasses.asdict(features)
    filled = (cypher
              .replace("$device_address", json.dumps(values.get("device_address", "")))
              .replace("$peer_address", json.dumps(values.get("peer_address", "")))
              .replace("$if_name", json.dumps(values.get("if_name", ""))))
    return {
        "expected_graph_state": filled,
        "wait_seconds": int(verification.get("wait_seconds", 30)),
    }

"""PlaybookExecutor — walks playbook steps, calls push_remediation, verifies recovery."""
from __future__ import annotations

import time
from typing import TYPE_CHECKING, Any, Callable, Optional

from .catalog import PlaybookCatalog

if TYPE_CHECKING:
    from ..client import BonsaiClient
    from ..detection import Detection, Features


class PlaybookExecutor:
    """
    Selects a playbook for a detection, executes its steps via push_remediation,
    and verifies recovery against the graph.

    Safety layers are enforced by the caller (RemediationExecutor):
      - dry-run flag
      - auto_remediate whitelist
      - circuit breaker
    This class only handles playbook selection, step execution, and verification.
    """

    def __init__(
        self,
        catalog: PlaybookCatalog,
        client: "BonsaiClient",
        on_step: Optional[Callable[[str, str], None]] = None,
    ) -> None:
        self._catalog = catalog
        self._client  = client
        self._on_step = on_step  # callback(step_type, description) for logging

    def select(self, detection: "Detection", vendor: str) -> Optional[dict[str, Any]]:
        """Return the first applicable playbook for this detection+vendor, or None."""
        candidates = self._catalog.for_detection(detection.rule_id, vendor)
        for playbook in candidates:
            if self._preconditions_met(playbook, detection.features):
                return playbook
        return None

    def execute(self, playbook: dict[str, Any], detection: "Detection") -> tuple[bool, str]:
        """Walk playbook steps. Returns (success, error_message)."""
        device  = detection.features.device_address
        steps   = playbook.get("steps", [])

        if not steps:
            return True, ""

        for step in steps:
            if "gnmi_set" in step:
                path  = _interpolate(step["gnmi_set"]["path"],  detection.features)
                value = _interpolate(step["gnmi_set"]["value"], detection.features)
                if self._on_step:
                    self._on_step("gnmi_set", f"{path} = {value}")
                resp = self._client.push_remediation(device, path, value)
                if not resp.success:
                    return False, f"gnmi_set failed: {resp.error}"
            elif "sleep" in step:
                time.sleep(float(step["sleep"]))

        return True, ""

    def verify(self, playbook: dict[str, Any], detection: "Detection") -> bool:
        """Poll the graph for recovery confirmation. Returns True if confirmed."""
        vfy = playbook.get("verification")
        if not vfy:
            return True
        # Canonical field name is `expected_graph_state` (matches all YAML in
        # playbooks/library/). `cypher` is kept as a legacy alias so any
        # hand-written playbooks using the old name still work.
        cypher = vfy.get("expected_graph_state") or vfy.get("cypher")
        if not cypher:
            return True
        wait_seconds = int(vfy.get("wait_seconds", 30))
        peer         = detection.features.peer_address
        device       = detection.features.device_address
        if_name      = detection.features.if_name

        # Substitute named params into the Cypher query string.
        cypher_filled = (cypher
            .replace("$device_address", f'"{device}"')
            .replace("$peer_address",   f'"{peer}"')
            .replace("$if_name",        f'"{if_name}"'))

        deadline = time.time() + wait_seconds
        while time.time() < deadline:
            try:
                rows = self._client.query(cypher_filled)
                if rows and rows[0] and rows[0][0]:
                    return True
            except Exception:
                pass
            time.sleep(3)
        return False

    # ── internals ────────────────────────────────────────────────────────────

    def _preconditions_met(self, playbook: dict, features: "Features") -> bool:
        for expr in playbook.get("preconditions", []):
            try:
                if not eval(expr, {}, {"features": features}):  # noqa: S307
                    return False
            except Exception:
                return False
        return True


def _interpolate(template: str, features: "Features") -> str:
    """Replace {field_name} tokens with values from features."""
    import dataclasses
    d = dataclasses.asdict(features)
    try:
        return template.format(**d)
    except KeyError:
        return template

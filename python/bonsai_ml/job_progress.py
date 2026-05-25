"""Job progress reporting for Bonsai ML Job Engine.

EV1-5 T4: JobProgressReporter emits MlEvent progress updates during long-running
jobs (training epochs, export rows, embedding batches).

Usage:
    async def my_training_job(reporter: JobProgressReporter):
        reporter.set_total(max_epochs)
        for epoch in range(max_epochs):
            # ... train ...
            reporter.report(epoch + 1, metric_name="loss", metric_value=loss)
"""
from __future__ import annotations

import logging
import time
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_API_URL = "http://localhost:3000"


class JobProgressReporter:
    """Emits JobProgress ML events for a running job.

    Args:
        job_id: The job identifier (e.g., "stgnn_training").
        api_url: Bonsai core API URL for publishing events.
        emit_interval_s: Minimum seconds between progress emissions to avoid
            flooding the SSE bus on tight loops.
    """

    def __init__(
        self,
        job_id: str,
        api_url: str = DEFAULT_API_URL,
        emit_interval_s: float = 2.0,
    ) -> None:
        self.job_id = job_id
        self.api_url = api_url.rstrip("/")
        self.emit_interval_s = emit_interval_s
        self._total_steps: Optional[int] = None
        self._last_emit: float = 0.0
        self._step: int = 0

    def set_total(self, total_steps: int) -> None:
        """Set total steps once known (e.g. after dataset is loaded)."""
        self._total_steps = total_steps

    def report(
        self,
        step: int,
        metric_name: Optional[str] = None,
        metric_value: Optional[float] = None,
        force: bool = False,
    ) -> None:
        """Emit a progress event.

        Args:
            step: Current step count (1-indexed, e.g. current epoch).
            metric_name: Optional metric label (e.g., "loss", "rows_written").
            metric_value: Optional metric value.
            force: If True, emit even if within throttle window.
        """
        self._step = step
        now = time.monotonic()
        if not force and (now - self._last_emit) < self.emit_interval_s:
            return

        self._last_emit = now
        payload: dict[str, Any] = {
            "job_id": self.job_id,
            "step": step,
            "total_steps": self._total_steps,
            "pct": round(step / self._total_steps * 100, 1) if self._total_steps else None,
        }
        if metric_name is not None:
            payload["metric_name"] = metric_name
        if metric_value is not None:
            payload["metric_value"] = round(float(metric_value), 6)

        self._publish(payload)

    def _publish(self, payload: dict) -> None:
        try:
            import requests
            requests.post(
                f"{self.api_url}/api/ml/events/publish",
                json={"event_type": "job_progress", "payload": payload},
                timeout=2,
            )
        except Exception as exc:
            log.debug("JobProgressReporter: failed to publish: %s", exc)

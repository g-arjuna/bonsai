#!/usr/bin/env python3
"""
Bonsai SSE event stream driver (T3-3).

Connects to /api/events, collects events for `--duration` seconds, validates
each event's shape, and writes a structured JSON result to
runtime/driver_results/event.json.

Usage:
    python tests/event_driver/run.py [--base-url http://localhost:3000] [--duration 30]
"""

import argparse
import json
import os
import sys
import time
from dataclasses import asdict, dataclass, field
from typing import Any

import requests

BASE_URL = os.environ.get("BONSAI_URL", "http://localhost:3000")

REQUIRED_EVENT_KEYS = {"device_address", "event_type", "occurred_at_ns"}
KNOWN_EVENT_TYPES = {
    "state_change", "detection", "remediation_proposed",
    "remediation_applied", "registry_added", "registry_updated", "registry_removed",
}


@dataclass
class EventResult:
    driver: str = "event"
    ts_unix: int = 0
    base_url: str = ""
    duration_secs: float = 0.0
    connected: bool = False
    events_received: int = 0
    events_valid: int = 0
    events_invalid: int = 0
    unknown_types: list[str] = field(default_factory=list)
    validation_errors: list[str] = field(default_factory=list)
    ok: bool = False
    error: str = ""


def validate_event(raw: str) -> tuple[bool, str, dict[str, Any]]:
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as e:
        return False, f"JSON parse error: {e}", {}

    missing = REQUIRED_EVENT_KEYS - set(payload.keys())
    if missing:
        return False, f"missing keys: {sorted(missing)}", payload

    if not isinstance(payload.get("occurred_at_ns"), (int, float)):
        return False, "occurred_at_ns is not numeric", payload

    return True, "", payload


def run(base_url: str, duration_secs: float) -> EventResult:
    result = EventResult(
        driver="event",
        ts_unix=int(time.time()),
        base_url=base_url,
        duration_secs=duration_secs,
    )

    url = f"{base_url}/api/events"
    try:
        resp = requests.get(url, stream=True, timeout=(5, duration_secs + 5),
                            headers={"Accept": "text/event-stream"})
    except Exception as e:
        result.error = f"connection failed: {e}"
        return result

    if resp.status_code != 200:
        result.error = f"HTTP {resp.status_code}"
        return result

    result.connected = True
    deadline = time.monotonic() + duration_secs
    seen_types: set[str] = set()

    try:
        for line in resp.iter_lines(decode_unicode=True):
            if time.monotonic() > deadline:
                break
            if not line or line.startswith(":"):
                continue
            if line.startswith("data:"):
                raw = line[5:].strip()
                if not raw:
                    continue
                result.events_received += 1
                ok, err, payload = validate_event(raw)
                if ok:
                    result.events_valid += 1
                    etype = payload.get("event_type", "")
                    seen_types.add(etype)
                    if etype and etype not in KNOWN_EVENT_TYPES:
                        result.unknown_types.append(etype)
                else:
                    result.events_invalid += 1
                    result.validation_errors.append(f"event {result.events_received}: {err}")
    except Exception as e:
        result.error = f"stream read error: {e}"

    result.ok = result.connected and result.events_invalid == 0
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default=BASE_URL)
    parser.add_argument("--duration", type=float, default=30.0,
                        help="seconds to listen for events")
    parser.add_argument("--output", default="runtime/driver_results/event.json")
    args = parser.parse_args()

    r = run(args.base_url, args.duration)
    r.ts_unix = int(time.time())

    output = json.dumps(asdict(r), indent=2)
    print(output)

    os.makedirs(os.path.dirname(args.output), exist_ok=True)
    with open(args.output, "w") as f:
        f.write(output)

    status = "PASS" if r.ok else ("FAIL" if not r.connected else "WARN")
    print(
        f"\nEvent driver [{status}]: connected={r.connected}, "
        f"received={r.events_received}, valid={r.events_valid}, invalid={r.events_invalid}",
        file=sys.stderr,
    )
    return 0 if r.ok else 1


if __name__ == "__main__":
    sys.exit(main())

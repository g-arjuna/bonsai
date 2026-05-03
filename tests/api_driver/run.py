#!/usr/bin/env python3
"""
Bonsai API contract driver (T3-2).

Exercises every documented REST endpoint and writes a structured JSON result
to runtime/driver_results/api.json for consumption by /api/_test/status and AI.

Usage:
    python tests/api_driver/run.py [--base-url http://localhost:3000] [--output runtime/driver_results/api.json]
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


@dataclass
class CaseResult:
    name: str
    method: str
    path: str
    status: int
    ok: bool
    error: str = ""
    response_keys: list[str] = field(default_factory=list)
    duration_ms: float = 0.0


@dataclass
class DriverResult:
    driver: str = "api"
    ts_unix: int = 0
    base_url: str = ""
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    cases: list[CaseResult] = field(default_factory=list)


def get(session: requests.Session, path: str) -> tuple[requests.Response | None, float, str]:
    t0 = time.monotonic()
    try:
        r = session.get(f"{BASE_URL}{path}", timeout=10)
        ms = (time.monotonic() - t0) * 1000
        return r, ms, ""
    except Exception as e:
        ms = (time.monotonic() - t0) * 1000
        return None, ms, str(e)


def check(
    result: DriverResult,
    name: str,
    path: str,
    *,
    method: str = "GET",
    expected_status: int = 200,
    required_keys: list[str] | None = None,
    skip_if_empty: bool = False,
    session: requests.Session,
) -> dict[str, Any] | None:
    r, ms, err = get(session, path)
    if r is None:
        case = CaseResult(
            name=name, method=method, path=path, status=0,
            ok=False, error=err, duration_ms=ms,
        )
        result.cases.append(case)
        result.failed += 1
        return None

    try:
        body = r.json()
    except Exception:
        body = {}

    keys = list(body.keys()) if isinstance(body, dict) else []

    missing = []
    if required_keys and r.status_code == expected_status:
        missing = [k for k in required_keys if k not in body]

    ok = r.status_code == expected_status and not missing
    error = ""
    if r.status_code != expected_status:
        error = f"HTTP {r.status_code} (expected {expected_status})"
    elif missing:
        error = f"missing keys: {missing}"

    case = CaseResult(
        name=name, method=method, path=path, status=r.status_code,
        ok=ok, error=error, response_keys=keys, duration_ms=round(ms, 1),
    )
    result.cases.append(case)
    if ok:
        result.passed += 1
    else:
        result.failed += 1

    return body if ok else None


def run(base_url: str) -> DriverResult:
    global BASE_URL
    BASE_URL = base_url

    result = DriverResult(
        driver="api",
        ts_unix=int(time.time()),
        base_url=base_url,
    )

    s = requests.Session()
    s.headers["Accept"] = "application/json"

    # ── Core topology + graph ─────────────────────────────────────────────────
    check(result, "topology", "/api/topology",
          required_keys=["devices", "links"], session=s)

    check(result, "detections", "/api/detections",
          required_keys=["detections"], session=s)

    check(result, "incidents", "/api/incidents",
          required_keys=["incidents"], session=s)

    check(result, "incidents_grouped", "/api/incidents/grouped",
          required_keys=["incidents"], session=s)

    # ── Operations + readiness ────────────────────────────────────────────────
    check(result, "readiness", "/api/readiness",
          required_keys=["detection_events", "state_change_events"], session=s)

    ops = check(result, "operations", "/api/operations",
                required_keys=["detection_events", "device_count",
                               "rss_bytes", "archive_disk_bytes", "graph_disk_bytes"],
                session=s)

    # ── Test status (T3-5) ────────────────────────────────────────────────────
    check(result, "test_status", "/api/_test/status",
          required_keys=["ts_unix", "memory", "disk"], session=s)

    # ── Devices + onboarding ──────────────────────────────────────────────────
    check(result, "managed_devices", "/api/onboarding/devices",
          required_keys=["devices"], session=s)

    check(result, "path_catalogue", "/api/path",
          required_keys=[], session=s)

    # ── Enrichment ────────────────────────────────────────────────────────────
    check(result, "enrichers", "/api/enrichers",
          required_keys=[], session=s)

    check(result, "environments", "/api/environments",
          required_keys=[], session=s)

    check(result, "sites", "/api/sites",
          required_keys=[], session=s)

    # ── Output adapters ───────────────────────────────────────────────────────
    check(result, "adapters", "/api/adapters",
          required_keys=[], session=s)

    # ── Collectors + assignment ───────────────────────────────────────────────
    check(result, "collectors", "/api/collectors",
          required_keys=[], session=s)

    check(result, "assignment_rules", "/api/assignment/rules",
          required_keys=[], session=s)

    check(result, "assignment_status", "/api/assignment/status",
          required_keys=[], session=s)

    # ── Credentials ───────────────────────────────────────────────────────────
    check(result, "credentials", "/api/credentials",
          required_keys=[], session=s)

    # ── Trust + remediation ───────────────────────────────────────────────────
    check(result, "trust_state", "/api/trust/state",
          required_keys=[], session=s)

    check(result, "overrides", "/api/overrides",
          required_keys=[], session=s)

    # ── Trace: requires a real detection event ID — skip if none found ────────
    detections_body, _, _ = get(s, "/api/detections")
    if detections_body is not None:
        try:
            body = detections_body.json() if hasattr(detections_body, 'json') else {}
            events = body.get("detections", [])
        except Exception:
            events = []
        if events:
            eid = events[0].get("id", "")
            if eid:
                check(result, "trace", f"/api/trace/{eid}",
                      required_keys=["id", "steps"], session=s)
            else:
                result.skipped += 1
        else:
            result.skipped += 1
    else:
        result.skipped += 1

    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default=BASE_URL)
    parser.add_argument("--output", default="runtime/driver_results/api.json")
    args = parser.parse_args()

    r = run(args.base_url)
    r.ts_unix = int(time.time())

    output = json.dumps(asdict(r), indent=2)
    print(output)

    os.makedirs(os.path.dirname(args.output), exist_ok=True)
    with open(args.output, "w") as f:
        f.write(output)

    total = r.passed + r.failed
    print(f"\nAPI driver: {r.passed}/{total} passed, {r.skipped} skipped", file=sys.stderr)
    return 0 if r.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())

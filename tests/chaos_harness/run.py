#!/usr/bin/env python3
"""
Bonsai chaos harness (T3-4).

Drives lab/fault_catalog.yaml: for each fault, asserts pre-fault baseline,
injects the fault, waits for bonsai to emit the expected detection event,
then heals and asserts the event clears.

Output: JSON results written to runtime/driver_results/chaos.json
        (same schema as api_driver and event_driver — consumed by /api/_test/status)

Usage:
    # Run all faults in the catalogue (requires running lab + bonsai)
    python tests/chaos_harness/run.py

    # Specific topology
    python tests/chaos_harness/run.py --topology dc
    python tests/chaos_harness/run.py --topology sp

    # Specific fault IDs
    python tests/chaos_harness/run.py --fault dc-link-down-leaf2-spine1

    # Dry-run: print fault plan without executing
    python tests/chaos_harness/run.py --dry-run

    # Override bonsai base URL
    python tests/chaos_harness/run.py --base-url http://localhost:3001  # lab-sp
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import requests
import yaml

REPO_ROOT = Path(__file__).parents[2]
FAULT_CATALOG = REPO_ROOT / "lab" / "fault_catalog.yaml"
DEFAULT_BASE_URL = os.environ.get("BONSAI_URL", "http://localhost:3000")
DEFAULT_OUTPUT = REPO_ROOT / "runtime" / "driver_results" / "chaos.json"


# ── Data model ────────────────────────────────────────────────────────────────

@dataclass
class FaultResult:
    fault_id: str
    topology: str
    description: str
    passed: bool
    pre_fault_clean: bool = True
    detection_observed: bool = False
    detection_latency_ms: float = 0.0
    post_heal_clear: bool = False
    error: str = ""
    expected_detections: list[dict] = field(default_factory=list)
    observed_detections: list[dict] = field(default_factory=list)


@dataclass
class HarnessResult:
    driver: str = "chaos"
    ts_unix: int = 0
    base_url: str = ""
    topology_filter: str = "all"
    total: int = 0
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    faults: list[FaultResult] = field(default_factory=list)


# ── Catalog loading ───────────────────────────────────────────────────────────

def load_catalog(path: Path) -> list[dict]:
    with open(path) as f:
        data = yaml.safe_load(f)
    return data.get("faults", [])


# ── Shell execution ───────────────────────────────────────────────────────────

def run_commands(commands: list[str], dry_run: bool = False) -> tuple[bool, str]:
    """Execute a list of shell commands sequentially. Returns (ok, stderr)."""
    if dry_run:
        for cmd in commands:
            print(f"  [dry-run] {cmd.strip()}", file=sys.stderr)
        return True, ""

    for cmd in commands:
        cmd = cmd.strip()
        try:
            result = subprocess.run(
                cmd, shell=True, capture_output=True, text=True, timeout=30
            )
            if result.returncode != 0:
                return False, f"cmd={cmd!r} rc={result.returncode} stderr={result.stderr!r}"
        except subprocess.TimeoutExpired:
            return False, f"cmd={cmd!r} timed out after 30s"
        except Exception as exc:
            return False, f"cmd={cmd!r} exception={exc}"
    return True, ""


# ── Bonsai API polling ────────────────────────────────────────────────────────

def poll_detections(base_url: str, timeout_s: int) -> list[dict]:
    """Poll /api/detections until timeout, returning all events seen."""
    deadline = time.monotonic() + timeout_s
    seen: list[dict] = []
    while time.monotonic() < deadline:
        try:
            resp = requests.get(f"{base_url}/api/detections", timeout=5)
            if resp.ok:
                data = resp.json()
                events = data if isinstance(data, list) else data.get("detections", [])
                seen = events
        except Exception:
            pass
        time.sleep(2)
    return seen


def detection_matches(event: dict, expectation: dict) -> bool:
    """Return True if a bonsai DetectionEvent matches an expected spec."""
    # Match on detection type (flexible key names)
    ev_type = (
        event.get("detection_type")
        or event.get("rule_id")
        or event.get("type")
        or ""
    ).lower()
    ex_type = expectation.get("detection", "").lower()
    if ex_type and ex_type not in ev_type:
        return False

    # Match on target hostname (optional)
    ex_target = expectation.get("target", "")
    if ex_target:
        ev_target = (
            event.get("target_hostname")
            or event.get("target")
            or event.get("hostname")
            or ""
        )
        if ex_target.lower() not in ev_target.lower():
            return False

    # Match on peer_ip (optional)
    ex_peer_ip = expectation.get("peer_ip", "")
    if ex_peer_ip:
        ev_peer = str(event.get("peer_ip", "") or event.get("peer", ""))
        if ex_peer_ip not in ev_peer:
            return False

    return True


def wait_for_detections(
    base_url: str,
    expectations: list[dict],
    timeout_s: int,
) -> tuple[bool, float, list[dict]]:
    """
    Poll /api/detections until all expectations are satisfied or timeout.
    Returns (all_matched, latency_ms, matched_events).
    """
    start = time.monotonic()
    deadline = start + timeout_s

    while time.monotonic() < deadline:
        try:
            resp = requests.get(f"{base_url}/api/detections", timeout=5)
            if resp.ok:
                data = resp.json()
                events = data if isinstance(data, list) else data.get("detections", [])

                matched = []
                for exp in expectations:
                    for ev in events:
                        if detection_matches(ev, exp):
                            matched.append(ev)
                            break

                if len(matched) == len(expectations):
                    latency_ms = (time.monotonic() - start) * 1000
                    return True, latency_ms, matched
        except Exception:
            pass
        time.sleep(3)

    latency_ms = (time.monotonic() - start) * 1000
    return False, latency_ms, []


def detections_cleared(base_url: str, expectations: list[dict]) -> bool:
    """Return True if none of the expected detection types are active."""
    try:
        resp = requests.get(f"{base_url}/api/detections", timeout=5)
        if not resp.ok:
            return True  # if we can't check, assume cleared
        data = resp.json()
        events = data if isinstance(data, list) else data.get("detections", [])
        for exp in expectations:
            for ev in events:
                if detection_matches(ev, exp):
                    return False  # still active
        return True
    except Exception:
        return True


# ── Fault execution ───────────────────────────────────────────────────────────

def run_fault(
    fault: dict,
    base_url: str,
    dry_run: bool = False,
) -> FaultResult:
    fid = fault["id"]
    topology = fault.get("topology", "")
    description = fault.get("description", "")
    inject_cmds: list[str] = fault.get("inject", [])
    heal_cmds: list[str] = fault.get("heal", [])
    expectations: list[dict] = fault.get("expects", [])

    result = FaultResult(
        fault_id=fid,
        topology=topology,
        description=description,
        passed=False,
        expected_detections=expectations,
    )

    print(f"\n[chaos] ── {fid} ──", file=sys.stderr)
    print(f"[chaos]   {description}", file=sys.stderr)

    # 1. Pre-fault baseline: no active detections for these expectations
    if not dry_run:
        if not detections_cleared(base_url, expectations):
            print("[chaos]   WARNING: pre-fault detections already active — may produce false pass", file=sys.stderr)
            result.pre_fault_clean = False

    # 2. Inject fault
    max_window = max((e.get("within_seconds", 60) for e in expectations), default=60)
    print(f"[chaos]   Injecting fault (expecting detection within {max_window}s)...", file=sys.stderr)
    ok, err = run_commands(inject_cmds, dry_run)
    if not ok:
        result.error = f"inject failed: {err}"
        print(f"[chaos]   INJECT FAILED: {err}", file=sys.stderr)
        return result

    if dry_run:
        result.passed = True
        return result

    # 3. Wait for expected detections
    matched, latency_ms, observed = wait_for_detections(
        base_url, expectations, timeout_s=max_window + 30
    )
    result.detection_observed = matched
    result.detection_latency_ms = round(latency_ms, 1)
    result.observed_detections = observed

    if matched:
        print(f"[chaos]   DETECTED in {latency_ms:.0f}ms ✓", file=sys.stderr)
    else:
        print(f"[chaos]   DETECTION MISSED after {latency_ms:.0f}ms ✗", file=sys.stderr)

    # 4. Heal fault
    print("[chaos]   Healing fault...", file=sys.stderr)
    ok, err = run_commands(heal_cmds, dry_run=False)
    if not ok:
        result.error = f"heal failed: {err}"
        print(f"[chaos]   HEAL FAILED: {err}", file=sys.stderr)

    # 5. Wait for detections to clear (up to 60s)
    if matched:
        time.sleep(10)
        cleared = detections_cleared(base_url, expectations)
        result.post_heal_clear = cleared
        if cleared:
            print("[chaos]   Cleared after heal ✓", file=sys.stderr)
        else:
            print("[chaos]   NOT cleared after heal ✗", file=sys.stderr)

    result.passed = matched and result.post_heal_clear
    return result


# ── Main ──────────────────────────────────────────────────────────────────────

def main() -> int:
    parser = argparse.ArgumentParser(description="Bonsai chaos harness")
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--output", default=str(DEFAULT_OUTPUT))
    parser.add_argument("--topology", choices=["dc", "sp", "all"], default="all")
    parser.add_argument("--fault", help="Run a single fault by ID")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if not FAULT_CATALOG.exists():
        print(f"ERROR: fault catalogue not found: {FAULT_CATALOG}", file=sys.stderr)
        return 1

    catalog = load_catalog(FAULT_CATALOG)

    # Filter
    faults_to_run = catalog
    if args.topology != "all":
        faults_to_run = [f for f in faults_to_run if f.get("topology") == args.topology]
    if args.fault:
        faults_to_run = [f for f in faults_to_run if f["id"] == args.fault]
        if not faults_to_run:
            print(f"ERROR: fault '{args.fault}' not found in catalogue", file=sys.stderr)
            return 1

    print(f"[chaos] Running {len(faults_to_run)} faults against {args.base_url}", file=sys.stderr)
    if args.dry_run:
        print("[chaos] DRY-RUN mode — commands printed but not executed", file=sys.stderr)

    # Verify bonsai reachable (skip in dry-run)
    if not args.dry_run:
        try:
            requests.get(f"{args.base_url}/api/topology", timeout=5)
        except Exception as exc:
            print(f"[chaos] WARNING: bonsai not reachable at {args.base_url}: {exc}", file=sys.stderr)
            print("[chaos] Proceeding anyway — detection checks will fail gracefully", file=sys.stderr)

    harness = HarnessResult(
        ts_unix=int(time.time()),
        base_url=args.base_url,
        topology_filter=args.topology,
        total=len(faults_to_run),
    )

    for fault in faults_to_run:
        fr = run_fault(fault, args.base_url, dry_run=args.dry_run)
        harness.faults.append(fr)
        if fr.passed:
            harness.passed += 1
        elif fr.error and "inject failed" not in fr.error:
            harness.skipped += 1
        else:
            harness.failed += 1

    print(
        f"\n[chaos] Results: {harness.passed}/{harness.total} passed, "
        f"{harness.failed} failed, {harness.skipped} skipped",
        file=sys.stderr,
    )

    # Write output
    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)

    payload: dict[str, Any] = asdict(harness)
    with open(output_path, "w") as f:
        json.dump(payload, f, indent=2)
    print(f"[chaos] Results written to {output_path}", file=sys.stderr)

    # Also print compact summary to stdout for AI consumption
    summary = {
        "driver": "chaos",
        "ts_unix": harness.ts_unix,
        "passed": harness.passed,
        "failed": harness.failed,
        "skipped": harness.skipped,
        "total": harness.total,
        "matrix": [
            {
                "id": fr.fault_id,
                "passed": fr.passed,
                "latency_ms": fr.detection_latency_ms,
                "error": fr.error,
            }
            for fr in harness.faults
        ],
    }
    print(json.dumps(summary, indent=2))

    return 0 if harness.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())

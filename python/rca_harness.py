#!/usr/bin/env python3
"""D4-8 T6: Fault injection RCA test harness.

Injects a fault via inject_fault.py, waits for Bonsai to fire a detection and
complete an investigation, then compares the root_cause_type in the investigation
summary against an expected value.

Tracks rca_accuracy_by_scenario results in a local JSONL file for trend analysis.

Usage:
    python python/rca_harness.py run --scenario bgp_neighbor_down \\
        --hostname srl-spine1 --peer 10.0.12.1 \\
        --expected-rca bgp_neighbor_down --bonsai-url http://localhost:8080

    python python/rca_harness.py matrix   # run all scenarios in the test matrix
    python python/rca_harness.py results  # print accuracy summary from JSONL log

Test matrix (hard-coded):
    interface_down       iface-down    srl-spine1  ethernet-1/1  → interface_down
    bgp_neighbor_down    bgp-flap      srl-spine1  10.0.12.1     → bgp_neighbor_down
    packet_loss_30       netem-loss    srl-spine1  e1-1 30       → degraded_path
    config_caused        bgp-down+...  srl-spine1  10.0.12.1     → config_caused_bgp_down
    redundancy_degraded  iface-down    srl-spine1  ethernet-1/2  → redundancy_degraded
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import requests

RESULTS_LOG = Path("runtime/rca_harness_results.jsonl")
BONSAI_DEFAULT_URL = "http://localhost:8080"
POLL_INTERVAL_SEC = 5
MAX_WAIT_DETECTION_SEC = 120
MAX_WAIT_INVESTIGATION_SEC = 300

TEST_MATRIX: list[dict[str, Any]] = [
    {
        "scenario": "interface_down",
        "inject_cmd": ["iface-down", "srl-spine1", "ethernet-1/1"],
        "restore_cmd": ["iface-up", "srl-spine1", "ethernet-1/1"],
        "expected_rca": "interface_down",
        "device_hint": "srl-spine1",
    },
    {
        "scenario": "bgp_neighbor_down",
        "inject_cmd": ["bgp-flap", "srl-spine1", "10.0.12.1", "--hold", "20"],
        "restore_cmd": None,
        "expected_rca": "bgp_neighbor_down",
        "device_hint": "srl-spine1",
    },
    {
        "scenario": "packet_loss_30",
        "inject_cmd": ["netem-loss", "srl-spine1", "e1-1", "30"],
        "restore_cmd": ["netem-clear", "srl-spine1", "e1-1"],
        "expected_rca": "degraded_path",
        "device_hint": "srl-spine1",
    },
    {
        "scenario": "config_caused_bgp_down",
        "inject_cmd": ["bgp-down", "srl-spine1", "10.0.12.1"],
        "restore_cmd": ["bgp-up", "srl-spine1", "10.0.12.1"],
        "expected_rca": "config_caused_bgp_down",
        "device_hint": "srl-spine1",
    },
    {
        "scenario": "redundancy_degraded",
        "inject_cmd": ["iface-down", "srl-spine1", "ethernet-1/2"],
        "restore_cmd": ["iface-up", "srl-spine1", "ethernet-1/2"],
        "expected_rca": "redundancy_degraded",
        "device_hint": "srl-spine1",
    },
]


def _inject(args_list: list[str], config_path: str, topology: str) -> None:
    script = Path(__file__).parent / "inject_fault.py"
    cmd = [sys.executable, str(script), "--config", config_path, "--topology", topology] + args_list
    print(f"  [inject] {' '.join(cmd)}")
    subprocess.run(cmd, check=True)


def _get_json(url: str, path: str) -> Any:
    resp = requests.get(f"{url}{path}", timeout=10)
    resp.raise_for_status()
    return resp.json()


def _wait_for_detection(bonsai_url: str, device_hint: str, since_ns: int) -> dict | None:
    """Poll /api/detections until a new event appears for device_hint after since_ns."""
    deadline = time.time() + MAX_WAIT_DETECTION_SEC
    print(f"  [wait] polling for detection on {device_hint} (up to {MAX_WAIT_DETECTION_SEC}s)...")
    while time.time() < deadline:
        try:
            events = _get_json(bonsai_url, "/api/detections")
            candidates = [
                e for e in events
                if device_hint.lower() in e.get("device_address", "").lower()
                or device_hint.lower() in e.get("hostname", "").lower()
                and e.get("fired_at", 0) >= since_ns
            ]
            if candidates:
                latest = max(candidates, key=lambda e: e.get("fired_at", 0))
                print(f"  [detection] rule={latest.get('rule_id')} severity={latest.get('severity')} id={latest.get('id')}")
                return latest
        except Exception as exc:
            print(f"  [warn] detection poll error: {exc}")
        time.sleep(POLL_INTERVAL_SEC)
    print(f"  [timeout] no detection found for {device_hint} within {MAX_WAIT_DETECTION_SEC}s")
    return None


def _wait_for_investigation(bonsai_url: str, detection_id: str) -> dict | None:
    """Poll /api/investigations until one linked to detection_id is complete."""
    deadline = time.time() + MAX_WAIT_INVESTIGATION_SEC
    print(f"  [wait] waiting for investigation on detection {detection_id} (up to {MAX_WAIT_INVESTIGATION_SEC}s)...")
    while time.time() < deadline:
        try:
            invs = _get_json(bonsai_url, "/api/investigations")
            for inv in invs:
                if inv.get("detection_id") == detection_id and inv.get("status") == "complete":
                    print(f"  [investigation] id={inv.get('id')} status=complete cost=${inv.get('cost_usd', 0):.4f}")
                    return inv
        except Exception as exc:
            print(f"  [warn] investigation poll error: {exc}")
        time.sleep(POLL_INTERVAL_SEC)
    print(f"  [timeout] no completed investigation for detection {detection_id}")
    return None


def _extract_rca(investigation: dict) -> str:
    """Pull root_cause_type from proposal_json or summary text."""
    proposal_json = investigation.get("proposal_json") or "{}"
    if isinstance(proposal_json, str):
        try:
            proposal = json.loads(proposal_json)
        except json.JSONDecodeError:
            proposal = {}
    else:
        proposal = proposal_json
    rca = proposal.get("root_cause_type") or proposal.get("rca_type") or ""
    if not rca:
        summary = investigation.get("summary", "")
        for keyword in ["interface_down", "bgp_neighbor_down", "config_caused_bgp_down",
                        "degraded_path", "redundancy_degraded", "thermal_sensor_critical"]:
            if keyword in summary.lower():
                rca = keyword
                break
    return rca or "unknown"


def run_scenario(
    scenario: str,
    inject_cmd: list[str],
    restore_cmd: list[str] | None,
    expected_rca: str,
    device_hint: str,
    bonsai_url: str,
    config_path: str,
    topology: str,
    dry_run: bool = False,
) -> dict:
    result: dict[str, Any] = {
        "scenario": scenario,
        "expected_rca": expected_rca,
        "actual_rca": "not_run",
        "passed": False,
        "detection_id": None,
        "investigation_id": None,
        "cost_usd": 0.0,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "error": None,
    }

    print(f"\n{'='*60}")
    print(f"  SCENARIO: {scenario}")
    print(f"  expected_rca: {expected_rca}")
    print(f"{'='*60}")

    if dry_run:
        print("  [dry-run] skipping injection")
        result["actual_rca"] = "dry_run"
        return result

    since_ns = int(time.time() * 1e9)

    try:
        _inject(inject_cmd, config_path, topology)
    except subprocess.CalledProcessError as exc:
        result["error"] = f"injection failed: {exc}"
        print(f"  [error] {result['error']}")
        return result

    detection = _wait_for_detection(bonsai_url, device_hint, since_ns)
    if detection is None:
        result["error"] = "detection timeout"
        if restore_cmd:
            try:
                _inject(restore_cmd, config_path, topology)
            except Exception:
                pass
        return result

    result["detection_id"] = detection.get("id")

    investigation = _wait_for_investigation(bonsai_url, detection["id"])

    if restore_cmd:
        try:
            _inject(restore_cmd, config_path, topology)
        except Exception as exc:
            print(f"  [warn] restore failed: {exc}")

    if investigation is None:
        result["error"] = "investigation timeout"
        return result

    result["investigation_id"] = investigation.get("id")
    result["cost_usd"] = investigation.get("cost_usd", 0.0)
    actual_rca = _extract_rca(investigation)
    result["actual_rca"] = actual_rca
    result["passed"] = actual_rca == expected_rca

    status_icon = "✅" if result["passed"] else "❌"
    print(f"  {status_icon} actual_rca={actual_rca!r}  expected={expected_rca!r}  passed={result['passed']}")
    return result


def save_result(result: dict) -> None:
    RESULTS_LOG.parent.mkdir(parents=True, exist_ok=True)
    with RESULTS_LOG.open("a") as f:
        f.write(json.dumps(result) + "\n")


def print_summary(results: list[dict]) -> None:
    if not results:
        print("No results.")
        return
    passed = sum(1 for r in results if r["passed"])
    total = len(results)
    print(f"\n{'='*60}")
    print(f"  RCA ACCURACY: {passed}/{total} ({100*passed//total}%)")
    print(f"{'='*60}")
    for r in results:
        icon = "✅" if r["passed"] else "❌"
        print(f"  {icon}  {r['scenario']:30s}  expected={r['expected_rca']:30s}  actual={r['actual_rca']}")
    total_cost = sum(r.get("cost_usd", 0.0) for r in results)
    print(f"\n  Total investigation cost: ${total_cost:.4f}")


def cmd_run(args: argparse.Namespace) -> None:
    result = run_scenario(
        scenario=args.scenario,
        inject_cmd=args.inject_cmd,
        restore_cmd=args.restore_cmd or [],
        expected_rca=args.expected_rca,
        device_hint=args.device_hint,
        bonsai_url=args.bonsai_url,
        config_path=args.config,
        topology=args.topology,
        dry_run=args.dry_run,
    )
    save_result(result)
    print_summary([result])
    sys.exit(0 if result["passed"] else 1)


def cmd_matrix(args: argparse.Namespace) -> None:
    results = []
    for entry in TEST_MATRIX:
        if args.filter and args.filter not in entry["scenario"]:
            continue
        result = run_scenario(
            scenario=entry["scenario"],
            inject_cmd=entry["inject_cmd"],
            restore_cmd=entry.get("restore_cmd") or [],
            expected_rca=entry["expected_rca"],
            device_hint=entry["device_hint"],
            bonsai_url=args.bonsai_url,
            config_path=args.config,
            topology=args.topology,
            dry_run=args.dry_run,
        )
        save_result(result)
        results.append(result)
        if not args.no_pause:
            print(f"  [pause] waiting 30s before next scenario...")
            time.sleep(30)
    print_summary(results)
    failed = [r for r in results if not r["passed"]]
    sys.exit(0 if not failed else 1)


def cmd_results(args: argparse.Namespace) -> None:
    if not RESULTS_LOG.exists():
        print(f"No results log at {RESULTS_LOG}")
        sys.exit(0)
    results = []
    with RESULTS_LOG.open() as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    results.append(json.loads(line))
                except json.JSONDecodeError:
                    pass

    if args.scenario:
        results = [r for r in results if r.get("scenario") == args.scenario]

    if args.last:
        results = results[-args.last:]

    by_scenario: dict[str, list[dict]] = {}
    for r in results:
        by_scenario.setdefault(r["scenario"], []).append(r)

    print(f"\nRCA accuracy trend ({RESULTS_LOG}):")
    for scenario, runs in sorted(by_scenario.items()):
        passed = sum(1 for r in runs if r["passed"])
        pct = 100 * passed // len(runs)
        trend = "".join("✅" if r["passed"] else "❌" for r in runs[-10:])
        print(f"  {scenario:35s}  {passed:3d}/{len(runs):3d} ({pct:3d}%)  {trend}")


def main() -> None:
    ap = argparse.ArgumentParser(
        description="D4-8 T6: Fault injection RCA test harness for Bonsai"
    )
    ap.add_argument("--config", default="bonsai.toml", help="Path to bonsai.toml")
    ap.add_argument("--topology", default="bonsai-phase4", help="ContainerLab topology name")
    ap.add_argument("--bonsai-url", default=BONSAI_DEFAULT_URL)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("run", help="Run a single scenario")
    p.add_argument("--scenario", required=True)
    p.add_argument("--inject-cmd", nargs="+", required=True,
                   help="inject_fault.py sub-command + args (e.g. bgp-down srl-spine1 10.0.12.1)")
    p.add_argument("--restore-cmd", nargs="+", default=[])
    p.add_argument("--expected-rca", required=True)
    p.add_argument("--device-hint", default="srl-spine1")
    p.add_argument("--dry-run", action="store_true")

    p = sub.add_parser("matrix", help="Run full test matrix")
    p.add_argument("--filter", default="", help="Only run scenarios containing this substring")
    p.add_argument("--no-pause", action="store_true", help="Skip 30s pause between scenarios")
    p.add_argument("--dry-run", action="store_true")

    p = sub.add_parser("results", help="Print accuracy trend from results log")
    p.add_argument("--scenario", default="")
    p.add_argument("--last", type=int, default=0, help="Show only last N runs per scenario")

    args = ap.parse_args()
    if args.cmd == "run":
        cmd_run(args)
    elif args.cmd == "matrix":
        cmd_matrix(args)
    elif args.cmd == "results":
        cmd_results(args)


if __name__ == "__main__":
    main()

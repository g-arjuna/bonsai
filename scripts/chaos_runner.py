#!/usr/bin/env python3
"""Automated chaos runner for sustained fault injection and training data accumulation.

Reads a YAML fault plan, runs for a configured duration, injects faults at random
intervals, heals them, and writes a ground-truth CSV for detection evaluation.

Usage:
    python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml
    python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --dry-run
    python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --duration-hours 2
"""
from __future__ import annotations

import argparse
import csv
import logging
import os
import random
import signal
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml

# Add project root so inject_fault is importable
sys.path.insert(0, str(Path(__file__).parents[1] / "python"))
import inject_fault

LOG_FORMAT = "%(asctime)s [%(levelname)s] %(message)s"
logging.basicConfig(level=logging.INFO, format=LOG_FORMAT)
log = logging.getLogger("chaos_runner")


# ── Plan loading ──────────────────────────────────────────────────────────────

def load_plan(path: str) -> dict:
    with open(path) as f:
        plan = yaml.safe_load(f)
    _validate_plan(plan)
    return plan


def _validate_plan(plan: dict) -> None:
    required = ("faults",)
    for key in required:
        if key not in plan:
            raise ValueError(f"Plan missing required key: '{key}'")
    for fault in plan["faults"]:
        if "type" not in fault:
            raise ValueError(f"Fault entry missing 'type': {fault}")
        if fault["weight"] <= 0:
            raise ValueError(f"Fault weight must be > 0: {fault}")


# ── Weighted random selection ─────────────────────────────────────────────────

def weighted_choice(faults: list[dict]) -> dict:
    weights = [f.get("weight", 1) for f in faults]
    return random.choices(faults, weights=weights, k=1)[0]


def random_from_range(value: int | list) -> int | float:
    """Return a random value from [min, max] if value is a 2-element list, else value itself."""
    if isinstance(value, list) and len(value) == 2:
        lo, hi = value
        return random.uniform(lo, hi) if isinstance(lo, float) or isinstance(hi, float) \
               else random.randint(int(lo), int(hi))
    return value


# ── Injection dispatch ────────────────────────────────────────────────────────

def inject(fault: dict, targets: dict, topology: str, dry_run: bool) -> dict | None:
    """Inject one fault. Returns an injection record dict or None on error."""
    fault_type = fault["type"]
    now_ns = time.time_ns()

    try:
        if fault_type == "bgp_session_down":
            hostname = random.choice(fault["targets"])
            peer = random.choice(fault["peer_addresses"])
            log.info("[INJECT] bgp_session_down  host=%s  peer=%s", hostname, peer)
            if not dry_run:
                inject_fault.dispatch_bgp_down(targets, hostname, peer)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": peer,
                "injected_at_ns": now_ns,
                "healed_at_ns": None,
            }

        elif fault_type == "interface_shut":
            hostname = random.choice(fault["targets"])
            iface = random.choice(fault["interfaces"])
            log.info("[INJECT] interface_shut  host=%s  iface=%s", hostname, iface)
            if not dry_run:
                inject_fault.dispatch_iface_down(targets, hostname, iface)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": iface,
                "injected_at_ns": now_ns,
                "healed_at_ns": None,
            }

        elif fault_type == "netem_loss":
            hostname = random.choice(fault["targets"])
            iface = random.choice(fault["interfaces"])
            loss = random_from_range(fault["loss_percent"])
            log.info("[INJECT] netem_loss  host=%s  iface=%s  loss=%.1f%%", hostname, iface, loss)
            if not dry_run:
                inject_fault.netem_loss(hostname, iface, loss, topology)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": f"{iface}:loss={loss:.1f}%",
                "injected_at_ns": now_ns,
                "healed_at_ns": None,
            }

        else:
            log.warning("Unknown fault type: %s — skipping", fault_type)
            return None

    except Exception as exc:
        log.error("[INJECT ERROR] %s: %s", fault_type, exc)
        return None


def heal(record: dict, fault: dict, targets: dict, topology: str, dry_run: bool) -> None:
    """Heal a previously injected fault. Updates record in-place."""
    fault_type = record["fault_type"]
    hostname = record["hostname"]
    param = record["param"]

    try:
        if fault_type == "bgp_session_down":
            log.info("[HEAL] bgp_session_up  host=%s  peer=%s", hostname, param)
            if not dry_run:
                inject_fault.dispatch_bgp_up(targets, hostname, param)

        elif fault_type == "interface_shut":
            log.info("[HEAL] interface_up  host=%s  iface=%s", hostname, param)
            if not dry_run:
                inject_fault.dispatch_iface_up(targets, hostname, param)

        elif fault_type == "netem_loss":
            iface = param.split(":")[0]
            log.info("[HEAL] netem_clear  host=%s  iface=%s", hostname, iface)
            if not dry_run:
                inject_fault.netem_clear(hostname, iface, topology)

    except Exception as exc:
        log.error("[HEAL ERROR] %s: %s", fault_type, exc)

    record["healed_at_ns"] = time.time_ns()


# ── CSV output ────────────────────────────────────────────────────────────────

CSV_FIELDS = [
    "fault_type", "hostname", "param",
    "injected_at_ns", "healed_at_ns",
    "injected_at_iso", "healed_at_iso",
]


def _ns_to_iso(ns: int | None) -> str:
    if ns is None:
        return ""
    return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).isoformat()


def write_csv(records: list[dict], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=CSV_FIELDS)
        writer.writeheader()
        for r in records:
            writer.writerow({
                **r,
                "injected_at_iso": _ns_to_iso(r.get("injected_at_ns")),
                "healed_at_iso":   _ns_to_iso(r.get("healed_at_ns")),
            })
    log.info("Ground-truth CSV written: %s  (%d rows)", path, len(records))


# ── Main loop ─────────────────────────────────────────────────────────────────

def run(plan: dict, args: argparse.Namespace) -> None:
    duration_h = args.duration_hours or plan.get("duration_hours", 1)
    duration_s = duration_h * 3600
    interval_range = plan.get("injection_interval_seconds", [60, 300])
    topology = plan.get("topology", inject_fault.TOPOLOGY_NAME)
    faults = plan["faults"]

    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = Path("chaos_runs") / run_id
    csv_path = out_dir / "injections.csv"

    config_path = args.config or inject_fault.CONFIG_PATH
    targets = inject_fault._load_targets(config_path)

    log.info("Chaos run %s starting — duration=%.1fh  targets=%s  dry_run=%s",
             run_id, duration_h, list(targets), args.dry_run)

    records: list[dict] = []
    deadline = time.monotonic() + duration_s
    _stop = False

    def _sigint(sig, frame):
        nonlocal _stop
        log.info("Interrupted — finishing current cycle then exiting")
        _stop = True
    signal.signal(signal.SIGINT, _sigint)

    while not _stop and time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break

        fault_def = weighted_choice(faults)
        record = inject(fault_def, targets, topology, args.dry_run)

        if record:
            records.append(record)
            heal_delay = random_from_range(fault_def.get("healing_delay_seconds", [20, 60]))
            log.info("  holding fault for %.0fs", heal_delay)
            time.sleep(min(heal_delay, remaining))
            heal(record, fault_def, targets, topology, args.dry_run)
            # Flush CSV after each completed injection so a Ctrl-C mid-run still has data.
            write_csv(records, csv_path)

        if _stop:
            break

        interval = random_from_range(interval_range)
        remaining = deadline - time.monotonic()
        wait = min(interval, remaining)
        if wait > 0:
            log.info("  next injection in %.0fs  (%.0f min remaining)",
                     wait, remaining / 60)
            time.sleep(wait)

    write_csv(records, csv_path)
    log.info("Chaos run complete.  %d injections.  CSV: %s", len(records), csv_path)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    ap = argparse.ArgumentParser(description="Bonsai chaos runner — sustained fault injection")
    ap.add_argument("plan", help="Path to YAML fault plan (e.g. chaos_plans/baseline_mix.yaml)")
    ap.add_argument("--config", default=None,
                    help="Path to bonsai.toml (default: bonsai.toml)")
    ap.add_argument("--duration-hours", type=float, default=None,
                    help="Override plan duration_hours")
    ap.add_argument("--dry-run", action="store_true",
                    help="Print what would be injected without actually doing it")
    args = ap.parse_args()

    plan = load_plan(args.plan)
    run(plan, args)


if __name__ == "__main__":
    main()

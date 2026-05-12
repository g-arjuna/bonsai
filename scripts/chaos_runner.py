#!/usr/bin/env python3
"""Automated chaos runner for sustained fault injection and training data accumulation.

Reads a YAML fault plan, runs for a configured duration, injects faults at random
intervals, heals them, and writes a ground-truth CSV for detection evaluation.

Usage:
    # Preferred: run inside WSL from the repo-local .venv so clab/netem are available
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
import shutil
import signal
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import json
import threading
import urllib.error
import urllib.request

import yaml

# Add project root so inject_fault is importable
sys.path.insert(0, str(Path(__file__).parents[1] / "python"))
import inject_fault

LOG_FORMAT = "%(asctime)s [%(levelname)s] %(message)s"
logging.basicConfig(level=logging.INFO, format=LOG_FORMAT)
log = logging.getLogger("chaos_runner")

# Snapshot offsets (seconds after injection): sync taken during heal sleep, async via Timer
SNAPSHOT_OFFSETS_SYNC = [10, 30, 60]
SNAPSHOT_OFFSETS_ASYNC = [300, 1800]

# ISO weekday numbers matching Python's datetime.weekday() (Mon=0 … Sun=6)
_DOW_MAP = {"MON": 0, "TUE": 1, "WED": 2, "THU": 3, "FRI": 4, "SAT": 5, "SUN": 6}


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

def _fault_support_reason(fault_type: str) -> str | None:
    """Return a reason when a fault type cannot run on the current host."""
    if fault_type == "gradual_degradation":
        return "fault type is not implemented yet"

    if fault_type in {"netem_loss", "netem_delay"} and shutil.which("clab") is None:
        return "`clab` is not available on PATH"

    return None


def filter_supported_faults(faults: list[dict]) -> tuple[list[dict], list[tuple[str, str]]]:
    """Split plan faults into runnable and skipped sets for the current environment."""
    supported: list[dict] = []
    skipped: list[tuple[str, str]] = []

    for fault in faults:
        fault_type = fault["type"]
        reason = _fault_support_reason(fault_type)
        if reason is None:
            supported.append(fault)
        else:
            skipped.append((fault_type, reason))

    return supported, skipped


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
                inject_fault.dispatch_bgp_down(targets, hostname, peer, topology)
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
                inject_fault.dispatch_iface_down(targets, hostname, iface, topology)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": iface,
                "injected_at_ns": now_ns,
                "healed_at_ns": None,
            }

        elif fault_type == "bfd_session_down":
            hostname = random.choice(fault["targets"])
            subinterface = random.choice(fault["subinterfaces"])
            scenario = fault.get("scenario", "admin_disable")
            log.info(
                "[INJECT] bfd_session_down  host=%s  subinterface=%s  scenario=%s",
                hostname,
                subinterface,
                scenario,
            )
            if not dry_run:
                inject_fault.dispatch_bfd_down(targets, hostname, subinterface, topology)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": f"{subinterface}:scenario={scenario}",
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

        elif fault_type == "route_flap":
            hostname = random.choice(fault["targets"])
            peer = random.choice(fault["peer_addresses"])
            flap_count = int(fault.get("flap_count", 3))
            flap_hold = int(random_from_range(fault.get("flap_hold_seconds", [3, 8])))
            log.info(
                "[INJECT] route_flap  host=%s  peer=%s  flap_count=%s  hold=%ss",
                hostname,
                peer,
                flap_count,
                flap_hold,
            )
            if not dry_run:
                for _ in range(flap_count):
                    inject_fault.dispatch_bgp_down(targets, hostname, peer, topology)
                    time.sleep(flap_hold)
                    inject_fault.dispatch_bgp_up(targets, hostname, peer, topology)
                    time.sleep(1)
            healed_at_ns = time.time_ns()
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": f"{peer}:flap_count={flap_count}:hold={flap_hold}",
                "injected_at_ns": now_ns,
                "healed_at_ns": healed_at_ns,
            }

        elif fault_type == "sr_policy_degrade":
            hostname = random.choice(fault["targets"])
            policy_name = random.choice(fault["policy_names"])
            log.info("[INJECT] sr_policy_degrade  host=%s  policy=%s", hostname, policy_name)
            if not dry_run:
                inject_fault.dispatch_sr_policy_down(targets, hostname, policy_name, topology)
            return {
                "fault_type": fault_type,
                "hostname": hostname,
                "param": policy_name,
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

    if record.get("healed_at_ns") is not None:
        return

    try:
        if fault_type == "bgp_session_down":
            log.info("[HEAL] bgp_session_up  host=%s  peer=%s", hostname, param)
            if not dry_run:
                inject_fault.dispatch_bgp_up(targets, hostname, param, topology)

        elif fault_type == "interface_shut":
            log.info("[HEAL] interface_up  host=%s  iface=%s", hostname, param)
            if not dry_run:
                inject_fault.dispatch_iface_up(targets, hostname, param, topology)

        elif fault_type == "bfd_session_down":
            subinterface = param.split(":")[0]
            log.info("[HEAL] bfd_session_up  host=%s  subinterface=%s", hostname, subinterface)
            if not dry_run:
                inject_fault.dispatch_bfd_up(targets, hostname, subinterface, topology)

        elif fault_type == "netem_loss":
            iface = param.split(":")[0]
            log.info("[HEAL] netem_clear  host=%s  iface=%s", hostname, iface)
            if not dry_run:
                inject_fault.netem_clear(hostname, iface, topology)

        elif fault_type == "sr_policy_degrade":
            log.info("[HEAL] sr_policy_restore  host=%s  policy=%s", hostname, param)
            if not dry_run:
                inject_fault.dispatch_sr_policy_up(targets, hostname, param, topology)

    except Exception as exc:
        log.error("[HEAL ERROR] %s: %s", fault_type, exc)

    record["healed_at_ns"] = time.time_ns()


# ── CSV output ────────────────────────────────────────────────────────────────

CSV_FIELDS = [
    "fault_type", "hostname", "param",
    "injected_at_ns", "healed_at_ns",
    "injected_at_iso", "healed_at_iso",
    "adversarial", "should_not_detect",
]


def _ns_to_iso(ns: int | None) -> str:
    if ns is None:
        return ""
    return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).isoformat()


def write_csv(records: list[dict], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=CSV_FIELDS, extrasaction="ignore")
        writer.writeheader()
        for r in records:
            writer.writerow({
                "adversarial": "",
                "should_not_detect": "",
                **r,
                "injected_at_iso": _ns_to_iso(r.get("injected_at_ns")),
                "healed_at_iso":   _ns_to_iso(r.get("healed_at_ns")),
            })
    log.info("Ground-truth CSV written: %s  (%d rows)", path, len(records))


# ── Fault propagation snapshots (T2-1) ────────────────────────────────────────

def _take_snapshot(
    base_url: str,
    snap_dir: Path,
    injection_index: int,
    offset_label: str,
    inject_ns: int,
    fault_type: str,
    hostname: str,
    adversarial: bool = False,
) -> None:
    """Fetch topology+detections from Bonsai API and write one snapshot JSON file."""
    snap_dir.mkdir(parents=True, exist_ok=True)
    payload: dict = {
        "injection_index": injection_index,
        "offset_label": offset_label,
        "captured_at_ns": time.time_ns(),
        "inject_ns": inject_ns,
        "fault_type": fault_type,
        "hostname": hostname,
        "adversarial": adversarial,
    }
    for endpoint, key in [("/api/topology", "topology"), ("/api/detections", "detections")]:
        try:
            with urllib.request.urlopen(f"{base_url}{endpoint}", timeout=5) as resp:
                payload[key] = json.loads(resp.read().decode())
        except Exception as exc:
            payload[key] = None
            payload[f"{key}_error"] = str(exc)

    fname = snap_dir / f"{injection_index:04d}_{offset_label}.json"
    fname.write_text(json.dumps(payload, default=str))
    log.debug("Snapshot written: %s", fname)


def _heal_with_snapshots(
    heal_delay: float,
    remaining: float,
    inject_ns: int,
    injection_index: int,
    fault_type: str,
    hostname: str,
    snap_dir: Path,
    base_url: str,
    dry_run: bool,
    adversarial: bool = False,
) -> None:
    """Sleep heal_delay, taking graph snapshots at SNAPSHOT_OFFSETS_SYNC during the window."""
    wait = min(heal_delay, remaining)
    elapsed = 0.0
    for offset_s in sorted(SNAPSHOT_OFFSETS_SYNC):
        if offset_s >= wait:
            break
        step = offset_s - elapsed
        if step > 0:
            time.sleep(step)
            elapsed = float(offset_s)
        if not dry_run:
            _take_snapshot(base_url, snap_dir, injection_index, f"+{offset_s}s",
                           inject_ns, fault_type, hostname, adversarial)
    leftover = wait - elapsed
    if leftover > 0:
        time.sleep(leftover)


def _schedule_async_snapshots(
    inject_ns: int,
    injection_index: int,
    fault_type: str,
    hostname: str,
    snap_dir: Path,
    base_url: str,
    remaining: float,
    adversarial: bool = False,
) -> list[threading.Timer]:
    """Schedule threading.Timers for post-heal snapshots at SNAPSHOT_OFFSETS_ASYNC.

    Timer delays are computed from now so that the label correctly reflects offset from inject_ns.
    """
    timers: list[threading.Timer] = []
    elapsed_s = (time.time_ns() - inject_ns) / 1e9
    for offset_s in SNAPSHOT_OFFSETS_ASYNC:
        delay = offset_s - elapsed_s
        if delay <= 0 or delay > remaining + 60:
            continue
        t = threading.Timer(
            delay,
            _take_snapshot,
            args=(base_url, snap_dir, injection_index, f"+{offset_s}s",
                  inject_ns, fault_type, hostname, adversarial),
        )
        t.daemon = True
        t.start()
        timers.append(t)
    return timers


# ── Protected baseline windows (T2-2) ─────────────────────────────────────────

def is_in_protected_window(baselines: list[dict]) -> str | None:
    """Return the baseline id if current UTC time falls within a protected window, else None.

    Cron entries must be in the form '0 H * * DOW' (minute=0, fixed hour, any dom/month, named dow).
    """
    now = datetime.now(timezone.utc)
    for bl in baselines:
        parts = bl.get("cron", "").split()
        if len(parts) != 5:
            continue
        _, hour_s, _, _, dow_s = parts
        try:
            hour = int(hour_s)
        except ValueError:
            continue
        dow = _DOW_MAP.get(dow_s.upper())
        if dow is None:
            continue
        duration_h = float(bl.get("duration_hours", 1))
        if now.weekday() == dow and hour <= now.hour < hour + int(duration_h):
            return bl.get("id", "unknown")
    return None


# ── Single-injection orchestrator ─────────────────────────────────────────────

def _run_one_injection(
    fault_def: dict,
    targets: dict,
    topology: str,
    deadline: float,
    snap_dir: Path,
    base_url: str,
    injection_index: int,
    records: list[dict],
    csv_path: Path,
    async_timers: list,
    dry_run: bool,
    adversarial: bool = False,
) -> None:
    """Inject one fault, take propagation snapshots, heal, schedule post-heal snapshots."""
    remaining = deadline - time.monotonic()
    inject_ns = time.time_ns()

    if not dry_run:
        _take_snapshot(base_url, snap_dir, injection_index, "pre",
                       inject_ns, fault_def["type"], "", adversarial)

    record = inject(fault_def, targets, topology, dry_run)
    if not record:
        return

    record["adversarial"] = adversarial
    record["should_not_detect"] = fault_def.get("should_not_detect", "") if adversarial else ""
    records.append(record)

    heal_delay = random_from_range(fault_def.get("healing_delay_seconds", [20, 60]))
    log.info("  holding fault for %.0fs  [%s]", heal_delay, "adversarial" if adversarial else "primary")
    _heal_with_snapshots(
        heal_delay, remaining, inject_ns, injection_index,
        fault_def["type"], record["hostname"], snap_dir, base_url, dry_run, adversarial,
    )
    heal(record, fault_def, targets, topology, dry_run)

    if not dry_run:
        remaining = deadline - time.monotonic()
        async_timers.extend(
            _schedule_async_snapshots(inject_ns, injection_index, fault_def["type"],
                                      record["hostname"], snap_dir, base_url, remaining, adversarial)
        )
    write_csv(records, csv_path)


# ── Main loop ─────────────────────────────────────────────────────────────────

def run(plan: dict, args: argparse.Namespace) -> None:
    duration_h = args.duration_hours or plan.get("duration_hours", 1)
    duration_s = duration_h * 3600
    interval_range = plan.get("injection_interval_seconds", [60, 300])
    topology = plan.get("topology", inject_fault.TOPOLOGY_NAME)
    faults, skipped_faults = filter_supported_faults(plan["faults"])
    if not faults:
        raise SystemExit("No runnable faults remain after host capability checks")

    # Adversarial plan — optional parallel stream injected at lower frequency (T2-3)
    adv_faults: list[dict] = []
    adv_freq = 0
    if getattr(args, "adversarial_plan", None):
        adv_plan = load_plan(args.adversarial_plan)
        adv_supported, adv_skipped = filter_supported_faults(adv_plan.get("faults", []))
        adv_faults = adv_supported
        adv_freq = int(adv_plan.get("injection_frequency_divisor", 8))
        for ft, reason in adv_skipped:
            log.warning("Adversarial: skipping %s: %s", ft, reason)

    base_url = (getattr(args, "bonsai_url", None)
                or plan.get("bonsai_base_url", "http://localhost:3000"))
    protected_baselines = plan.get("protected_baselines", [])

    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = Path("chaos_runs") / run_id
    snap_dir = out_dir / "snapshots"
    csv_path = out_dir / "injections.csv"

    config_path = args.config or inject_fault.CONFIG_PATH
    targets = inject_fault._load_targets(config_path)

    log.info(
        "Chaos run %s starting — duration=%.1fh  targets=%s  dry_run=%s  snapshots=%s",
        run_id, duration_h, list(targets), args.dry_run, snap_dir,
    )
    for fault_type, reason in skipped_faults:
        log.warning("Skipping fault type %s: %s", fault_type, reason)

    records: list[dict] = []
    deadline = time.monotonic() + duration_s
    injection_index = 0
    async_timers: list[threading.Timer] = []
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

        # Protected baseline window check (T2-2): skip injection, wait, re-check
        protected_id = is_in_protected_window(protected_baselines)
        if protected_id:
            log.info("Protected baseline window '%s' active — skipping injection", protected_id)
            time.sleep(60)
            continue

        # Adversarial injection every adv_freq primary cycles (T2-3)
        if adv_faults and adv_freq > 0 and injection_index > 0 and injection_index % adv_freq == 0:
            _run_one_injection(
                fault_def=weighted_choice(adv_faults),
                targets=targets, topology=topology, deadline=deadline,
                snap_dir=snap_dir, base_url=base_url, injection_index=injection_index,
                records=records, csv_path=csv_path, async_timers=async_timers,
                dry_run=args.dry_run, adversarial=True,
            )
            injection_index += 1

        # Primary injection (T2-1 snapshots wired inside _run_one_injection)
        _run_one_injection(
            fault_def=weighted_choice(faults),
            targets=targets, topology=topology, deadline=deadline,
            snap_dir=snap_dir, base_url=base_url, injection_index=injection_index,
            records=records, csv_path=csv_path, async_timers=async_timers,
            dry_run=args.dry_run, adversarial=False,
        )
        injection_index += 1

        if _stop:
            break

        interval = random_from_range(interval_range)
        remaining = deadline - time.monotonic()
        wait = min(interval, remaining)
        if wait > 0:
            log.info("  next injection in %.0fs  (%.0f min remaining)", wait, remaining / 60)
            time.sleep(wait)

    write_csv(records, csv_path)
    log.info("Chaos run complete.  %d injections.  CSV: %s", len(records), csv_path)

    live = [t for t in async_timers if t.is_alive()]
    if live:
        log.info("Waiting for %d in-flight snapshot timers…", len(live))
        for t in live:
            t.join(timeout=10)


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
    ap.add_argument("--adversarial-plan", default=None,
                    help="Optional adversarial YAML plan (T2-3); injected every N primary cycles")
    ap.add_argument("--bonsai-url", default="http://localhost:3000",
                    help="Bonsai HTTP base URL for propagation snapshots (T2-1)")
    args = ap.parse_args()

    plan = load_plan(args.plan)
    run(plan, args)


if __name__ == "__main__":
    main()

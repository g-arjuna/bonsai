#!/usr/bin/env python3
"""Automated fault-cycle runner — accumulates ML training data.

Runs repeated inject → wait-for-detection → restore → cool-down cycles.
Each cycle generates: one or more DetectionEvents (anomaly class) +
a Remediation node (auto-heal result). After N cycles the graph has
enough labelled data to train Model A and Model C.

Usage:
    # 20 BGP flap cycles, 15s hold, 30s cool-down between cycles
    python python/soak_test.py --cycles 20 --fault bgp --hold 15 --cooldown 30

    # Interface flap cycles
    python python/soak_test.py --cycles 10 --fault iface --hold 20 --cooldown 45

    # Mixed (alternates BGP and interface faults)
    python python/soak_test.py --cycles 30 --fault mixed --hold 15 --cooldown 30

    # Dry run — shows what would be injected without touching devices
    python python/soak_test.py --cycles 5 --fault bgp --dry-run

After the soak, export training data:
    python python/export_training.py --output data/training.parquet
    python python/export_training.py --mode remediation --output data/remediation.parquet

Minimum for a useful Model A:  ~50 cycles (50 anomaly rows + normal baseline)
Minimum for a useful Model C:  ~50 successful auto-remediations
"""
from __future__ import annotations

import argparse
import itertools
import sys
import time
import tomllib
from pathlib import Path

from bonsai_sdk.client import BonsaiClient

# ── lab topology — BGP peers and interfaces to cycle through ──────────────────
# Each entry: (hostname, peer_or_iface)
# Rotated round-robin so each cycle hits a different node/session.

_BGP_TARGETS = [
    ("srl-spine1", "10.0.12.1"),    # spine → leaf1
    ("srl-spine1", "10.0.13.1"),    # spine → leaf2
    ("srl-spine1", "10.0.14.1"),    # spine → xrd-pe1
    ("srl-leaf1",  "10.0.12.0"),    # leaf1 → spine
    ("srl-leaf2",  "10.0.13.0"),    # leaf2 → spine
]

_IFACE_TARGETS = [
    ("srl-spine1", "ethernet-1/1"),  # spine ↔ leaf1
    ("srl-spine1", "ethernet-1/2"),  # spine ↔ leaf2
    ("srl-leaf1",  "ethernet-1/1"),  # leaf1 ↔ spine
    ("srl-leaf2",  "ethernet-1/1"),  # leaf2 ↔ spine
]


# ── bonsai.toml loader ────────────────────────────────────────────────────────

def _load_targets(cfg_path: str = "bonsai.toml") -> dict[str, dict]:
    path = Path(cfg_path)
    if not path.exists():
        sys.exit(f"ERROR: {cfg_path} not found")
    with open(path, "rb") as f:
        cfg = tomllib.load(f)
    result = {}
    for t in cfg.get("target", []):
        hostname = t.get("hostname", "")
        if hostname:
            addr = t.get("address", "").split(":")[0]
            vendor = t.get("vendor", "nokia_srl")
            if "xrd" in hostname.lower():
                vendor = "cisco_xrd"
            result[hostname] = {
                "address":  addr,
                "username": t.get("username", "admin"),
                "password": t.get("password", ""),
                "vendor":   vendor,
            }
    return result


# ── detection poller ──────────────────────────────────────────────────────────

def _wait_for_detection(client: BonsaiClient, since_ns: int, timeout_s: int) -> list:
    """Poll for new DetectionEvents since `since_ns`. Returns list of rows."""
    deadline = time.time() + timeout_s
    cypher = f"""
        MATCH (e:DetectionEvent)
        WHERE e.fired_at >= {since_ns}
        RETURN e.rule_id, e.severity, e.fired_at
        ORDER BY e.fired_at DESC LIMIT 10
    """
    while time.time() < deadline:
        try:
            rows = client.query(cypher)
            if rows:
                return rows
        except Exception:
            pass
        time.sleep(2)
    return []


# ── fault injection (inline, no subprocess dependency on inject_fault.py) ────

def _inject(targets: dict, hostname: str, fault_type: str, target: str,
             action: str, dry_run: bool) -> None:
    """action: 'down' or 'up'"""
    if dry_run:
        print(f"  [DRY-RUN] would {fault_type}-{action} {hostname} {target}")
        return

    t = targets.get(hostname)
    if not t:
        print(f"  WARNING: {hostname} not in bonsai.toml — skipping")
        return

    # Import here to avoid hard dependency when --dry-run is used without paramiko
    from inject_fault import (
        dispatch_bgp_down, dispatch_bgp_up,
        dispatch_iface_down, dispatch_iface_up,
    )

    if fault_type == "bgp":
        if action == "down":
            dispatch_bgp_down(targets, hostname, target)
        else:
            dispatch_bgp_up(targets, hostname, target)
    elif fault_type == "iface":
        if action == "down":
            dispatch_iface_down(targets, hostname, target)
        else:
            dispatch_iface_up(targets, hostname, target)


# ── cycle runner ──────────────────────────────────────────────────────────────

class CycleStats:
    def __init__(self) -> None:
        self.total          = 0
        self.detected       = 0
        self.missed         = 0
        self.detect_times   : list[float] = []
        self.errors         : list[str]   = []

    def record(self, detected: bool, detect_time_s: float | None, error: str = "") -> None:
        self.total += 1
        if detected:
            self.detected += 1
            if detect_time_s is not None:
                self.detect_times.append(detect_time_s)
        else:
            self.missed += 1
        if error:
            self.errors.append(error)

    def summary(self) -> str:
        avg = (sum(self.detect_times) / len(self.detect_times)
               if self.detect_times else None)
        lines = [
            f"  Cycles run   : {self.total}",
            f"  Detected     : {self.detected}",
            f"  Missed       : {self.missed}",
            f"  Avg detect   : {avg:.1f}s" if avg else "  Avg detect   : n/a",
        ]
        if self.errors:
            lines.append(f"  Errors       : {len(self.errors)}")
        return "\n".join(lines)


def run_cycles(
    client: BonsaiClient,
    targets: dict,
    fault_type: str,
    cycles: int,
    hold_s: int,
    cooldown_s: int,
    detect_timeout_s: int,
    dry_run: bool,
) -> CycleStats:
    stats = CycleStats()

    if fault_type == "bgp":
        target_pool = itertools.cycle(_BGP_TARGETS)
    elif fault_type == "iface":
        target_pool = itertools.cycle(_IFACE_TARGETS)
    else:  # mixed
        target_pool = itertools.cycle(_BGP_TARGETS + _IFACE_TARGETS)

    # Determine fault_type per entry for mixed mode
    bgp_set  = set(map(tuple, _BGP_TARGETS))

    for i in range(1, cycles + 1):
        hostname, target = next(target_pool)
        eff_type = "bgp" if (hostname, target) in bgp_set else "iface"
        label    = f"{'BGP' if eff_type == 'bgp' else 'IFACE'} {hostname} {target}"

        print(f"\n{'-'*60}")
        print(f"Cycle {i}/{cycles} — {label}")

        inject_start = time.time()
        try:
            _inject(targets, hostname, eff_type, target, "down", dry_run)
        except Exception as exc:
            print(f"  ERROR injecting fault: {exc}")
            stats.record(False, None, str(exc))
            continue

        since_ns = int(time.time() * 1e9)
        print(f"  fault injected, waiting up to {detect_timeout_s}s for detection...")

        if dry_run:
            time.sleep(1)
            detected_rows = [["<dry-run>", "warn", since_ns]]
        else:
            detected_rows = _wait_for_detection(client, since_ns, detect_timeout_s)

        detect_time = time.time() - inject_start

        if detected_rows:
            rule_ids = [r[0] for r in detected_rows]
            print(f"  [OK] detected in {detect_time:.1f}s - rules: {rule_ids}")
            stats.record(True, detect_time)
        else:
            print(f"  [MISS] no detection within {detect_timeout_s}s")
            stats.record(False, None)

        print(f"  restoring (hold was {hold_s}s)...")
        elapsed = time.time() - inject_start
        remaining_hold = max(0, hold_s - elapsed)
        if remaining_hold > 0 and not dry_run:
            time.sleep(remaining_hold)

        try:
            _inject(targets, hostname, eff_type, target, "up", dry_run)
        except Exception as exc:
            print(f"  WARNING: restore failed: {exc} — manual restore may be needed")

        if i < cycles:
            print(f"  cooling down {cooldown_s}s before next cycle...")
            if not dry_run:
                time.sleep(cooldown_s)

    return stats


# ── main ──────────────────────────────────────────────────────────────────────

def main() -> None:
    ap = argparse.ArgumentParser(
        description="Soak test: run fault cycles to accumulate ML training data"
    )
    ap.add_argument("--cycles",   type=int, default=20,
                    help="Number of fault/restore cycles (default 20)")
    ap.add_argument("--fault",    choices=["bgp", "iface", "mixed"], default="bgp",
                    help="Fault type to inject (default bgp)")
    ap.add_argument("--hold",     type=int, default=15,
                    help="Seconds to hold fault before restoring (default 15)")
    ap.add_argument("--cooldown", type=int, default=30,
                    help="Seconds between cycles (default 30)")
    ap.add_argument("--detect-timeout", type=int, default=40,
                    help="Seconds to wait for detection before marking missed (default 40)")
    ap.add_argument("--api",      default="[::1]:50051")
    ap.add_argument("--config",   default="bonsai.toml")
    ap.add_argument("--dry-run",  action="store_true",
                    help="Print what would be done without touching devices")
    args = ap.parse_args()

    targets = _load_targets(args.config)

    total_time_min = args.cycles * (args.hold + args.cooldown + args.detect_timeout) / 60
    print(f"Bonsai soak test")
    print(f"  Fault type   : {args.fault}")
    print(f"  Cycles       : {args.cycles}")
    print(f"  Hold         : {args.hold}s")
    print(f"  Cooldown     : {args.cooldown}s")
    print(f"  Est. duration: ~{total_time_min:.0f} min")
    if args.dry_run:
        print(f"  DRY-RUN: no devices will be touched")
    print(f"\nTargets from {args.config}:")
    for h, t in targets.items():
        print(f"  {h:20s} {t['address']:15s} [{t['vendor']}]")
    print()

    with BonsaiClient(args.api) as client:
        stats = run_cycles(
            client     = client,
            targets    = targets,
            fault_type = args.fault,
            cycles     = args.cycles,
            hold_s     = args.hold,
            cooldown_s = args.cooldown,
            detect_timeout_s = args.detect_timeout,
            dry_run    = args.dry_run,
        )

    print(f"\n{'='*60}")
    print("Soak test complete")
    print(stats.summary())
    print()
    print("Next steps:")
    print("  python python/export_training.py --output data/training.parquet")
    print("  python python/export_training.py --mode remediation --output data/remediation.parquet")
    print("  python python/train_anomaly.py --input data/training.parquet --eval")


if __name__ == "__main__":
    main()

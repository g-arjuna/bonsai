#!/usr/bin/env python3
"""Compute per-rule detection baselines from chaos ground-truth + bonsai detection events.

Reads:
  - chaos_runs/*/injections.csv   — ground-truth fault injection records
  - GET /api/detections            — bonsai DetectionEvent+Remediation pairs

Outputs:
  - docs/test_results/detection_baselines/<YYYYMMDD>.md

Matching logic:
  A detection is a True Positive for fault F if:
    - detection.target matches fault.hostname
    - detection.timestamp_ns is within [injected_at_ns, healed_at_ns + GRACE_NS]
  A fault with no matching detection is a False Negative.
  A detection in a quiescent window (no active fault on that target) is a False Positive.

Usage:
    python scripts/compute_detection_baselines.py
    python scripts/compute_detection_baselines.py --api http://localhost:3000
    python scripts/compute_detection_baselines.py --chaos-dir chaos_runs --out docs/test_results/detection_baselines
    python scripts/compute_detection_baselines.py --dry-run   # print report to stdout, no file write
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import os
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    import urllib.request
    import urllib.error
    _HAS_URLLIB = True
except ImportError:
    _HAS_URLLIB = False

# ── Constants ─────────────────────────────────────────────────────────────────

# How long after heal() to still accept a detection as TP (detection latency buffer)
GRACE_NS = 30 * 1_000_000_000  # 30 seconds

SCRIPT_DIR = Path(__file__).parent
REPO_ROOT = SCRIPT_DIR.parent


# ── Ground-truth loading ──────────────────────────────────────────────────────

def load_chaos_runs(chaos_dir: Path) -> list[dict]:
    """Load all injections.csv files from chaos_runs/<run_id>/injections.csv."""
    records: list[dict] = []
    for csv_path in sorted(chaos_dir.glob("*/injections.csv")):
        run_id = csv_path.parent.name
        with open(csv_path, newline="") as f:
            for row in csv.DictReader(f):
                row["run_id"] = run_id
                # Cast numeric fields
                for ns_field in ("injected_at_ns", "healed_at_ns"):
                    try:
                        row[ns_field] = int(row[ns_field]) if row[ns_field] else None
                    except (ValueError, KeyError):
                        row[ns_field] = None
                records.append(row)
    return records


# ── Detection event loading ───────────────────────────────────────────────────

def load_detections_from_api(api_base: str) -> list[dict]:
    """Fetch DetectionEvents from GET /api/detections."""
    url = api_base.rstrip("/") + "/api/detections"
    try:
        with urllib.request.urlopen(url, timeout=10) as resp:
            data = json.loads(resp.read())
        events = data if isinstance(data, list) else data.get("detections", [])
        return events
    except Exception as exc:
        print(f"WARN: could not fetch {url}: {exc}", file=sys.stderr)
        return []


def load_detections_from_parquet(archive_dir: Path) -> list[dict]:
    """Load DetectionEvents from Parquet archive as fallback."""
    try:
        import pyarrow.parquet as pq
    except ImportError:
        return []

    events: list[dict] = []
    for pq_file in sorted(archive_dir.glob("**/*.parquet")):
        try:
            table = pq.read_table(str(pq_file))
            for batch in table.to_batches():
                for i in range(batch.num_rows):
                    row = {col: batch.column(col)[i].as_py() for col in batch.schema.names}
                    # Only include detection-type rows
                    if str(row.get("path", "")).startswith("/detections/") or \
                       "detection" in str(row.get("path", "")).lower():
                        events.append(row)
        except Exception:
            continue
    return events


# ── Matching engine ───────────────────────────────────────────────────────────

def _ns_to_dt(ns: int | None) -> str:
    if ns is None:
        return "—"
    return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def match_detections(
    faults: list[dict],
    detections: list[dict],
) -> dict[str, dict]:
    """
    Returns per-rule stats dict:
      {
        rule_id: {
          tp: int, fp: int, fn: int,
          latencies_ns: [int, ...],    # ns from inject to first detection
          clear_times_ns: [int, ...],  # ns from heal to detection disappearing
        }
      }
    """
    stats: dict[str, dict] = defaultdict(lambda: {
        "tp": 0, "fp": 0, "fn": 0,
        "latencies_ns": [], "clear_times_ns": [],
    })

    # Index detections by target for fast lookup
    det_by_target: dict[str, list[dict]] = defaultdict(list)
    for det in detections:
        target = det.get("target") or det.get("hostname") or ""
        det_by_target[target].append(det)

    # Track which detections were matched (for FP calculation)
    matched_det_ids: set[int] = set()

    for fault in faults:
        hostname = fault.get("hostname", "")
        fault_type = fault.get("fault_type", "unknown")
        injected_ns = fault.get("injected_at_ns")
        healed_ns = fault.get("healed_at_ns")

        if injected_ns is None:
            continue

        window_end = (healed_ns + GRACE_NS) if healed_ns else (injected_ns + 300 * 1_000_000_000)

        # Find matching detections in the fault window
        matched = False
        for i, det in enumerate(det_by_target.get(hostname, [])):
            det_ts = _det_timestamp_ns(det)
            if det_ts is None:
                continue
            if injected_ns <= det_ts <= window_end:
                stats[fault_type]["tp"] += 1
                latency = det_ts - injected_ns
                stats[fault_type]["latencies_ns"].append(latency)
                matched_det_ids.add(id(det))
                matched = True
                # Time-to-clear: look for the detection "clearing" after heal
                if healed_ns:
                    clear_ts = _det_clear_timestamp_ns(det)
                    if clear_ts and clear_ts > healed_ns:
                        stats[fault_type]["clear_times_ns"].append(clear_ts - healed_ns)
                break  # one TP per fault event

        if not matched:
            stats[fault_type]["fn"] += 1

    # FP: detections that matched no fault window on their target
    for det in detections:
        if id(det) not in matched_det_ids:
            det_ts = _det_timestamp_ns(det)
            if det_ts is not None:
                target = det.get("target") or ""
                # Check if any fault was active on this target at this time
                active = any(
                    f.get("hostname") == target
                    and f.get("injected_at_ns") is not None
                    and f["injected_at_ns"] <= det_ts <= (
                        (f["healed_at_ns"] + GRACE_NS) if f.get("healed_at_ns") else (f["injected_at_ns"] + 300e9)
                    )
                    for f in faults
                )
                if not active:
                    det_rule = det.get("rule_id") or det.get("fault_type") or "unknown"
                    stats[det_rule]["fp"] += 1

    return dict(stats)


def _det_timestamp_ns(det: dict) -> int | None:
    for key in ("timestamp_ns", "detected_at_ns", "timestamp"):
        v = det.get(key)
        if v is None:
            continue
        if isinstance(v, (int, float)):
            # If it looks like seconds rather than nanoseconds, convert
            return int(v * 1e9) if v < 1e15 else int(v)
        if isinstance(v, str):
            try:
                dt = datetime.fromisoformat(v.replace("Z", "+00:00"))
                return int(dt.timestamp() * 1e9)
            except ValueError:
                pass
    return None


def _det_clear_timestamp_ns(det: dict) -> int | None:
    for key in ("cleared_at_ns", "resolved_at_ns", "cleared_at"):
        v = det.get(key)
        if v is None:
            continue
        if isinstance(v, (int, float)):
            return int(v * 1e9) if v < 1e15 else int(v)
    return None


# ── Percentile helper ─────────────────────────────────────────────────────────

def _percentile(values: list[float], p: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    idx = p / 100 * (len(s) - 1)
    lo = int(idx)
    hi = min(lo + 1, len(s) - 1)
    frac = idx - lo
    return s[lo] * (1 - frac) + s[hi] * frac


def _fmt_ns(ns: float | None) -> str:
    if ns is None:
        return "—"
    s = ns / 1e9
    if s < 1:
        return f"{ns / 1e6:.0f} ms"
    return f"{s:.1f}s"


def _pct(num: int, den: int) -> str:
    if den == 0:
        return "—"
    return f"{100 * num / den:.1f}%"


# ── Report generation ─────────────────────────────────────────────────────────

def build_report(
    stats: dict[str, dict],
    faults: list[dict],
    detections: list[dict],
    date_str: str,
) -> str:
    lines: list[str] = []
    a = lines.append

    a(f"# Detection Baselines — {date_str}")
    a("")
    a(f"Generated: {datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}")
    a(f"Fault records: {len(faults)}")
    a(f"Detection events: {len(detections)}")
    a("")

    if not stats:
        a("_No data — run chaos_runner.py and restart bonsai to accumulate events._")
        return "\n".join(lines)

    # Summary table
    a("## Per-rule summary")
    a("")
    a("| Rule / Fault type | TP | FP | FN | Precision | Recall | F1 |")
    a("|---|---|---|---|---|---|---|")

    for rule, s in sorted(stats.items()):
        tp, fp, fn = s["tp"], s["fp"], s["fn"]
        prec = tp / (tp + fp) if (tp + fp) > 0 else None
        rec  = tp / (tp + fn) if (tp + fn) > 0 else None
        f1   = (2 * prec * rec / (prec + rec)) if (prec and rec) else None

        prec_s = f"{100*prec:.1f}%" if prec is not None else "—"
        rec_s  = f"{100*rec:.1f}%"  if rec  is not None else "—"
        f1_s   = f"{f1:.3f}"        if f1   is not None else "—"

        a(f"| `{rule}` | {tp} | {fp} | {fn} | {prec_s} | {rec_s} | {f1_s} |")

    a("")
    a("## Detection latency")
    a("")
    a("| Rule / Fault type | p50 | p95 | p99 | N |")
    a("|---|---|---|---|---|")

    for rule, s in sorted(stats.items()):
        lats = s["latencies_ns"]
        p50 = _fmt_ns(_percentile(lats, 50))
        p95 = _fmt_ns(_percentile(lats, 95))
        p99 = _fmt_ns(_percentile(lats, 99))
        a(f"| `{rule}` | {p50} | {p95} | {p99} | {len(lats)} |")

    a("")
    a("## Time-to-clear")
    a("")
    a("| Rule / Fault type | p50 | p95 | p99 | N |")
    a("|---|---|---|---|---|")

    for rule, s in sorted(stats.items()):
        clears = s["clear_times_ns"]
        p50 = _fmt_ns(_percentile(clears, 50))
        p95 = _fmt_ns(_percentile(clears, 95))
        p99 = _fmt_ns(_percentile(clears, 99))
        a(f"| `{rule}` | {p50} | {p95} | {p99} | {len(clears)} |")

    a("")
    a("## Notes")
    a("")
    a("- **TP**: bonsai emitted a detection within the fault window + 30s grace")
    a("- **FP**: bonsai emitted a detection when no fault was active on that target")
    a("- **FN**: fault was injected but no matching detection was emitted")
    a("- Latency measured from `injected_at_ns` to first matching detection timestamp")
    a("- Time-to-clear measured from `healed_at_ns` to `cleared_at_ns` (if present)")

    return "\n".join(lines)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    ap = argparse.ArgumentParser(description="Compute detection baselines from chaos runs")
    ap.add_argument("--api", default="http://localhost:3000",
                    help="Bonsai HTTP API base URL (default: http://localhost:3000)")
    ap.add_argument("--chaos-dir", type=Path, default=REPO_ROOT / "chaos_runs",
                    help="Directory containing chaos_runs/<run_id>/injections.csv files")
    ap.add_argument("--archive-dir", type=Path, default=REPO_ROOT / "runtime" / "archive",
                    help="Parquet archive dir (fallback when API is unreachable)")
    ap.add_argument("--out", type=Path,
                    default=REPO_ROOT / "docs" / "test_results" / "detection_baselines",
                    help="Output directory for markdown reports")
    ap.add_argument("--dry-run", action="store_true",
                    help="Print report to stdout instead of writing file")
    args = ap.parse_args()

    # Load ground truth
    if not args.chaos_dir.exists():
        print(f"WARN: chaos_dir not found: {args.chaos_dir}", file=sys.stderr)
        faults: list[dict] = []
    else:
        faults = load_chaos_runs(args.chaos_dir)
        print(f"Loaded {len(faults)} fault records from {args.chaos_dir}", file=sys.stderr)

    # Load detections (API preferred, parquet fallback)
    detections = load_detections_from_api(args.api)
    if not detections and args.archive_dir.exists():
        print("API returned no detections — trying parquet archive", file=sys.stderr)
        detections = load_detections_from_parquet(args.archive_dir)
    print(f"Loaded {len(detections)} detection events", file=sys.stderr)

    stats = match_detections(faults, detections)

    date_str = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    report = build_report(stats, faults, detections, date_str)

    if args.dry_run:
        print(report)
        return

    args.out.mkdir(parents=True, exist_ok=True)
    out_path = args.out / f"{date_str}.md"
    out_path.write_text(report, encoding="utf-8")
    print(f"Report written: {out_path}", file=sys.stderr)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""D4-19 T5 — Generate a test summary checklist from evidence + Playwright results.

Reads:
  - runtime/driver_results/*.json       (API / event / rejection driver outputs)
  - test-results/results.json           (Playwright JSON report, if present)
  - Previous summary at docs/TEST_SUMMARY.md (for diff / regression detection)

Writes:
  - docs/TEST_SUMMARY.md                (updated summary checklist)
  - Prints Markdown diff to stdout showing regressions or improvements.

Usage:
    python3 scripts/generate_test_summary.py [--results-dir runtime/driver_results]
"""

import argparse
import datetime
import json
import os
import sys
from pathlib import Path


def load_driver_results(results_dir: str) -> dict:
    """Load all JSON results from the driver results directory."""
    results = {}
    p = Path(results_dir)
    if not p.exists():
        return results
    for f in sorted(p.glob("*.json")):
        try:
            data = json.loads(f.read_text())
            results[f.stem] = data
        except (json.JSONDecodeError, OSError):
            results[f.stem] = {"error": f"Failed to parse {f.name}"}
    return results


def load_playwright_results(ui_dir: str = "ui") -> dict:
    """Load Playwright JSON reporter output if available."""
    candidates = [
        Path(ui_dir) / "test-results" / "results.json",
        Path("test-results") / "results.json",
        Path("tests") / "playwright" / "test-results" / "results.json",
    ]
    for c in candidates:
        if c.exists():
            try:
                return json.loads(c.read_text())
            except (json.JSONDecodeError, OSError):
                pass
    return {}


def evaluate_driver(name: str, data: dict) -> tuple:
    """Evaluate a single driver result. Returns (passed: bool, detail: str)."""
    if "error" in data and isinstance(data["error"], str):
        return False, data["error"]

    # Standard driver format: list of step dicts with 'passed' key
    if isinstance(data, list):
        total = len(data)
        passed = sum(1 for s in data if s.get("passed", False))
        if passed == total:
            return True, f"{passed}/{total} steps passed"
        return False, f"{passed}/{total} steps passed"

    # Dict with 'results' or 'steps' key
    for key in ("results", "steps", "checks"):
        if key in data and isinstance(data[key], list):
            total = len(data[key])
            passed = sum(1 for s in data[key] if s.get("passed", s.get("ok", False)))
            if passed == total:
                return True, f"{passed}/{total} {key} passed"
            return False, f"{passed}/{total} {key} passed"

    # Simple success field
    if "success" in data:
        return bool(data["success"]), str(data.get("message", ""))
    if "passed" in data:
        return bool(data["passed"]), str(data.get("detail", ""))

    return True, "no structured result (assumed ok)"


def evaluate_playwright(data: dict) -> list:
    """Parse Playwright JSON report into checklist items."""
    items = []
    suites = data.get("suites", [])
    for suite in suites:
        for spec in suite.get("specs", []):
            title = spec.get("title", "unnamed")
            ok = spec.get("ok", False)
            items.append((title, ok, "passed" if ok else "failed"))
        # Recurse one level
        for child in suite.get("suites", []):
            for spec in child.get("specs", []):
                title = spec.get("title", "unnamed")
                ok = spec.get("ok", False)
                items.append((title, ok, "passed" if ok else "failed"))
    return items


def load_previous_summary(path: str) -> dict:
    """Parse previous TEST_SUMMARY.md to extract result map for diff."""
    results = {}
    p = Path(path)
    if not p.exists():
        return results
    for line in p.read_text().splitlines():
        if line.startswith("| "):
            parts = [c.strip() for c in line.split("|")]
            if len(parts) >= 4 and parts[1] not in ("Test", "---", ""):
                name = parts[1]
                status = "pass" if "✅" in parts[2] else "fail"
                results[name] = status
    return results


def generate_summary(driver_results: dict, playwright_data: dict) -> tuple:
    """Generate the summary table. Returns (markdown_str, current_results_map)."""
    now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    lines = [
        "# Bonsai Test Summary",
        "",
        f"*Generated: {now}*",
        "",
        "## Driver Results",
        "",
        "| Test | Status | Detail |",
        "|------|--------|--------|",
    ]

    current = {}
    for name, data in sorted(driver_results.items()):
        passed, detail = evaluate_driver(name, data)
        icon = "✅" if passed else "❌"
        lines.append(f"| {name} | {icon} | {detail} |")
        current[name] = "pass" if passed else "fail"

    if not driver_results:
        lines.append("| *(no driver results found)* | — | — |")

    # Playwright section
    pw_items = evaluate_playwright(playwright_data)
    if pw_items:
        lines.extend([
            "",
            "## UI Smoke Tests (Playwright)",
            "",
            "| Test | Status | Detail |",
            "|------|--------|--------|",
        ])
        for title, ok, detail in pw_items:
            icon = "✅" if ok else "❌"
            lines.append(f"| {title} | {icon} | {detail} |")
            current[f"pw:{title}"] = "pass" if ok else "fail"
    else:
        lines.extend([
            "",
            "## UI Smoke Tests (Playwright)",
            "",
            "*No Playwright results found. Run `npm run test:smoke` in the `ui/` directory.*",
        ])

    # Summary stats
    total = len(current)
    passed = sum(1 for v in current.values() if v == "pass")
    failed = total - passed
    lines.extend([
        "",
        "## Summary",
        "",
        f"- **Total**: {total}",
        f"- **Passed**: {passed}",
        f"- **Failed**: {failed}",
        f"- **Pass rate**: {passed/total*100:.0f}%" if total > 0 else "- **Pass rate**: N/A",
        "",
    ])

    return "\n".join(lines), current


def compute_diff(previous: dict, current: dict) -> str:
    """Compute Markdown diff between previous and current results."""
    lines = ["## Diff vs Previous Run", ""]
    regressions = []
    improvements = []
    new_tests = []

    for name, status in sorted(current.items()):
        if name not in previous:
            new_tests.append(name)
        elif previous[name] == "pass" and status == "fail":
            regressions.append(name)
        elif previous[name] == "fail" and status == "pass":
            improvements.append(name)

    removed = [n for n in previous if n not in current]

    if regressions:
        lines.append("### 🔴 Regressions")
        for r in regressions:
            lines.append(f"- {r}")
        lines.append("")
    if improvements:
        lines.append("### 🟢 Improvements")
        for i in improvements:
            lines.append(f"- {i}")
        lines.append("")
    if new_tests:
        lines.append("### 🆕 New Tests")
        for n in new_tests:
            lines.append(f"- {n}")
        lines.append("")
    if removed:
        lines.append("### ⚪ Removed")
        for r in removed:
            lines.append(f"- {r}")
        lines.append("")

    if not (regressions or improvements or new_tests or removed):
        lines.append("*No changes from previous run.*")
        lines.append("")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Generate test summary checklist")
    parser.add_argument(
        "--results-dir",
        default="runtime/driver_results",
        help="Directory containing driver JSON results",
    )
    parser.add_argument(
        "--output",
        default="docs/TEST_SUMMARY.md",
        help="Output Markdown file",
    )
    args = parser.parse_args()

    # Load data
    driver_results = load_driver_results(args.results_dir)
    playwright_data = load_playwright_results()

    # Load previous for diff
    previous = load_previous_summary(args.output)

    # Generate
    summary_md, current = generate_summary(driver_results, playwright_data)
    diff_md = compute_diff(previous, current)

    # Write summary
    os.makedirs(os.path.dirname(args.output), exist_ok=True)
    Path(args.output).write_text(summary_md + "\n")
    print(f"✅ Summary written to {args.output}")

    # Print diff to stdout
    if previous:
        print()
        print(diff_md)
    else:
        print("ℹ  No previous summary found — diff skipped (first run).")


if __name__ == "__main__":
    main()

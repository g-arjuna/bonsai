#!/usr/bin/env python3
"""Merge EV1 testing part files into a single consolidated guide."""

import os

PARTS = [
    "ev1-testing-part1.md",
    "ev1-testing-part2.md",
    "ev1-testing-part3.md",
]

HEADER = """# EV1 Ubuntu Testing Guide

> **Sprint**: EV1 — ML Intelligence, GNN Architecture & BonPy–Bonsai Unification
> **Generated**: auto-merged from ev1/ev1-testing-part1..3.md
> **Ubuntu ops box prerequisites**: Rust 1.95+, cmake, protoc, Docker 24+, ContainerLab ≥0.54, Python 3.12+
> **Key ports**: Bonsai API `:3000`, Sidecar health `:9200`, Sidecar Prometheus `:9201`, PyATS sidecar `:5000`

---

## Table of Contents

| Phase | Area |
|-------|------|
| 0 | Pre-Flight Checklist |
| 1 | Clean-Slate Bonsai Core Startup |
| 2 | Device Onboarding — PyATS (automated) + Manual |
| 3 | gNMI Telemetry Flow |
| 4 | Syslog Reception |
| 5 | SNMP Trap Reception |
| 6 | Multi-Source Correlation |
| 7 | Detection Firing Baseline |
| 8 | Remediation Proposal Flow |
| 9 | Python Sidecar Startup |
| 10 | ML Job Engine |
| 11 | Parquet Export Pipeline |
| 12 | STGNN Training |
| 13 | STGNN Live Inference |
| 14 | Semantic Embeddings |
| 15 | Parquet Store Management |
| 16 | Memory Management & Backpressure |
| 17 | BonPy MLOps Console UI |
| 18 | Rule Management (EV1-7) |
| 19 | Change Management Integration |
| 20 | End-to-End ML Fault Detection Cycle |
| 21 | NetBox Integration Test |
| 22 | Final Validation Scorecard |

---

"""

OUTPUT = "../docs/EV1_UBUNTU_TESTING_GUIDE.md"

script_dir = os.path.dirname(os.path.abspath(__file__))
output_path = os.path.join(script_dir, OUTPUT)

sections = []
for part in PARTS:
    part_path = os.path.join(script_dir, part)
    with open(part_path) as f:
        content = f.read()
    # Strip the H1 title from each part (we use the merged header instead)
    lines = content.splitlines(keepends=True)
    # Drop first line if it's the H1
    if lines and lines[0].startswith("# EV1 Ubuntu Testing Guide"):
        lines = lines[1:]
    # Drop leading blank lines
    while lines and lines[0].strip() == "":
        lines = lines[1:]
    # Drop the "---" separator at very top if present
    if lines and lines[0].strip() == "---":
        lines = lines[1:]
    while lines and lines[0].strip() == "":
        lines = lines[1:]
    sections.append("".join(lines))

merged = HEADER + "\n\n---\n\n".join(sections)

os.makedirs(os.path.dirname(output_path), exist_ok=True)
with open(output_path, "w") as f:
    f.write(merged)

lines = merged.count("\n")
print(f"Generated: {output_path}")
print(f"  Parts merged: {len(PARTS)}")
print(f"  Lines: {lines}")
print(f"  Size: {len(merged):,} bytes")

#!/usr/bin/env python3
"""Merge all dv4-*.md files into BONSAI_CONSOLIDATED_BACKLOG_DV4.md"""

import os
import glob

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT = os.path.join(os.path.dirname(SCRIPT_DIR), "BONSAI_CONSOLIDATED_BACKLOG_DV4.md")

# Ordered file list: header first, then epics 1-23
files = [os.path.join(SCRIPT_DIR, "dv4-00-header.md")]
for i in range(1, 24):
    path = os.path.join(SCRIPT_DIR, f"dv4-{i}.md")
    if os.path.exists(path):
        files.append(path)
    else:
        print(f"WARNING: missing {path}")

parts = []
for f in files:
    with open(f, "r") as fh:
        content = fh.read().strip()
    parts.append(content)
    print(f"  read: {os.path.basename(f)} ({len(content)} chars)")

merged = "\n\n---\n\n".join(parts)

with open(OUTPUT, "w") as fh:
    fh.write(merged)
    fh.write("\n")

size_kb = os.path.getsize(OUTPUT) / 1024
print(f"\nWrote {OUTPUT}")
print(f"Total: {len(parts)} sections, {size_kb:.1f} KB")

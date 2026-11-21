#!/usr/bin/env python3
"""m6f_manifest.py — resolve one vendor entry from the M6F manifest.

Usage:
  m6f_manifest.py MANIFEST VENDOR            # print entry as JSON
  m6f_manifest.py MANIFEST --list           # print vendor names
  m6f_manifest.py MANIFEST VENDOR --field F # print single field
"""
import json
import sys

import yaml


def main():
    manifest_path, vendor = sys.argv[1], sys.argv[2]
    doc = yaml.safe_load(open(manifest_path))
    vendors = doc.get("vendors", {})
    if vendor == "--list":
        print("\n".join(sorted(vendors)))
        return 0
    if vendor not in vendors:
        print(f"vendor '{vendor}' not in manifest", file=sys.stderr)
        return 2
    entry = vendors[vendor]
    entry["_vendor"] = vendor
    if len(sys.argv) > 3 and sys.argv[3] == "--field":
        print(entry.get(sys.argv[4], ""))
    else:
        print(json.dumps(entry))
    return 0


if __name__ == "__main__":
    sys.exit(main())

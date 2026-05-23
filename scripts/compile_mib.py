#!/usr/bin/env python3
"""D4-1 T4 — MIB compile pipeline.

Accepts a .mib file path, compiles it using pysmi (if available) or a
simple regex-based OID extractor fallback, and outputs a JSON array of
OID pattern entries suitable for ingestion into Bonsai's SNMP OID pattern
library.

Usage:
    python3 scripts/compile_mib.py <mib_file> [--output <output.json>]

Output format (array of objects):
    [
      {
        "oid_prefix": "1.3.6.1.2.1.2.2.1.8",
        "name": "ifOperStatus",
        "mib_module": "IF-MIB",
        "description": "The current operational state of the interface."
      },
      ...
    ]
"""

import argparse
import json
import re
import sys
from pathlib import Path


def compile_with_pysmi(mib_path: str) -> list:
    """Try to compile using pysmi library."""
    try:
        from pysmi.reader import FileReader
        from pysmi.parser import SmiV2Parser
        from pysmi.codegen import JsonCodeGen
        from pysmi.compiler import MibCompiler
        from pysmi.writer import CallbackWriter

        results = []

        def callback(mibName, jsonDoc, cbCtx):
            try:
                data = json.loads(jsonDoc)
                for name, info in data.items():
                    if isinstance(info, dict) and "oid" in info:
                        results.append({
                            "oid_prefix": info["oid"],
                            "name": name,
                            "mib_module": mibName,
                            "description": info.get("description", ""),
                        })
            except (json.JSONDecodeError, TypeError):
                pass

        mib_dir = str(Path(mib_path).parent)
        compiler = MibCompiler(
            SmiV2Parser(),
            JsonCodeGen(),
            CallbackWriter(callback),
        )
        compiler.addSources(FileReader(mib_dir))
        compiler.compile(Path(mib_path).stem)
        return results
    except ImportError:
        return []


def compile_with_regex(mib_path: str) -> list:
    """Fallback: extract OID definitions using regex patterns."""
    text = Path(mib_path).read_text(errors="replace")
    module_name = Path(mib_path).stem.upper().replace("_", "-")

    # Try to extract MODULE-IDENTITY name
    m = re.search(r"(\S+)\s+MODULE-IDENTITY", text)
    if m:
        module_name = m.group(1)

    results = []
    seen = set()

    # Pattern 1: OBJECT-TYPE with OID assignment
    # e.g., ifOperStatus OBJECT-TYPE ... ::= { ifEntry 8 }
    obj_pattern = re.compile(
        r"(\w+)\s+OBJECT-TYPE\s.*?DESCRIPTION\s*\"([^\"]*?)\".*?::=\s*\{\s*(\w+)\s+(\d+)\s*\}",
        re.DOTALL | re.IGNORECASE,
    )
    for m in obj_pattern.finditer(text):
        name = m.group(1)
        desc = m.group(2).strip().replace("\n", " ")[:200]
        parent = m.group(3)
        idx = m.group(4)
        if name not in seen:
            seen.add(name)
            results.append({
                "oid_prefix": f"{parent}.{idx}",
                "name": name,
                "mib_module": module_name,
                "description": desc,
            })

    # Pattern 2: OBJECT IDENTIFIER assignments
    # e.g., ifMIB OBJECT IDENTIFIER ::= { mib-2 31 }
    oid_pattern = re.compile(
        r"(\w+)\s+OBJECT\s+IDENTIFIER\s*::=\s*\{\s*(\w+)\s+(\d+)\s*\}",
        re.IGNORECASE,
    )
    for m in oid_pattern.finditer(text):
        name = m.group(1)
        parent = m.group(2)
        idx = m.group(3)
        if name not in seen:
            seen.add(name)
            results.append({
                "oid_prefix": f"{parent}.{idx}",
                "name": name,
                "mib_module": module_name,
                "description": "",
            })

    # Pattern 3: NOTIFICATION-TYPE
    notif_pattern = re.compile(
        r"(\w+)\s+NOTIFICATION-TYPE\s.*?DESCRIPTION\s*\"([^\"]*?)\".*?::=\s*\{\s*(\w+)\s+(\d+)\s*\}",
        re.DOTALL | re.IGNORECASE,
    )
    for m in notif_pattern.finditer(text):
        name = m.group(1)
        desc = m.group(2).strip().replace("\n", " ")[:200]
        parent = m.group(3)
        idx = m.group(4)
        if name not in seen:
            seen.add(name)
            results.append({
                "oid_prefix": f"{parent}.{idx}",
                "name": name,
                "mib_module": module_name,
                "description": desc,
            })

    return results


# ── Well-known OID resolution table ──────────────────────────────────────────
WELL_KNOWN = {
    "iso": "1",
    "org": "1.3",
    "dod": "1.3.6",
    "internet": "1.3.6.1",
    "mgmt": "1.3.6.1.2",
    "mib-2": "1.3.6.1.2.1",
    "enterprises": "1.3.6.1.4.1",
    "snmpV2": "1.3.6.1.6",
    "snmpModules": "1.3.6.1.6.3",
    "system": "1.3.6.1.2.1.1",
    "interfaces": "1.3.6.1.2.1.2",
    "ifTable": "1.3.6.1.2.1.2.2",
    "ifEntry": "1.3.6.1.2.1.2.2.1",
    "ip": "1.3.6.1.2.1.4",
    "bgp": "1.3.6.1.2.1.15",
    "ospf": "1.3.6.1.2.1.14",
    "dot1dBridge": "1.3.6.1.2.1.17",
    "snmp": "1.3.6.1.2.1.11",
    "transmission": "1.3.6.1.2.1.10",
}


def resolve_oid(oid_str: str, local_oids: dict) -> str:
    """Resolve symbolic parent.index OID to numeric form."""
    parts = oid_str.split(".")
    if parts[0].isdigit():
        return oid_str

    parent = parts[0]
    rest = ".".join(parts[1:])

    if parent in WELL_KNOWN:
        return f"{WELL_KNOWN[parent]}.{rest}" if rest else WELL_KNOWN[parent]
    if parent in local_oids:
        resolved_parent = resolve_oid(local_oids[parent], local_oids)
        return f"{resolved_parent}.{rest}" if rest else resolved_parent

    return oid_str  # Can't resolve — return as-is


def main():
    parser = argparse.ArgumentParser(description="Compile MIB file to OID patterns")
    parser.add_argument("mib_file", help="Path to .mib file")
    parser.add_argument("--output", "-o", help="Output JSON file (default: stdout)")
    args = parser.parse_args()

    if not Path(args.mib_file).exists():
        print(f"Error: {args.mib_file} not found", file=sys.stderr)
        sys.exit(1)

    # Try pysmi first, fall back to regex
    results = compile_with_pysmi(args.mib_file)
    if not results:
        results = compile_with_regex(args.mib_file)

    # Build local OID map for resolution
    local_oids = {}
    for r in results:
        local_oids[r["name"]] = r["oid_prefix"]

    # Resolve symbolic OIDs to numeric
    for r in results:
        r["oid_prefix"] = resolve_oid(r["oid_prefix"], local_oids)

    if args.output:
        Path(args.output).write_text(json.dumps(results, indent=2))
        print(f"✅ Wrote {len(results)} OID patterns to {args.output}", file=sys.stderr)
    else:
        print(json.dumps(results, indent=2))

    return 0


if __name__ == "__main__":
    sys.exit(main())

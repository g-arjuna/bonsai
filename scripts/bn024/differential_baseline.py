#!/usr/bin/env python3
"""BN-024-01 — bounded differential baseline vs pinned SuzieQ fixtures.

Compares what Bonsai's parser chain produces today against the typed records
SuzieQ extracts for the same device CLI output, using SuzieQ's own
``tests/integration/nossim`` fixtures and ``suzieq/config/schema/*.avsc``
avro schemas at the pinned revision 75198b5585ebef91f0659b6b1ec3c964cc2cd1ad.

This is a *gap-measurement* baseline, not a parity claim: Bonsai's
``bonsai_native`` backend performs an unstructured line-split, so typed-field
parity is expected to be zero. The report makes that explicit per fixture and
records fixture provenance (path + sha256) so the baseline is reproducible.

Usage:
    python3 scripts/bn024/differential_baseline.py \
        --suzieq-src /path/to/suzieq-75198b5 --out report.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import time
from pathlib import Path

PINNED_REVISION = "75198b5585ebef91f0659b6b1ec3c964cc2cd1ad"

# fixture relative path -> SuzieQ table whose avsc schema describes the
# typed records SuzieQ extracts from that command's output.
FIXTURES = {
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_interfaces.txt": "interfaces",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_ip_interfaces.txt": "interfaces",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_ip_route_vrf_all.txt": "routes",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_ipv6_route_vrf_all.txt": "routes",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_ip_arp.txt": "arpnd",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_mac_address_table.txt": "macs",
    "tests/integration/nossim/iosxe/16.10.1/C9300/show_version.txt": "device",
    "tests/integration/nossim/qfx/19.4R1.10/leaf01/show_version.txt": "device",
    "tests/integration/nossim/mx/19.4R3.11/testnode/show_version.txt": "device",
}


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def native_parse(raw: str) -> dict:
    """Mirror of Rust ``native_parse`` in src/parser_chain.rs."""
    lines = [line.strip() for line in raw.splitlines() if line.strip()]
    return {
        "result_kind": "unstructured_fallback",
        "backend_used": "line_split",
        "line_count": len(lines),
        "lines": lines,
    }


def avsc_field_names(path: Path) -> list[str]:
    schema = json.loads(path.read_text())
    return [f["name"] for f in schema.get("fields", [])]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--suzieq-src", required=True, type=Path)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    src = args.suzieq_src
    schema_dir = src / "suzieq" / "config" / "schema"
    results = []
    missing = []

    for rel, table in FIXTURES.items():
        fixture = src / rel
        avsc = schema_dir / f"{table}.avsc"
        if not fixture.exists():
            missing.append(rel)
            continue
        if not avsc.exists():
            missing.append(f"{rel} (schema {table}.avsc)")
            continue

        raw = fixture.read_text(errors="replace")
        start = time.perf_counter()
        parsed = native_parse(raw)
        elapsed_ms = (time.perf_counter() - start) * 1000.0

        fields = avsc_field_names(avsc)
        results.append(
            {
                "fixture": rel,
                "sha256": sha256_file(fixture),
                "bytes": fixture.stat().st_size,
                "suzieq_table": table,
                "suzieq_typed_fields": len(fields),
                "suzieq_field_names": fields,
                "bonsai_backend": "bonsai_native",
                "bonsai_backend_used": "line_split",
                "bonsai_result_kind": parsed["result_kind"],
                "bonsai_line_count": parsed["line_count"],
                "bonsai_parse_ms": round(elapsed_ms, 3),
                "typed_field_parity": 0,
                "parity_note": "bonsai_native emits raw lines only; no typed fields extracted",
            }
        )

    report = {
        "baseline": "bn024-01-differential",
        "suzieq_pinned_revision": PINNED_REVISION,
        "suzieq_src": str(src),
        "method": "bonsai_native line-split vs suzieq avsc typed schema field count",
        "fixtures": results,
        "missing": missing,
        "summary": {
            "fixtures_evaluated": len(results),
            "total_suzieq_typed_fields": sum(r["suzieq_typed_fields"] for r in results),
            "total_typed_field_parity": sum(r["typed_field_parity"] for r in results),
        },
    }

    text = json.dumps(report, indent=2)
    if args.out:
        args.out.write_text(text + "\n")
    else:
        sys.stdout.write(text + "\n")
    if missing:
        print(f"warning: {len(missing)} fixture(s) missing: {missing}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

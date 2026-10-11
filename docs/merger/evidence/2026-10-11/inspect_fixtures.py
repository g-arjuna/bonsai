"""Inspect existing fixture bytes independently of their generator/test helpers.

Usage: python3 inspect_fixtures.py /path/to/rustybmp
Outputs JSON; never writes to the source repository. This is byte evidence,
not execution of a BMP parser or a conformance test suite.
"""

import hashlib
import json
from pathlib import Path
import re
import sys


root = Path(sys.argv[1]).resolve()
rows = []
for path in sorted((root / "tests/fixtures/bmp").glob("*.bin")):
    data = path.read_bytes()
    if len(data) < 6:
        raise ValueError(f"Common header truncated: {path}")
    declared = int.from_bytes(data[1:5], "big")
    rows.append({
        "file": path.name,
        "sha256": hashlib.sha256(data).hexdigest(),
        "actual_length": len(data),
        "version": data[0],
        "rfc_length": declared,
        "rfc_type": data[5],
        "first_seven_bytes_hex": data[:7].hex(),
        "length_matches": declared == len(data),
    })
if not rows:
    raise ValueError("No binary fixtures found")

source = (root / "tests/integration/bmp_pdu.rs").read_text()
literal = source.split("const ROUTE_MONITOR_PDU: &[u8] = &[", 1)[1].split("];", 1)[0]
literal = re.sub(r"//[^\n]*", "", literal)
data = bytes(int(value, 16) for value in re.findall(r"0x[0-9A-Fa-f]+", literal))
bgp_offset = 6 + 42
print(json.dumps({
    "reference": "RFC 7854 section 4.1: version at 0, length at 1..4, type at 5",
    "fixtures": rows,
    "inline_route_monitor": {
        "actual_length": len(data),
        "declared_length": int.from_bytes(data[1:5], "big"),
        "bgp_actual_length": len(data) - bgp_offset,
        "bgp_declared_length": int.from_bytes(data[bgp_offset + 16:bgp_offset + 18], "big"),
    },
}, indent=2))

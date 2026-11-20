#!/usr/bin/env python3
"""Generate the independent pyang reference schema for the checked-in YANG
fixtures used by tests/yang_catalogue.rs.

This is a DEVELOPMENT/TEST ORACLE ONLY. pyang is not a Bonsai runtime
dependency; the committed reference JSON is what the Rust test suite
compares against, so normal test runs stay fully offline.

Usage:
    python3 scripts/yang_reference.py --pyang /path/to/pyang \
        --out tests/fixtures/yang/reference/expected-schema.json

Regenerate after changing fixtures or the pyang version, then review the
diff.  Deterministic: fixture files are processed in sorted order and the
output JSON is sorted; no timestamps are embedded.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys

DEFAULT_DIRS = [
    "tests/fixtures/yang/oc",
    "tests/fixtures/yang/ietf",
]

NODE_RE = re.compile(r"([+xo])--([a-zA-Z-]{0,2})\s+(\S+)(.*)$")
STATUS = {"+": "current", "x": "deprecated", "o": "obsolete"}


def fixture_files(dirs):
    files = []
    for d in dirs:
        for name in sorted(os.listdir(d)):
            if name.endswith(".yang"):
                files.append(os.path.join(d, name))
    return sorted(files)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_tree(text):
    """Parse `pyang -f tree` output into canonical schema entries.

    Returns {module_name: [entry, ...]} where each entry is
    {path, kind, config, mandatory, keys, type}.
    """
    modules = {}
    current = None
    stack = []  # (depth, name) — depth counted in tree-drawing columns

    for raw in text.splitlines():
        line = raw.rstrip("\n")
        if line.startswith("module:"):
            current = line.split(":", 1)[1].strip()
            modules[current] = []
            stack = []
            continue
        if current is None or not line.strip():
            continue
        m = NODE_RE.search(line)
        if not m:
            continue
        status_ch, flags, name_tok, rest = m.groups()
        # Marker column is 2 + 3*depth (pyang tree-drawing convention).
        marker_start = max(
            line.rfind("+--"), line.rfind("x--"), line.rfind("o--")
        )
        depth = (marker_start - 2) // 3

        # Non-data statements (rpc/action/notification/input/output/uses)
        # carry '-' flags; they are not instance-data schema nodes. They
        # still occupy a tree level, so push a transparent placeholder.
        if flags.startswith("-"):
            stack = stack[:depth] + [None]
            continue

        # choice '(name)[?]' and case ':(name)' are transparent wrappers.
        transparent = name_tok.startswith("(") or name_tok.startswith(":(")

        is_list = name_tok.endswith("*")
        optional = name_tok.endswith("?")
        name = name_tok.rstrip("*?!")
        if name.startswith("("):
            name = name.strip("()")
        if name.startswith(":("):
            name = name[2:].rstrip(")")
        toks = rest.split()
        keys = []
        annotation_toks = []
        for t in toks:
            if t.startswith("[") and t.endswith("]") and not annotation_toks:
                keys = t[1:-1].split()
            else:
                annotation_toks.append(t)
        annotation = " ".join(annotation_toks)
        is_leafref = "->" in annotation

        has_keys = bool(keys) or any(t == "[]" for t in annotation_toks)
        if transparent:
            kind = None
        elif is_list and has_keys:
            kind = "list"
        elif is_list:
            kind = "leaf-list"
        elif annotation or is_leafref:
            kind = "leaf"
        else:
            kind = "container"

        stack = stack[:depth] + [name if not transparent else None]
        if transparent or kind is None:
            continue
        path = "/" + "/".join(s for s in stack if s)

        config = flags == "rw"
        # pyang only marks optionality where 'mandatory' applies; for
        # containers/lists/leaf-lists the flag is not meaningful -> null.
        mandatory = (
            None
            if kind in ("container", "list", "leaf-list")
            else not optional
        )

        modules[current].append(
            {
                "path": path,
                "kind": kind,
                "config": config,
                "mandatory": mandatory,
                "keys": keys,
                "status": STATUS.get(status_ch, status_ch),
                "type": annotation,
            }
        )

    for entries in modules.values():
        entries.sort(key=lambda e: e["path"])
    return modules


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pyang", default=os.environ.get("PYANG", "pyang"))
    ap.add_argument("--dirs", nargs="+", default=DEFAULT_DIRS)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    files = fixture_files(args.dirs)
    if not files:
        sys.exit("no fixture .yang files found")

    version = subprocess.run(
        [args.pyang, "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()

    search_dirs = sorted({os.path.dirname(f) for f in files})
    cmd = [args.pyang]
    for d in search_dirs:
        cmd += ["-p", d]
    cmd += ["-f", "tree"] + files
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(f"pyang failed with exit {proc.returncode}")
    if proc.stderr.strip():
        sys.stderr.write("pyang diagnostics:\n" + proc.stderr)

    modules = parse_tree(proc.stdout)

    out = {
        "oracle": "pyang",
        "pyang_version": version,
        "command": " ".join(cmd),
        "fixture_sha256": {f: sha256(f) for f in files},
        "modules": modules,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(out, f, indent=2, sort_keys=True)
        f.write("\n")
    total = sum(len(v) for v in modules.values())
    print(f"wrote {args.out}: {total} paths across {len(modules)} modules")


if __name__ == "__main__":
    main()

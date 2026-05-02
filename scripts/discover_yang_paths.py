#!/usr/bin/env python3
"""Discover subscribable gNMI paths from public YANG repositories.

Clones (or pulls) canonical public YANG repositories, parses YANG files with
pyang, extracts container and list paths that are suitable for gNMI
subscription, and writes draft profile-candidates.yaml files for human
curation.

Discovered paths are CANDIDATES — they require lab verification before
promotion to the default catalogue. See docs/path_profiles/PROMOTING.md
for the curation workflow.

Usage:
    python scripts/discover_yang_paths.py [OPTIONS]

Options:
    --vendor VENDOR     Only process this vendor (openconfig, nokia, cisco, juniper, arista)
    --cache-dir DIR     Where to clone repos (default: .yang-cache)
    --output-dir DIR    Where to write candidates (default: discovered_paths)
    --no-pull           Skip git pull on already-cloned repos
    --dry-run           Print discovered paths, do not write YAML
    --list-sources      Print configured vendor sources and exit
    --max-paths N       Maximum paths per vendor (default: 200)

Requirements:
    pip install pyyaml gitpython pyang
"""

from __future__ import annotations

import argparse
import datetime
import re
import shutil
import subprocess
import sys
import textwrap
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterator

try:
    import yaml
except ImportError:
    print("error: pyyaml not installed.  Run: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

REPO_ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# Vendor source registry
# ---------------------------------------------------------------------------

@dataclass
class VendorSource:
    vendor: str
    repo_url: str
    clone_name: str
    yang_subdirs: list[str]       # relative paths inside the repo to search
    model_prefixes: list[str]     # only parse files whose name starts with these
    vendor_tag: str               # tag written into candidate YAML (matches bonsai vendor keys)
    description: str


VENDOR_SOURCES: list[VendorSource] = [
    VendorSource(
        vendor="openconfig",
        repo_url="https://github.com/openconfig/public.git",
        clone_name="openconfig-public",
        yang_subdirs=["release/models"],
        model_prefixes=["openconfig-"],
        vendor_tag="",
        description="OpenConfig public YANG models",
    ),
    VendorSource(
        vendor="nokia",
        repo_url="https://github.com/nokia/7x50_YangModels.git",
        clone_name="nokia-yang",
        yang_subdirs=["YANG/nokia-combined", "YANG"],
        model_prefixes=["nokia-", "srl_nokia"],
        vendor_tag="nokia_srl",
        description="Nokia 7x50 / SR Linux YANG models",
    ),
    VendorSource(
        vendor="cisco",
        repo_url="https://github.com/YangModels/yang.git",
        clone_name="yangmodels-yang",
        yang_subdirs=[
            "vendor/cisco/xr/791",
            "vendor/cisco/xr/792",
            "vendor/cisco/xr/800",
        ],
        model_prefixes=["Cisco-IOS-XR-"],
        vendor_tag="cisco_xrd",
        description="Cisco IOS-XR YANG models (via YangModels/yang)",
    ),
    VendorSource(
        vendor="juniper",
        repo_url="https://github.com/Juniper/yang.git",
        clone_name="juniper-yang",
        yang_subdirs=["23.2/23.2R1", "22.4/22.4R1"],
        model_prefixes=["junos-", "Juniper-"],
        vendor_tag="juniper_crpd",
        description="Juniper YANG models",
    ),
    VendorSource(
        vendor="arista",
        repo_url="https://github.com/aristanetworks/yang.git",
        clone_name="arista-yang",
        yang_subdirs=["EOS-4.31.0F/release/openconfig", "EOS-4.29.0F/release/openconfig"],
        model_prefixes=["arista-", "openconfig-"],
        vendor_tag="arista_ceos",
        description="Arista EOS YANG models",
    ),
]

VENDOR_SOURCE_MAP = {s.vendor: s for s in VENDOR_SOURCES}

# ---------------------------------------------------------------------------
# Candidate path dataclass
# ---------------------------------------------------------------------------

@dataclass
class CandidatePath:
    path: str
    origin: str            # "openconfig" or "" (native)
    mode: str              # SAMPLE | ON_CHANGE | BOTH
    sample_interval_ns: int
    source_repo: str
    source_file: str
    yang_module: str
    needs_lab_verification: bool = True
    optional: bool = False
    rationale: str = ""
    vendor_only: list[str] = field(default_factory=list)
    required_any_models: list[str] = field(default_factory=list)

    def to_dict(self) -> dict:
        d: dict = {
            "path": self.path,
            "origin": self.origin,
            "mode": self.mode,
        }
        if self.sample_interval_ns:
            d["sample_interval_ns"] = self.sample_interval_ns
        if self.required_any_models:
            d["required_any_models"] = self.required_any_models
        if self.vendor_only:
            d["vendor_only"] = self.vendor_only
        if self.optional:
            d["optional"] = True
        d["rationale"] = self.rationale or f"Discovered from {self.yang_module}."
        d["_discovery_meta"] = {
            "needs_lab_verification": True,
            "source_repo": self.source_repo,
            "source_file": self.source_file,
            "yang_module": self.yang_module,
            "discovered_at": datetime.datetime.utcnow().isoformat() + "Z",
        }
        return d


# ---------------------------------------------------------------------------
# pyang detection and invocation
# ---------------------------------------------------------------------------

def check_pyang() -> str | None:
    """Return the pyang executable path, or None if not found."""
    pyang_path = shutil.which("pyang")
    if pyang_path:
        return pyang_path
    # Try the venv at repo root
    venv_pyang = REPO_ROOT / ".venv" / "bin" / "pyang"
    if venv_pyang.exists():
        return str(venv_pyang)
    return None


def pyang_tree(yang_file: Path, search_dirs: list[Path], pyang_exe: str) -> str | None:
    """Run pyang -f tree on a YANG file and return the output, or None on error."""
    cmd = [pyang_exe, "-f", "tree", "--tree-line-length=0"]
    for d in search_dirs:
        cmd += ["-p", str(d)]
    cmd.append(str(yang_file))
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode == 0 and result.stdout:
            return result.stdout
    except (subprocess.TimeoutExpired, OSError):
        pass
    return None


# ---------------------------------------------------------------------------
# YANG tree parser — extracts subscribable container and list paths
# ---------------------------------------------------------------------------

# Pattern for pyang tree nodes:
#   +--rw interfaces
#   +--rw interface* [name]
_TREE_NODE_RE = re.compile(
    r"^\s*\+--(?:rw|ro)\s+([\w.-]+)(\*?)\s*(?:\[([^\]]+)\])?"
)
_MODULE_RE = re.compile(r"^module:\s+([\w.-]+)")

# Paths that are almost certainly too verbose / low-value for gNMI subscription
_SKIP_SUFFIXES = {
    "config", "state", "input", "output", "groupings", "typedefs",
    "identities", "augment", "deviation",
}

# Top-level containers/lists we want to promote (curated list per origin)
_OC_INTERESTING = {
    "interfaces", "network-instances", "lldp", "bgp", "bfd",
    "mpls", "segment-routing", "rib", "local-routes",
    "platform", "system", "qos", "acl", "routing-policy",
    "isis", "ospf", "lacp", "vlan", "stp", "spanning-tree",
}


def parse_tree_output(tree_text: str, yang_file: Path, source: VendorSource) -> list[CandidatePath]:
    """Parse pyang -f tree output and return candidate paths."""
    candidates: list[CandidatePath] = []
    module_name = ""
    stack: list[tuple[int, str]] = []   # (indent, path_segment)

    for line in tree_text.splitlines():
        module_m = _MODULE_RE.match(line)
        if module_m:
            module_name = module_m.group(1)
            stack.clear()
            continue

        node_m = _TREE_NODE_RE.match(line)
        if not node_m:
            continue

        # Compute indent depth (2 spaces per level after the initial +--rw)
        indent = len(line) - len(line.lstrip())
        name = node_m.group(1)
        is_list = bool(node_m.group(2))
        keys = node_m.group(3) or ""

        if name.lower() in _SKIP_SUFFIXES:
            continue

        # Trim stack to current depth
        while stack and stack[-1][0] >= indent:
            stack.pop()

        stack.append((indent, name))

        # Only emit paths at depth 1 (top-level modules) and depth 2 (first children)
        depth = len(stack)
        if depth > 3:
            continue

        # Build the path
        path_parts = [seg for _, seg in stack]
        if is_list and keys:
            key_list = "/".join(f"{k.strip()}=*" for k in keys.split())
            path_parts[-1] = f"{path_parts[-1]}[{key_list}]"

        path = "/".join(path_parts)

        # Decide origin
        if source.vendor == "openconfig" or module_name.startswith("openconfig-"):
            origin = "openconfig"
            # Only emit if top-level name is in our interesting set
            if path_parts[0].split("[")[0] not in _OC_INTERESTING and depth == 1:
                continue
        else:
            origin = ""

        # Decide mode and interval
        if is_list or depth >= 2:
            mode = "ON_CHANGE"
            interval = 0
        else:
            mode = "SAMPLE"
            interval = 30_000_000_000   # 30s default for new candidates

        # vendor_only
        vendor_only = [source.vendor_tag] if source.vendor_tag else []

        # required_any_models
        required_any = [module_name] if module_name else []

        rel_file = str(yang_file.relative_to(REPO_ROOT / ".yang-cache" / source.clone_name))

        candidate = CandidatePath(
            path=path,
            origin=origin,
            mode=mode,
            sample_interval_ns=interval,
            source_repo=source.repo_url,
            source_file=rel_file,
            yang_module=module_name,
            vendor_only=vendor_only,
            required_any_models=required_any,
        )
        candidates.append(candidate)

    return candidates


# ---------------------------------------------------------------------------
# Fallback: regex-based path extraction (no pyang)
# ---------------------------------------------------------------------------

_CONTAINER_RE = re.compile(r"^\s+container\s+([\w-]+)\s*\{", re.MULTILINE)
_LIST_RE = re.compile(r"^\s+list\s+([\w-]+)\s*\{", re.MULTILINE)
_MODULE_NAME_RE = re.compile(r"^\s*module\s+([\w-]+)\s*\{", re.MULTILINE)


def regex_extract_paths(yang_file: Path, source: VendorSource) -> list[CandidatePath]:
    """Fallback: extract top-level containers/lists via regex when pyang is unavailable."""
    try:
        text = yang_file.read_text(errors="replace")
    except OSError:
        return []

    module_m = _MODULE_NAME_RE.search(text)
    module_name = module_m.group(1) if module_m else yang_file.stem

    origin = "openconfig" if (
        source.vendor == "openconfig" or module_name.startswith("openconfig-")
    ) else ""

    candidates: list[CandidatePath] = []
    seen: set[str] = set()

    for m in list(_CONTAINER_RE.finditer(text)) + list(_LIST_RE.finditer(text)):
        name = m.group(1)
        if name in _SKIP_SUFFIXES or name in seen:
            continue
        if origin == "openconfig" and name not in _OC_INTERESTING:
            continue
        seen.add(name)

        is_list = "list" in text[m.start():m.start() + 10]
        vendor_only = [source.vendor_tag] if source.vendor_tag else []
        rel_file = str(yang_file.name)

        candidates.append(CandidatePath(
            path=name,
            origin=origin,
            mode="ON_CHANGE" if is_list else "SAMPLE",
            sample_interval_ns=0 if is_list else 30_000_000_000,
            source_repo=source.repo_url,
            source_file=rel_file,
            yang_module=module_name,
            vendor_only=vendor_only,
            required_any_models=[module_name],
            rationale=f"Regex-extracted from {module_name} (pyang not available — verify manually).",
        ))

    return candidates


# ---------------------------------------------------------------------------
# Git operations
# ---------------------------------------------------------------------------

def clone_or_pull(cache_dir: Path, source: VendorSource, no_pull: bool) -> Path | None:
    """Clone or update a repo. Returns the repo directory, or None on failure."""
    repo_dir = cache_dir / source.clone_name

    if repo_dir.exists():
        if no_pull:
            print(f"  [cache] {source.clone_name} (skipping pull)")
            return repo_dir
        print(f"  [pull]  {source.clone_name} ...", end="", flush=True)
        try:
            subprocess.run(
                ["git", "pull", "--ff-only", "--quiet"],
                cwd=repo_dir,
                check=True,
                capture_output=True,
                timeout=120,
            )
            print(" ok")
            return repo_dir
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e:
            print(f" warning: pull failed ({e}); using cached state")
            return repo_dir
    else:
        print(f"  [clone] {source.clone_name} ...", end="", flush=True)
        try:
            subprocess.run(
                ["git", "clone", "--depth=1", "--quiet", source.repo_url, str(repo_dir)],
                check=True,
                capture_output=True,
                timeout=300,
            )
            print(" ok")
            return repo_dir
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as e:
            print(f" FAILED: {e}")
            return None


# ---------------------------------------------------------------------------
# Discovery per vendor
# ---------------------------------------------------------------------------

def discover_for_source(
    source: VendorSource,
    repo_dir: Path,
    pyang_exe: str | None,
    max_paths: int,
) -> list[CandidatePath]:
    """Walk YANG files in the configured subdirs and extract candidate paths."""
    candidates: list[CandidatePath] = []
    seen_paths: set[str] = set()

    search_dirs: list[Path] = []
    yang_files: list[Path] = []

    for subdir in source.yang_subdirs:
        target = repo_dir / subdir
        if not target.exists():
            continue
        search_dirs.append(target)
        for yang_file in sorted(target.rglob("*.yang")):
            # Only process files whose names match our prefix filters
            if not any(yang_file.name.startswith(p) for p in source.model_prefixes):
                continue
            yang_files.append(yang_file)

    if not yang_files:
        print(f"    no matching .yang files found in configured subdirs")
        return []

    print(f"    {len(yang_files)} YANG files to process", flush=True)

    for yang_file in yang_files:
        if len(candidates) >= max_paths:
            break

        if pyang_exe:
            tree_text = pyang_tree(yang_file, search_dirs, pyang_exe)
            if tree_text:
                new_paths = parse_tree_output(tree_text, yang_file, source)
            else:
                new_paths = regex_extract_paths(yang_file, source)
        else:
            new_paths = regex_extract_paths(yang_file, source)

        for cp in new_paths:
            if cp.path not in seen_paths:
                seen_paths.add(cp.path)
                candidates.append(cp)
                if len(candidates) >= max_paths:
                    break

    return candidates


# ---------------------------------------------------------------------------
# Output: profile-candidates.yaml
# ---------------------------------------------------------------------------

def write_candidates(
    output_dir: Path,
    source: VendorSource,
    candidates: list[CandidatePath],
    repo_dir: Path,
) -> Path:
    """Write candidates to discovered_paths/<vendor>/<clone_name>/profile-candidates.yaml."""
    # Try to get the repo's current HEAD short SHA for the release label
    release = "unknown-release"
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=repo_dir,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode == 0:
            release = result.stdout.strip()
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
        pass

    out_dir = output_dir / source.vendor / release
    out_dir.mkdir(parents=True, exist_ok=True)
    out_file = out_dir / "profile-candidates.yaml"

    header = textwrap.dedent(f"""\
        # profile-candidates.yaml — auto-generated by scripts/discover_yang_paths.py
        #
        # Source  : {source.description}
        # Repo    : {source.repo_url}
        # Revision: {release}
        # Generated: {datetime.datetime.utcnow().isoformat()}Z
        #
        # THESE ARE CANDIDATES — NOT CATALOGUE ENTRIES.
        # Each path requires lab verification before promotion.
        # See docs/path_profiles/PROMOTING.md for the curation workflow.
        #
        # To promote a path to the catalogue:
        #   1. Verify in the lab that the device streams data on this path.
        #   2. Record vendor, OS version, and model in the rationale.
        #   3. Move the path entry (minus _discovery_meta) to the appropriate
        #      profile YAML in config/path_profiles/.
        #   4. Remove _discovery_meta before committing.
        #
        name: {source.vendor}-candidates
        description: "Discovered {source.vendor} paths — needs lab verification"
        environment: []
        vendor_scope: [{repr(source.vendor_tag) if source.vendor_tag else ""}]
        roles: []
        rationale: "Auto-generated candidates from {source.repo_url}"
        paths:
    """)

    path_entries = []
    for cp in candidates:
        path_entries.append(cp.to_dict())

    with out_file.open("w") as f:
        f.write(header)
        yaml.dump(path_entries, f, default_flow_style=False, allow_unicode=True, indent=2)

    return out_file


# ---------------------------------------------------------------------------
# Workflow promotion doc
# ---------------------------------------------------------------------------

def write_promoting_doc(docs_dir: Path) -> None:
    promoting_path = docs_dir / "PROMOTING.md"
    if promoting_path.exists():
        return

    content = textwrap.dedent("""\
        # Promoting Discovered Paths to the Default Catalogue

        Paths in `discovered_paths/` are **candidates** — they come from public YANG
        repositories and have not been tested against real devices. Promotion to the
        default catalogue (`config/path_profiles/`) requires explicit lab verification.

        ## Promotion Steps

        1. **Find candidates**: `discovered_paths/<vendor>/<revision>/profile-candidates.yaml`

        2. **Lab-verify the path**
           - Add the path to a temporary profile or use `bonsai device` CLI to test.
           - Confirm the device streams data on a `SUBSCRIBE` RPC.
           - Record the device OS version, vendor, and any quirks.

        3. **Choose the target profile**
           - Check `config/path_profiles/` for an existing profile that fits.
           - If none exists, create a new profile YAML following the v2 schema.

        4. **Move the path entry**
           - Copy the `path`, `origin`, `mode`, `sample_interval_ns`,
             `required_any_models`, `vendor_only`, and `optional` fields.
           - Write a human-authored `rationale` (not the generated one).
           - **Drop `_discovery_meta`** entirely — do not commit it.

        5. **Generate the doc**
           ```
           python scripts/gen_profile_docs.py --profile <profile-name> --force
           ```

        6. **Commit**
           - Include the source repo, revision, and lab-verified device info in the commit message.

        ## Cadence

        Run the discovery script periodically when vendor YANG repos publish new releases:
        ```
        python scripts/discover_yang_paths.py
        ```

        New candidate files are written to `discovered_paths/` (gitignored by default).
        They are input material for manual curation, not committed artefacts.
    """)
    promoting_path.write_text(content)
    print(f"  wrote  docs/path_profiles/PROMOTING.md")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--vendor", help="Only process this vendor")
    parser.add_argument("--cache-dir", default=str(REPO_ROOT / ".yang-cache"), help="Git cache directory")
    parser.add_argument("--output-dir", default=str(REPO_ROOT / "discovered_paths"), help="Output directory")
    parser.add_argument("--no-pull", action="store_true", help="Skip git pull")
    parser.add_argument("--dry-run", action="store_true", help="Print without writing")
    parser.add_argument("--list-sources", action="store_true", help="List configured sources and exit")
    parser.add_argument("--max-paths", type=int, default=200, help="Max paths per vendor")
    args = parser.parse_args()

    if args.list_sources:
        print("Configured vendor sources:")
        for s in VENDOR_SOURCES:
            subdirs = ", ".join(s.yang_subdirs[:2])
            if len(s.yang_subdirs) > 2:
                subdirs += f" (+{len(s.yang_subdirs) - 2} more)"
            print(f"  {s.vendor:<12} {s.description}")
            print(f"              repo: {s.repo_url}")
            print(f"              dirs: {subdirs}")
        return

    sources = VENDOR_SOURCES
    if args.vendor:
        if args.vendor not in VENDOR_SOURCE_MAP:
            print(f"error: unknown vendor '{args.vendor}'. Use --list-sources.", file=sys.stderr)
            sys.exit(1)
        sources = [VENDOR_SOURCE_MAP[args.vendor]]

    cache_dir = Path(args.cache_dir)
    output_dir = Path(args.output_dir)
    docs_dir = REPO_ROOT / "docs" / "path_profiles"

    pyang_exe = check_pyang()
    if pyang_exe:
        print(f"pyang found: {pyang_exe}")
    else:
        print(
            "warning: pyang not found. Using regex-based fallback (less accurate).\n"
            "         Install with: pip install pyang\n",
            file=sys.stderr,
        )

    cache_dir.mkdir(parents=True, exist_ok=True)
    docs_dir.mkdir(parents=True, exist_ok=True)

    if not args.dry_run:
        write_promoting_doc(docs_dir)

    total_candidates = 0

    for source in sources:
        print(f"\n[{source.vendor}] {source.description}")

        repo_dir = clone_or_pull(cache_dir, source, args.no_pull)
        if repo_dir is None:
            print(f"  skipping {source.vendor} — clone failed")
            continue

        print(f"  discovering paths ...", flush=True)
        candidates = discover_for_source(source, repo_dir, pyang_exe, args.max_paths)

        if not candidates:
            print(f"  no candidates found for {source.vendor}")
            continue

        print(f"  found {len(candidates)} candidate paths")

        if args.dry_run:
            for cp in candidates[:10]:
                print(f"    {cp.origin or 'native':<12} {cp.mode:<10} {cp.path}")
            if len(candidates) > 10:
                print(f"    ... and {len(candidates) - 10} more")
        else:
            out_file = write_candidates(output_dir, source, candidates, repo_dir)
            print(f"  wrote   {out_file.relative_to(REPO_ROOT)}")
            total_candidates += len(candidates)

    if not args.dry_run:
        print(f"\ndone: {total_candidates} total candidate paths across {len(sources)} vendor(s)")
        print(f"output: {output_dir.relative_to(REPO_ROOT)}/")
        print(f"next:   review candidates and promote verified paths — see docs/path_profiles/PROMOTING.md")


if __name__ == "__main__":
    main()

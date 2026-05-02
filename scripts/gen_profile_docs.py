#!/usr/bin/env python3
"""Generate markdown documentation for bonsai path profiles.

Reads all YAML files in config/path_profiles/ and produces a corresponding
docs/path_profiles/<name>.md for each one that doesn't already have a doc.

Usage:
    python scripts/gen_profile_docs.py [--force] [--profile NAME]

Options:
    --force          Overwrite existing docs (default: skip existing)
    --profile NAME   Only process the named profile (matches YAML name field)
    --dry-run        Print what would be generated without writing files
"""

import argparse
import sys
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:
    print("error: pyyaml not installed. Run: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

REPO_ROOT = Path(__file__).resolve().parent.parent
PROFILES_DIR = REPO_ROOT / "config" / "path_profiles"
DOCS_DIR = REPO_ROOT / "docs" / "path_profiles"


def load_profiles(directory: Path) -> list[dict]:
    profiles = []
    for yaml_file in sorted(directory.glob("*.yaml")):
        if yaml_file.name == "MANIFEST.yaml":
            continue
        with yaml_file.open() as f:
            try:
                data = yaml.safe_load(f)
                if isinstance(data, dict) and "name" in data:
                    data["_source_file"] = yaml_file.name
                    profiles.append(data)
            except yaml.YAMLError as e:
                print(f"warning: skipping {yaml_file.name}: {e}", file=sys.stderr)
    return profiles


def _sample_interval_human(ns: int) -> str:
    if ns == 0:
        return "—"
    seconds = ns // 1_000_000_000
    if seconds >= 60:
        return f"{seconds // 60}m"
    return f"{seconds}s"


def _models_cell(path: dict) -> str:
    required = path.get("required_models", [])
    any_models = path.get("required_any_models", [])
    if required:
        return ", ".join(f"`{m}`" for m in required)
    if any_models:
        return "any of: " + ", ".join(f"`{m}`" for m in any_models)
    return "—"


def _vendor_cell(path: dict) -> str:
    vendor_only = path.get("vendor_only", [])
    if not vendor_only:
        return "all vendors"
    return ", ".join(vendor_only)


def _path_display(path_str: str) -> str:
    if len(path_str) > 60:
        return f"`…{path_str[-55:]}`"
    return f"`{path_str}`"


def _environment_summary(profile: dict) -> str:
    envs = profile.get("environment", [])
    if not envs:
        return "all environments"
    return ", ".join(e.replace("_", " ") for e in envs)


def _roles_summary(profile: dict) -> str:
    roles = profile.get("roles", [])
    if not roles:
        return "all roles"
    return ", ".join(roles)


def _vendor_scope_summary(profile: dict) -> str:
    vs = profile.get("vendor_scope", [])
    if not vs:
        return "all vendors (OpenConfig + per-vendor natives)"
    return ", ".join(vs)


def _categorize_paths(paths: list[dict]) -> dict[str, list[dict]]:
    """Split paths into openconfig, vendor-native, and universal groups."""
    oc = [p for p in paths if p.get("origin") == "openconfig"]
    native = [p for p in paths if p.get("origin", "") != "openconfig" and p.get("vendor_only")]
    universal = [p for p in paths if p.get("origin", "") != "openconfig" and not p.get("vendor_only")]
    return {"openconfig": oc, "native": native, "universal": universal}


def generate_doc(profile: dict) -> str:
    name = profile["name"]
    description = profile.get("description", "")
    rationale = profile.get("rationale", "")
    paths = profile.get("paths", [])

    env_str = _environment_summary(profile)
    roles_str = _roles_summary(profile)
    vendor_str = _vendor_scope_summary(profile)

    lines = []

    # Header
    lines.append(f"# {name} — {description}")
    lines.append("")
    lines.append(f"**Environment**: {env_str}  ")
    lines.append(f"**Roles**: {roles_str}  ")
    lines.append(f"**Vendor scope**: {vendor_str}  ")
    lines.append("**Verification**: not-yet-verified")
    lines.append("")

    # Rationale
    if rationale:
        lines.append("## Rationale")
        lines.append("")
        lines.append(rationale)
        lines.append("")

    # Path table
    lines.append("## Subscribed Paths")
    lines.append("")
    lines.append("| Path | Origin | Mode | Interval | Models | Vendors | Optional |")
    lines.append("|------|--------|------|----------|--------|---------|----------|")

    for p in paths:
        path_str = p.get("path", "")
        origin = p.get("origin", "") or "native"
        mode = p.get("mode", "")
        interval = _sample_interval_human(p.get("sample_interval_ns", 0))
        models = _models_cell(p)
        vendors = _vendor_cell(p)
        optional = "yes" if p.get("optional") else "no"
        lines.append(
            f"| {_path_display(path_str)} | {origin} | {mode} | {interval} | {models} | {vendors} | {optional} |"
        )

    lines.append("")

    # YANG models required
    lines.append("## YANG Models Required")
    lines.append("")

    all_models: dict[str, str] = {}
    for p in paths:
        for m in p.get("required_models", []):
            if m not in all_models:
                vendor_note = _vendor_cell(p)
                all_models[m] = vendor_note
        for m in p.get("required_any_models", []):
            if m not in all_models:
                vendor_note = _vendor_cell(p)
                all_models[m] = f"{vendor_note} (any-of)"

    if all_models:
        lines.append("| Model | Vendor scope |")
        lines.append("|-------|-------------|")
        for model, vendor_note in sorted(all_models.items()):
            lines.append(f"| `{model}` | {vendor_note} |")
        lines.append("")
    else:
        lines.append("No YANG model constraints declared — all paths are unconditional.")
        lines.append("")

    # Fallback / vendor-native notes
    native_with_fallback = [p for p in paths if p.get("fallback_for")]
    if native_with_fallback:
        lines.append("## Vendor-Native Fallbacks")
        lines.append("")
        for p in native_with_fallback:
            vendor = ", ".join(p.get("vendor_only", ["unknown vendor"]))
            lines.append(
                f"- **{vendor}** `{p['path']}` falls back for `{p['fallback_for']}` "
                f"when the preferred OpenConfig model is not advertised."
            )
        lines.append("")

    # Path rationales
    lines.append("## Path Rationales")
    lines.append("")
    for p in paths:
        rationale_text = p.get("rationale", "")
        if rationale_text:
            origin_tag = f"[{p.get('origin') or 'native'}] " if p.get("origin") else "[native] "
            lines.append(f"- **`{p['path']}`** {origin_tag}— {rationale_text}")
    lines.append("")

    # Known gaps placeholder
    lines.append("## Known Gaps")
    lines.append("")
    lines.append("<!-- Add known gaps, vendor quirks, or lab-verification notes here. -->")
    lines.append("")

    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--force", action="store_true", help="Overwrite existing docs")
    parser.add_argument("--profile", help="Only process this profile name")
    parser.add_argument("--dry-run", action="store_true", help="Print without writing")
    args = parser.parse_args()

    DOCS_DIR.mkdir(parents=True, exist_ok=True)

    profiles = load_profiles(PROFILES_DIR)
    if not profiles:
        print(f"no profiles found in {PROFILES_DIR}", file=sys.stderr)
        sys.exit(1)

    if args.profile:
        profiles = [p for p in profiles if p["name"] == args.profile]
        if not profiles:
            print(f"profile '{args.profile}' not found", file=sys.stderr)
            sys.exit(1)

    generated = skipped = 0
    for profile in profiles:
        name = profile["name"]
        doc_path = DOCS_DIR / f"{name}.md"

        if doc_path.exists() and not args.force:
            print(f"skip   {name}.md  (exists; use --force to overwrite)")
            skipped += 1
            continue

        doc = generate_doc(profile)

        if args.dry_run:
            print(f"--- {doc_path} ---")
            print(doc)
            print()
        else:
            doc_path.write_text(doc)
            action = "updated" if doc_path.exists() else "created"
            print(f"write  {name}.md  ({len(profile.get('paths', []))} paths)")
            generated += 1

    if not args.dry_run:
        print(f"\ndone: {generated} written, {skipped} skipped")


if __name__ == "__main__":
    main()

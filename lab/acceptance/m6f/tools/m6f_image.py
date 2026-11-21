#!/usr/bin/env python3
"""m6f_image.py — deterministic container-image resolution for M6F archives.

Solves the "which image did this archive import?" problem without ever
consulting a global "newest image" listing:

  * Reads the docker-save ``manifest.json`` inside the archive (and inside
    a single nested docker archive, e.g. the XRd wrapper .tgz) to learn
    the exact RepoTags / RepoDigests / image config ID the archive
    carries.
  * Checks ``docker image inspect`` ONLY for those archive-declared
    references — never ``docker images`` sorted by creation time.
  * Reports ``present`` (compatible image already imported → skip
    ``docker load`` entirely), ``needs_load`` (with the exact loadable
    member path), ``ambiguous`` (multiple distinct images → operator
    must select explicitly) or ``error``.

Commands:
  inspect  --archive PATH [--workdir DIR]
      Print JSON describing the archive's docker image(s) and the
      loadable member path. Read-only; extracts nested members into
      --workdir when required.
  resolve  --archive PATH [--select REF] [--workdir DIR]
      Full pre-load resolution. Prints JSON:
        {status: present|needs_load|ambiguous|error, image_ref,
         image_id, repo_digests, load_path, candidates, reason}
  identify --archive PATH --ref REF
      Post-``docker load`` identification: inspect REF and print
      {image_ref, image_id, repo_tags, repo_digests}.

All docker interaction goes through the ``docker`` CLI on PATH so tests
can substitute a fake.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile

NESTED_SUFFIXES = (".tgz", ".tar.gz", ".tar.xz", ".tar.bz2", ".tar")
MAX_NESTING = 2


def _docker(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["docker", *args], capture_output=True, text=True, timeout=120
    )


def _docker_inspect(ref: str):
    """Return image inspect dict or None."""
    r = _docker("image", "inspect", ref)
    if r.returncode != 0:
        return None
    try:
        data = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    return data[0] if isinstance(data, list) and data else None


def _read_manifest(path: str):
    """Return parsed docker-save manifest.json list, or None."""
    try:
        with tarfile.open(path, "r:*") as t:
            try:
                member = t.getmember("manifest.json")
            except KeyError:
                return None
            f = t.extractfile(member)
            if f is None:
                return None
            return json.load(f)
    except (tarfile.TarError, OSError, json.JSONDecodeError):
        return None


def _nested_candidates(path: str):
    """Member names that could be a nested docker archive."""
    try:
        with tarfile.open(path, "r:*") as t:
            return [
                m.name
                for m in t.getmembers()
                if m.isfile() and m.name.lower().endswith(NESTED_SUFFIXES)
            ]
    except (tarfile.TarError, OSError):
        return []


def _extract_member(path: str, member_name: str, workdir: str) -> str:
    os.makedirs(workdir, exist_ok=True)
    base = os.path.basename(member_name)
    out = os.path.join(workdir, base)
    with tarfile.open(path, "r:*") as t:
        member = t.getmember(member_name)
        src = t.extractfile(member)
        if src is None:
            raise RuntimeError(f"cannot read member {member_name}")
        with open(out, "wb") as dst:
            shutil.copyfileobj(src, dst, length=1 << 22)
    return out


def inspect_archive(archive: str, workdir: str, depth: int = 0):
    """Locate docker-save manifest.json, descending into nested archives.

    Returns dict {ok, format, images, load_path, nested_member, error}.
    """
    manifest = _read_manifest(archive)
    if manifest is not None:
        images = []
        for entry in manifest:
            images.append(
                {
                    "config": entry.get("Config"),
                    "repo_tags": entry.get("RepoTags") or [],
                    "repo_digests": entry.get("RepoDigests") or [],
                }
            )
        return {
            "ok": True,
            "format": "docker-save",
            "archive": archive,
            "images": images,
            "load_path": archive,
            "nested_member": None,
        }
    if depth >= MAX_NESTING:
        return {
            "ok": False,
            "format": "unknown",
            "archive": archive,
            "images": [],
            "load_path": None,
            "nested_member": None,
            "error": "no docker-save manifest.json found",
        }
    candidates = _nested_candidates(archive)
    if len(candidates) != 1:
        return {
            "ok": False,
            "format": "unknown",
            "archive": archive,
            "images": [],
            "load_path": None,
            "nested_member": None,
            "error": (
                "archive has no docker-save manifest.json and does not "
                f"contain exactly one nested docker archive "
                f"(found {len(candidates)}: {candidates[:8]})"
            ),
        }
    member = candidates[0]
    inner = _extract_member(archive, member, workdir)
    sub = inspect_archive(inner, workdir, depth + 1)
    sub["outer_archive"] = archive
    sub["nested_member"] = member
    return sub


def _image_refs(image: dict) -> list:
    """All inspectable references for one archive image entry."""
    refs = []
    for tag in image.get("repo_tags") or []:
        if tag:
            refs.append(tag)
    for digest in image.get("repo_digests") or []:
        if digest:
            refs.append(digest)
    cfg = image.get("config") or ""
    if cfg.endswith(".json"):
        refs.append(cfg[: -len(".json")])
    # de-dup preserving order
    seen, out = set(), []
    for r in refs:
        if r not in seen:
            seen.add(r)
            out.append(r)
    return out


def _pick_ref(refs: list) -> str:
    """Deterministic single-image reference choice.

    Prefer a digest-pinned reference, then a tagged reference over an
    untagged one, then lexicographic order. Never time- or
    recency-based.
    """
    digest_refs = [r for r in refs if "@" in r]
    tagged = [r for r in refs if ":" in r.split("@", 1)[0] and "@" not in r]
    pool = digest_refs or tagged or refs
    return sorted(pool)[0]


def cmd_inspect(args) -> int:
    res = inspect_archive(args.archive, args.workdir)
    print(json.dumps(res, indent=2))
    return 0 if res.get("ok") else 1


def cmd_resolve(args) -> int:
    info = inspect_archive(args.archive, args.workdir)
    if not info.get("ok"):
        print(
            json.dumps(
                {
                    "status": "error",
                    "reason": info.get("error", "archive inspection failed"),
                }
            )
        )
        return 1
    images = info["images"]
    if not images:
        print(json.dumps({"status": "error", "reason": "archive has no images"}))
        return 1

    # Group archive entries by image config id — multiple RepoTags for
    # the SAME image are fine; multiple distinct images need selection.
    by_config = {}
    for img in images:
        by_config.setdefault(img.get("config") or "", []).append(img)

    selected_img = None
    if args.select:
        for img in images:
            if args.select in _image_refs(img):
                selected_img = img
                break
        if selected_img is None:
            all_refs = [r for img in images for r in _image_refs(img)]
            print(
                json.dumps(
                    {
                        "status": "error",
                        "reason": (
                            f"--select '{args.select}' does not match any "
                            "reference carried by this archive"
                        ),
                        "candidates": sorted(all_refs),
                    }
                )
            )
            return 1
    elif len(by_config) == 1:
        # One image, possibly several tags — deterministic pick.
        refs = []
        for img in images:
            refs.extend(_image_refs(img))
        selected_img = dict(images[0])
        selected_img["_all_refs"] = sorted(set(refs))
    else:
        candidates = []
        for img in images:
            candidates.extend(_image_refs(img))
        print(
            json.dumps(
                {
                    "status": "ambiguous",
                    "reason": (
                        "archive carries multiple distinct images; set "
                        "manifest image_select or pass --select explicitly"
                    ),
                    "candidates": sorted(set(candidates)),
                }
            )
        )
        return 1

    # Find a locally present image by inspecting ONLY archive refs.
    present = None
    for ref in _image_refs(selected_img) + selected_img.get("_all_refs", []):
        insp = _docker_inspect(ref)
        if insp:
            present = (ref, insp)
            break
    if present:
        ref, insp = present
        print(
            json.dumps(
                {
                    "status": "present",
                    "image_ref": ref,
                    "image_id": insp.get("Id"),
                    "repo_tags": insp.get("RepoTags") or [],
                    "repo_digests": insp.get("RepoDigests") or [],
                    "load_path": None,
                    "candidates": selected_img.get("_all_refs")
                    or sorted(set(_image_refs(selected_img))),
                },
                indent=2,
            )
        )
        return 0

    chosen = _pick_ref(
        selected_img.get("_all_refs") or _image_refs(selected_img)
    )
    print(
        json.dumps(
            {
                "status": "needs_load",
                "image_ref": chosen,
                "image_id": None,
                "repo_digests": [],
                "load_path": info["load_path"],
                "nested_member": info.get("nested_member"),
                "outer_archive": info.get("outer_archive"),
                "candidates": selected_img.get("_all_refs")
                or sorted(set(_image_refs(selected_img))),
            },
            indent=2,
        )
    )
    return 0


def cmd_identify(args) -> int:
    insp = _docker_inspect(args.ref)
    if not insp:
        print(
            json.dumps(
                {"status": "error", "reason": f"image not found: {args.ref}"}
            )
        )
        return 1
    tags = insp.get("RepoTags") or []
    print(
        json.dumps(
            {
                "status": "loaded",
                "image_ref": args.ref,
                "canonical_ref": tags[0] if tags else args.ref,
                "image_id": insp.get("Id"),
                "repo_tags": tags,
                "repo_digests": insp.get("RepoDigests") or [],
            },
            indent=2,
        )
    )
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("inspect", "resolve"):
        p = sub.add_parser(name)
        p.add_argument("--archive", required=True)
        p.add_argument("--select", default="")
        p.add_argument("--workdir", default=tempfile.gettempdir())
        p.set_defaults(fn=cmd_inspect if name == "inspect" else cmd_resolve)
    p = sub.add_parser("identify")
    p.add_argument("--archive", required=True)
    p.add_argument("--ref", required=True)
    p.set_defaults(fn=cmd_identify)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

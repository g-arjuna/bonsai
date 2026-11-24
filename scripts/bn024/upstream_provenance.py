#!/usr/bin/env python3
"""BN-024-00 upstream provenance manifest validator/verifier.

Two offline-capable operations on a provenance manifest such as
``docs/bn024/provenance/upstream-sources.json``:

    validate <manifest.json>          schema + policy validation (no network)
    verify   <manifest.json> <dir>    recompute content hashes against local
                                      checkouts of the pinned revisions

``verify`` expects ``<dir>`` to contain one subdirectory per upstream
repository, named after the repository basename, e.g.::

    <dir>/suzieq/            checkout of netenglabs/suzieq @ pinned revision
    <dir>/ntc-templates/     checkout of networktocode/ntc-templates
    <dir>/textfsm/           checkout of google/textfsm

For ``content_kind: "tree"`` entries the hash is computed over the sorted
``relpath\\0sha256(file)\\n`` lines of every regular file under the
upstream path — the same algorithm used to generate the manifest.

Exit status: 0 = all checks pass, 1 = validation/verification failures,
2 = usage error.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys

MANIFEST_VERSION = 1

USAGE_CLASSES = {
    "behavioral_reference_only",
    "direct_reuse_permitted",
    "modified_translation_reuse",
    "independent_implementation",
    "test_fixture_redistribution",
    "local_only_oracle_material",
    "unclear_prohibited_pending_review",
}

REUSE_CLASSES = {
    "direct_reuse_permitted",
    "modified_translation_reuse",
    "test_fixture_redistribution",
}

REVIEW_STATUSES = {
    "pending_review",
    "approved_reference",
    "approved_reuse",
    "rejected",
}

MODIFICATION_STATUSES = {"not_vendored", "none", "modified", "translated"}

CONTENT_KINDS = {"file", "tree"}

REQUIRED_FIELDS = [
    "source_id",
    "upstream_repository",
    "immutable_revision",
    "upstream_path",
    "content_sha256",
    "content_kind",
    "license_identifier",
    "license_evidence",
    "required_notice",
    "usage_classification",
    "local_destination",
    "modification_status",
    "review_status",
]

_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_REVISION_RE = re.compile(r"^[0-9a-f]{40}$")
_LICENSE_RE = re.compile(r"^[A-Za-z0-9.+\- ]+$")


def _sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _tree_hash(root: str, rel: str) -> str:
    base = os.path.join(root, rel)
    entries: list[tuple[str, str]] = []
    for dirpath, dirnames, files in os.walk(base):
        dirnames.sort()
        for name in sorted(files):
            full = os.path.join(dirpath, name)
            if not os.path.isfile(full) or os.path.islink(full):
                continue
            r = os.path.relpath(full, base)
            entries.append((r, _sha256_file(full)))
    entries.sort(key=lambda item: item[0])
    h = hashlib.sha256()
    for r, digest in entries:
        h.update(r.encode())
        h.update(b"\0")
        h.update(digest.encode())
        h.update(b"\n")
    return h.hexdigest()


def _repo_dirname(repo_url: str) -> str:
    return repo_url.rstrip("/").rsplit("/", 1)[-1]


def _check_path_containment(repo_dir: str, upstream_path: str) -> str | None:
    """Return an error string if upstream_path escapes repo_dir, else None."""
    base = os.path.realpath(repo_dir)
    target = os.path.realpath(os.path.join(base, upstream_path))
    if target != base and not target.startswith(base + os.sep):
        return f"upstream_path '{upstream_path}' escapes repository checkout"
    return None


def validate(manifest: dict) -> list[str]:
    errors: list[str] = []
    if manifest.get("manifest_version") != MANIFEST_VERSION:
        errors.append(f"manifest_version must be {MANIFEST_VERSION}")
    sources = manifest.get("sources")
    if not isinstance(sources, list) or not sources:
        errors.append("sources must be a non-empty list")
        return errors

    seen_ids: set[str] = set()
    for i, src in enumerate(sources):
        where = src.get("source_id", f"sources[{i}]") if isinstance(src, dict) else f"sources[{i}]"
        if not isinstance(src, dict):
            errors.append(f"{where}: entry is not an object")
            continue
        for field in REQUIRED_FIELDS:
            if field not in src:
                errors.append(f"{where}: missing required field '{field}'")
        for field in ("source_id", "upstream_repository", "upstream_path",
                      "license_identifier", "license_evidence", "required_notice"):
            if field in src and not src[field]:
                errors.append(f"{where}: '{field}' must be non-empty")

        sid = src.get("source_id")
        if sid in seen_ids:
            errors.append(f"{where}: duplicate source_id")
        seen_ids.add(sid)

        if "immutable_revision" in src and not _REVISION_RE.match(str(src["immutable_revision"])):
            errors.append(f"{where}: immutable_revision must be 40 lowercase hex chars")
        if "content_sha256" in src and not _SHA256_RE.match(str(src["content_sha256"])):
            errors.append(f"{where}: content_sha256 must be 64 lowercase hex chars")
        if src.get("content_kind") not in CONTENT_KINDS:
            errors.append(f"{where}: content_kind must be one of {sorted(CONTENT_KINDS)}")
        if src.get("usage_classification") not in USAGE_CLASSES:
            errors.append(f"{where}: usage_classification must be one of {sorted(USAGE_CLASSES)}")
        if src.get("review_status") not in REVIEW_STATUSES:
            errors.append(f"{where}: review_status must be one of {sorted(REVIEW_STATUSES)}")
        if src.get("modification_status") not in MODIFICATION_STATUSES:
            errors.append(f"{where}: modification_status must be one of {sorted(MODIFICATION_STATUSES)}")
        if "license_identifier" in src and not _LICENSE_RE.match(str(src["license_identifier"])):
            errors.append(f"{where}: license_identifier has unexpected characters")

        usage = src.get("usage_classification")
        review = src.get("review_status")
        if usage in REUSE_CLASSES and review != "approved_reuse":
            errors.append(
                f"{where}: usage_classification '{usage}' requires "
                f"review_status 'approved_reuse' (got '{review}') — unapproved reuse is prohibited"
            )
        if usage in {"behavioral_reference_only", "local_only_oracle_material",
                     "unclear_prohibited_pending_review"} and review == "approved_reuse":
            errors.append(f"{where}: review_status 'approved_reuse' inconsistent with usage '{usage}'")
        if usage == "unclear_prohibited_pending_review" and review == "approved_reference":
            errors.append(f"{where}: unclear assets cannot be approved_reference")

        path = src.get("upstream_path")
        if isinstance(path, str) and path:
            parts = path.split("/")
            if path.startswith("/") or "\x00" in path or any(
                p in ("", ".", "..") for p in parts
            ):
                errors.append(
                    f"{where}: upstream_path must be a clean relative path "
                    "(no absolute paths, '.', '..' or empty segments)"
                )
        dest = src.get("local_destination")
        if dest is not None:
            if not isinstance(dest, str) or not dest:
                errors.append(f"{where}: local_destination must be null or a non-empty relative path")
            elif dest.startswith("/") or ".." in dest.split("/"):
                errors.append(f"{where}: local_destination must be a safe relative path")
    return errors


def _git_head(repo_dir: str) -> str | None:
    """Return HEAD revision of a git checkout, or None if not a git repo."""
    import subprocess

    if not (os.path.isdir(os.path.join(repo_dir, ".git"))
            or os.path.isfile(os.path.join(repo_dir, ".git"))):
        return None
    try:
        out = subprocess.run(
            ["git", "-C", repo_dir, "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip() or None


def verify(manifest: dict, checkout_root: str, require_revision: bool = True) -> list[str]:
    errors: list[str] = []
    revision_checked: set[str] = set()
    for src in manifest.get("sources", []):
        sid = src.get("source_id", "?")
        repo_url = src.get("upstream_repository", "")
        repo_dir = os.path.join(checkout_root, _repo_dirname(repo_url))
        upstream_path = src.get("upstream_path", "")
        if not os.path.isdir(repo_dir):
            errors.append(f"{sid}: no checkout for {repo_url} at {repo_dir}")
            continue

        # Verify the checkout is actually at the pinned immutable revision —
        # content hashes alone cannot prove revision identity.
        if repo_dir not in revision_checked:
            revision_checked.add(repo_dir)
            head = _git_head(repo_dir)
            if head is None:
                if require_revision:
                    errors.append(
                        f"{sid}: checkout {repo_dir} is not a git repository; "
                        "cannot verify pinned immutable_revision"
                    )
            elif head != src.get("immutable_revision"):
                errors.append(
                    f"{sid}: checkout {repo_dir} HEAD {head[:12]}… does not "
                    f"match pinned revision {str(src.get('immutable_revision'))[:12]}…"
                )

        containment = _check_path_containment(repo_dir, upstream_path)
        if containment:
            errors.append(f"{sid}: {containment}")
            continue

        target = os.path.join(repo_dir, upstream_path)
        if src["content_kind"] == "file":
            if not os.path.isfile(target):
                errors.append(f"{sid}: missing file {target}")
                continue
            actual = _sha256_file(target)
        else:
            if not os.path.isdir(target):
                errors.append(f"{sid}: missing tree {target}")
                continue
            actual = _tree_hash(repo_dir, src["upstream_path"])
        if actual != src["content_sha256"]:
            errors.append(f"{sid}: hash mismatch for {src['upstream_path']} "
                          f"(manifest {src['content_sha256'][:16]}…, actual {actual[:16]}…)")
    return errors


def _load(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def main(argv: list[str]) -> int:
    if len(argv) < 2 or argv[1] not in {"validate", "verify"}:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        print("usage: upstream_provenance.py validate <manifest> | verify <manifest> <checkout-dir>",
              file=sys.stderr)
        return 2
    cmd = argv[1]
    if cmd == "validate" and len(argv) == 3:
        errors = validate(_load(argv[2]))
    elif cmd == "verify" and len(argv) == 4:
        manifest = _load(argv[2])
        errors = validate(manifest)
        if not errors:
            errors = verify(manifest, argv[3])
    else:
        print("usage: upstream_provenance.py validate <manifest> | verify <manifest> <checkout-dir>",
              file=sys.stderr)
        return 2
    if errors:
        for e in errors:
            print(f"FAIL: {e}")
        print(f"{len(errors)} problem(s)")
        return 1
    print("OK" if cmd == "validate" else f"OK ({len(_load(argv[2])['sources'])} entries verified)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

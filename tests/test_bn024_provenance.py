"""BN-024-00 — upstream provenance manifest validation tests.

Offline tests for ``scripts/bn024/upstream_provenance.py`` and the
committed manifest ``docs/bn024/provenance/upstream-sources.json``.
No network access required.
"""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "bn024" / "upstream_provenance.py"
MANIFEST = REPO_ROOT / "docs" / "bn024" / "provenance" / "upstream-sources.json"

spec = importlib.util.spec_from_file_location("upstream_provenance", SCRIPT)
up = importlib.util.module_from_spec(spec)
sys.modules["upstream_provenance"] = up
spec.loader.exec_module(up)

PINNED_REVISION = "75198b5585ebef91f0659b6b1ec3c964cc2cd1ad"


def _manifest() -> dict:
    return json.loads(MANIFEST.read_text())


def test_committed_manifest_validates() -> None:
    errors = up.validate(_manifest())
    assert errors == []


def test_manifest_pins_immutable_suzieq_revision() -> None:
    manifest = _manifest()
    suzieq = [s for s in manifest["sources"]
              if s["upstream_repository"] == "https://github.com/netenglabs/suzieq"]
    assert suzieq, "manifest must contain SuzieQ OSS entries"
    for src in suzieq:
        assert src["immutable_revision"] == PINNED_REVISION
        assert re_fullmatch(src["content_sha256"])


def re_fullmatch(sha: str) -> bool:
    import re
    return re.fullmatch(r"[0-9a-f]{64}", sha) is not None


def test_manifest_covers_required_asset_families() -> None:
    paths = {s["upstream_path"] for s in _manifest()["sources"]}
    required_prefixes = (
        "suzieq/config/interfaces.yml",
        "suzieq/config/bgp.yml",
        "suzieq/config/routes.yml",
        "suzieq/config/lldp.yml",
        "suzieq/config/arpnd.yml",
        "suzieq/config/macs.yml",
        "suzieq/config/evpnVni.yml",
        "suzieq/config/mlag.yml",
        "suzieq/config/schema/",
        "suzieq/poller/worker/services/",
        "suzieq/engines/",
        "LICENSE",
    )
    for prefix in required_prefixes:
        assert any(p == prefix or p.startswith(prefix) for p in paths), prefix


def test_no_private_key_or_secret_material_scoped() -> None:
    for src in _manifest()["sources"]:
        assert "etc/" not in src["upstream_path"], (
            "suzieq/config/etc contains upstream test private keys — excluded"
        )
        assert not src["upstream_path"].endswith((".pem", ".key"))


@pytest.mark.parametrize(
    "mutate,needle",
    [
        (lambda m: m["sources"][0].pop("required_notice"), "missing required field 'required_notice'"),
        (lambda m: m["sources"][0].update(content_sha256="zz"), "content_sha256 must be 64"),
        (lambda m: m["sources"][0].update(immutable_revision="main"), "immutable_revision must be 40"),
        (lambda m: m["sources"][0].update(usage_classification="bogus"), "usage_classification must be one of"),
        (lambda m: m["sources"][0].update(review_status="bogus"), "review_status must be one of"),
    ],
)
def test_validator_rejects_malformed_entries(mutate, needle: str) -> None:
    manifest = _manifest()
    mutate(manifest)
    errors = up.validate(manifest)
    assert errors, "validator must reject malformed manifest"
    assert any(needle in e for e in errors)


def test_validator_rejects_unapproved_reuse() -> None:
    """Negative fixture: a reuse classification without approved_reuse review."""
    manifest = _manifest()
    target = next(s for s in manifest["sources"]
                  if s["source_id"] == "suzieq-oss:suzieq/config/interfaces.yml")
    target["usage_classification"] = "modified_translation_reuse"
    target["review_status"] = "pending_review"
    errors = up.validate(manifest)
    assert any("requires" in e and "approved_reuse" in e for e in errors)


def test_validator_rejects_missing_notice_on_reuse() -> None:
    """Negative fixture: reuse-approved entry with empty required_notice."""
    manifest = _manifest()
    target = next(s for s in manifest["sources"]
                  if s["source_id"] == "suzieq-oss:suzieq/config/interfaces.yml")
    target["usage_classification"] = "direct_reuse_permitted"
    target["review_status"] = "approved_reuse"
    target["required_notice"] = ""
    errors = up.validate(manifest)
    assert any("required_notice" in e for e in errors)


def test_validator_rejects_duplicate_source_id() -> None:
    manifest = _manifest()
    manifest["sources"].append(copy.deepcopy(manifest["sources"][0]))
    errors = up.validate(manifest)
    assert any("duplicate source_id" in e for e in errors)


def test_tree_hash_is_deterministic_and_content_sensitive(tmp_path: Path) -> None:
    root = tmp_path / "fake"
    (root / "sub").mkdir(parents=True)
    (root / "sub" / "a.txt").write_text("alpha")
    (root / "b.txt").write_text("beta")
    h1 = up._tree_hash(str(tmp_path), "fake")
    h2 = up._tree_hash(str(tmp_path), "fake")
    assert h1 == h2
    (root / "b.txt").write_text("beta2")
    assert up._tree_hash(str(tmp_path), "fake") != h1


def test_verify_detects_tampered_file(tmp_path: Path) -> None:
    root = tmp_path / "acme-widget"
    root.mkdir()
    target = root / "data.txt"
    target.write_text("original")
    sha = hashlib.sha256(b"original").hexdigest()
    manifest = {
        "manifest_version": 1,
        "sources": [{
            "source_id": "acme:data",
            "upstream_repository": "https://example.com/acme-widget",
            "immutable_revision": "0" * 40,
            "upstream_path": "data.txt",
            "content_sha256": sha,
            "content_kind": "file",
            "license_identifier": "Apache-2.0",
            "license_evidence": "test",
            "required_notice": "test",
            "usage_classification": "behavioral_reference_only",
            "local_destination": None,
            "modification_status": "not_vendored",
            "review_status": "pending_review",
        }],
    }
    assert up.verify(manifest, str(tmp_path), require_revision=False) == []
    target.write_text("tampered")
    errors = up.verify(manifest, str(tmp_path), require_revision=False)
    assert any("hash mismatch" in e for e in errors)


def _fake_manifest_entry(repo_name: str, revision: str) -> dict:
    return {
        "manifest_version": 1,
        "sources": [{
            "source_id": f"acme:{repo_name}",
            "upstream_repository": f"https://example.com/{repo_name}",
            "immutable_revision": revision,
            "upstream_path": "data.txt",
            "content_sha256": hashlib.sha256(b"x").hexdigest(),
            "content_kind": "file",
            "license_identifier": "Apache-2.0",
            "license_evidence": "test",
            "required_notice": "test",
            "usage_classification": "behavioral_reference_only",
            "local_destination": None,
            "modification_status": "not_vendored",
            "review_status": "pending_review",
        }],
    }


def _git(repo: Path, *args: str) -> None:
    import subprocess
    subprocess.run(["git", "-C", str(repo), *args], check=True,
                   capture_output=True)


def test_verify_requires_git_revision_identity(tmp_path: Path) -> None:
    """A non-git checkout cannot prove it is at the pinned revision."""
    repo = tmp_path / "acme-widget"
    repo.mkdir()
    (repo / "data.txt").write_text("x")
    manifest = _fake_manifest_entry("acme-widget", "0" * 40)
    errors = up.verify(manifest, str(tmp_path))
    assert any("not a git repository" in e for e in errors)


def test_verify_detects_wrong_revision(tmp_path: Path) -> None:
    import subprocess
    repo = tmp_path / "acme-widget"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "-c", "user.email=t@t", "-c", "user.name=t",
         "commit", "-q", "--allow-empty", "-m", "init")
    head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True
                          ).stdout.strip()
    manifest = _fake_manifest_entry("acme-widget", "0" * 40)
    errors = up.verify(manifest, str(tmp_path))
    assert any("does not match pinned revision" in e for e in errors)
    manifest["sources"][0]["immutable_revision"] = head
    manifest["sources"][0]["content_sha256"] = hashlib.sha256(
        (repo / "data.txt").read_bytes() if (repo / "data.txt").exists()
        else b"").hexdigest()
    (repo / "data.txt").write_text("x")
    manifest["sources"][0]["content_sha256"] = hashlib.sha256(b"x").hexdigest()
    assert up.verify(manifest, str(tmp_path)) == []


def test_verify_rejects_path_escape(tmp_path: Path) -> None:
    """upstream_path containing '..' must not read outside the checkout."""
    import subprocess
    repo = tmp_path / "acme-widget"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "-c", "user.email=t@t", "-c", "user.name=t",
         "commit", "-q", "--allow-empty", "-m", "init")
    head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                          capture_output=True, text=True, check=True
                          ).stdout.strip()
    (tmp_path / "secret.txt").write_text("topsecret")
    manifest = _fake_manifest_entry("acme-widget", head)
    manifest["sources"][0]["upstream_path"] = "../secret.txt"
    errors = up.verify(manifest, str(tmp_path))
    assert any("escapes repository checkout" in e for e in errors)


def test_validate_rejects_dirty_upstream_path() -> None:
    manifest = _manifest()
    manifest["sources"][0]["upstream_path"] = "../etc/passwd"
    errors = up.validate(manifest)
    assert any("clean relative path" in e for e in errors)


def test_tree_hash_sorts_across_directories(tmp_path: Path) -> None:
    """Tree hash must not depend on filesystem directory iteration order."""
    root = tmp_path / "fake"
    (root / "zdir").mkdir(parents=True)
    (root / "adir").mkdir(parents=True)
    (root / "zdir" / "f.txt").write_text("z")
    (root / "adir" / "f.txt").write_text("a")
    h1 = up._tree_hash(str(tmp_path), "fake")
    # Reverse creation order must not change the digest.
    root2 = tmp_path / "fake2"
    (root2 / "adir").mkdir(parents=True)
    (root2 / "zdir").mkdir(parents=True)
    (root2 / "adir" / "f.txt").write_text("a")
    (root2 / "zdir" / "f.txt").write_text("z")
    h2 = up._tree_hash(str(tmp_path), "fake2")
    assert h1 == h2

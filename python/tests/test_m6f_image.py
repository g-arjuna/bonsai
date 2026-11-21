"""Deterministic tests for lab/acceptance/m6f/tools/m6f_image.py.

Builds synthetic docker-save tarballs and a fake ``docker`` CLI so the
archive→image resolution logic is exercised without touching real
images. The critical invariant: selection comes ONLY from references
declared inside the archive's own manifest.json — never from a global
"newest image" listing.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
import stat
import tarfile
import textwrap
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
TOOL = REPO / "lab/acceptance/m6f/tools/m6f_image.py"

spec = importlib.util.spec_from_file_location("m6f_image", TOOL)
m6f_image = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m6f_image)


def _docker_save(path: Path, images: list[dict]):
    """Write a minimal docker-save-format tarball.

    images: [{config, repo_tags, repo_digests}]
    """
    with tarfile.open(path, "w") as t:
        manifest = []
        for i, img in enumerate(images):
            cfg_name = img.get("config") or f"{i:064x}.json"
            cfg = tarfile.TarInfo(cfg_name)
            payload = json.dumps({"architecture": "amd64"}).encode()
            cfg.size = len(payload)
            t.addfile(cfg, io.BytesIO(payload))
            layer = tarfile.TarInfo(f"layer{i}.tar")
            layer.size = 0
            t.addfile(layer)
            manifest.append(
                {
                    "Config": cfg_name,
                    "RepoTags": img.get("repo_tags") or [],
                    "RepoDigests": img.get("repo_digests") or [],
                    "Layers": [f"layer{i}.tar"],
                }
            )
        m = json.dumps(manifest).encode()
        mi = tarfile.TarInfo("manifest.json")
        mi.size = len(m)
        t.addfile(mi, io.BytesIO(m))


def _nested_wrapper(path: Path, inner_name: str, inner_path: Path):
    """Outer tarball containing one nested docker archive member."""
    with tarfile.open(path, "w:gz") as t:
        data = inner_path.read_bytes()
        ti = tarfile.TarInfo(f"./{inner_name}")
        ti.size = len(data)
        t.addfile(ti, io.BytesIO(data))
        ti2 = tarfile.TarInfo("./README.txt")
        payload = b"wrapper\n"
        ti2.size = len(payload)
        t.addfile(ti2, io.BytesIO(payload))


@pytest.fixture()
def fake_docker(tmp_path, monkeypatch):
    """Install a fake ``docker`` on PATH.

    FAKE_DOCKER_JSON maps inspect ref -> inspect object (or absent =
    'no such image'). Every invocation is appended to FAKE_DOCKER_LOG.
    """
    bindir = tmp_path / "bin"
    bindir.mkdir()
    imgmap = tmp_path / "images.json"
    imgmap.write_text("{}")
    log = tmp_path / "docker.log"
    script = bindir / "docker"
    script.write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env python3
            import json, os, sys
            log = os.environ["FAKE_DOCKER_LOG"]
            with open(log, "a") as f:
                f.write(" ".join(sys.argv[1:]) + "\\n")
            if sys.argv[1:3] == ["image", "inspect"]:
                ref = sys.argv[3]
                imgs = json.load(open(os.environ["FAKE_DOCKER_JSON"]))
                if ref in imgs:
                    print(json.dumps([imgs[ref]]))
                    sys.exit(0)
                sys.stderr.write("Error: No such image: " + ref + "\\n")
                sys.exit(1)
            sys.stderr.write("fake docker: unsupported " + " ".join(sys.argv[1:]) + "\\n")
            sys.exit(2)
            """
        )
    )
    script.chmod(script.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    monkeypatch.setenv("PATH", f"{bindir}:{os.environ['PATH']}")
    monkeypatch.setenv("FAKE_DOCKER_JSON", str(imgmap))
    monkeypatch.setenv("FAKE_DOCKER_LOG", str(log))
    return {"imgmap": imgmap, "log": log, "tmp": tmp_path}


def _set_images(fake, mapping):
    fake["imgmap"].write_text(json.dumps(mapping))


def test_flat_archive_needs_load_deterministic(fake_docker, tmp_path):
    arch = tmp_path / "vendor.tar"
    _docker_save(
        arch,
        [
            {
                "config": "a" * 64 + ".json",
                "repo_tags": ["vendor/nos:1.0", "vendor/nos:latest"],
                "repo_digests": [],
            }
        ],
    )
    res = m6f_image.inspect_archive(str(arch), str(tmp_path / "w"))
    assert res["ok"] and res["format"] == "docker-save"
    assert res["images"][0]["repo_tags"] == ["vendor/nos:1.0", "vendor/nos:latest"]

    # resolve with no local image → needs_load, deterministic ref
    import io, contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        args = argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"), select="")
        rc = m6f_image.cmd_resolve(args)
    assert rc == 0
    out = json.loads(buf.getvalue())
    assert out["status"] == "needs_load"
    # deterministic: tagged refs preferred over the bare config id,
    # lexicographically first
    assert out["image_ref"] in {"vendor/nos:1.0", "vendor/nos:latest"}
    assert out["load_path"] == str(arch)


def test_flat_archive_present_skips_load(fake_docker, tmp_path):
    arch = tmp_path / "vendor.tar"
    _docker_save(
        arch,
        [{"config": "b" * 64 + ".json",
          "repo_tags": ["vendor/nos:2.0"], "repo_digests": []}],
    )
    _set_images(fake_docker, {
        "vendor/nos:2.0": {"Id": "sha256:img1",
                           "RepoTags": ["vendor/nos:2.0"],
                           "RepoDigests": []}
    })
    import io, contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"), select=""))
    assert rc == 0
    out = json.loads(buf.getvalue())
    assert out["status"] == "present"
    assert out["image_id"] == "sha256:img1"
    assert out["load_path"] is None
    # selection consulted ONLY archive refs via image inspect — never a
    # global image listing
    calls = fake_docker["log"].read_text()
    assert "images" not in calls.replace("image inspect", "")
    assert "vendor/nos:2.0" in calls


def test_multiple_distinct_images_require_selection(fake_docker, tmp_path):
    arch = tmp_path / "multi.tar"
    _docker_save(
        arch,
        [
            {"config": "c" * 64 + ".json",
             "repo_tags": ["vendor/a:1"], "repo_digests": []},
            {"config": "d" * 64 + ".json",
             "repo_tags": ["vendor/b:1"], "repo_digests": []},
        ],
    )
    import io, contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"), select=""))
    assert rc == 1
    out = json.loads(buf.getvalue())
    assert out["status"] == "ambiguous"
    assert "vendor/a:1" in out["candidates"] and "vendor/b:1" in out["candidates"]

    # explicit selection of a ref belonging to the archive resolves it
    _set_images(fake_docker, {
        "vendor/b:1": {"Id": "sha256:imgB",
                       "RepoTags": ["vendor/b:1"], "RepoDigests": []}
    })
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"),
                        select="vendor/b:1"))
    assert rc == 0
    out = json.loads(buf.getvalue())
    assert out["status"] == "present" and out["image_id"] == "sha256:imgB"

    # selection of a ref NOT in the archive fails closed
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"),
                        select="unrelated/image:9"))
    assert rc == 1
    assert json.loads(buf.getvalue())["status"] == "error"


def test_nested_wrapper_archive(fake_docker, tmp_path):
    inner = tmp_path / "inner.tgz"
    _docker_save(
        inner,
        [{"config": "e" * 64 + ".json",
          "repo_tags": ["vendor/nested:3.0"], "repo_digests": []}],
    )
    outer = tmp_path / "wrapper.tgz"
    _nested_wrapper(outer, "inner.tgz", inner)
    workdir = tmp_path / "w"
    res = m6f_image.inspect_archive(str(outer), str(workdir))
    assert res["ok"]
    assert res["nested_member"] == "./inner.tgz"
    assert res["images"][0]["repo_tags"] == ["vendor/nested:3.0"]
    assert res["load_path"].endswith("inner.tgz")
    assert Path(res["load_path"]).exists()

    import io, contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(outer), workdir=str(workdir), select=""))
    assert rc == 0
    out = json.loads(buf.getvalue())
    assert out["status"] == "needs_load"
    assert out["image_ref"] == "vendor/nested:3.0"


def test_unrelated_images_never_confuse_selection(fake_docker, tmp_path):
    """Many unrelated local images must not influence the pick."""
    arch = tmp_path / "vendor.tar"
    _docker_save(
        arch,
        [{"config": "f" * 64 + ".json",
          "repo_tags": ["vendor/nos:4.0"], "repo_digests": []}],
    )
    # unrelated images exist locally (newest first in any listing) —
    # the resolver must ignore them entirely
    _set_images(fake_docker, {
        "unrelated/newest:1": {"Id": "sha256:zzz", "RepoTags": [],
                              "RepoDigests": []},
        "vendor/nos:4.0": {"Id": "sha256:img4",
                           "RepoTags": ["vendor/nos:4.0"],
                           "RepoDigests": []},
    })
    import io, contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = m6f_image.cmd_resolve(
            argparse_ns(archive=str(arch), workdir=str(tmp_path / "w"), select=""))
    assert rc == 0
    out = json.loads(buf.getvalue())
    assert out["status"] == "present"
    assert out["image_ref"] == "vendor/nos:4.0"
    assert out["image_id"] == "sha256:img4"
    calls = fake_docker["log"].read_text()
    assert "unrelated" not in calls


def test_non_archive_input_errors(tmp_path):
    bad = tmp_path / "notatar.tar"
    bad.write_bytes(b"this is not a tarball")
    res = m6f_image.inspect_archive(str(bad), str(tmp_path / "w"))
    assert res["ok"] is False


def argparse_ns(**kw):
    import argparse
    return argparse.Namespace(**kw)

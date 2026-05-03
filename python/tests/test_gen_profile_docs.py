"""Tests for scripts/gen_profile_docs.py (T1-9 / doc generation)."""
from __future__ import annotations

import importlib.util
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parent.parent.parent / "scripts" / "gen_profile_docs.py"
spec = importlib.util.spec_from_file_location("gen_profile_docs", _SCRIPT)
_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(_mod)

generate_doc = _mod.generate_doc
load_profiles = _mod.load_profiles


# ── generate_doc output shape ─────────────────────────────────────────────────

MINIMAL_PROFILE = {
    "name": "dc-spine-standard",
    "description": "DC spine standard paths",
    "rationale": "Required for fabric telemetry.",
    "paths": [
        {
            "path": "/interfaces/interface/state",
            "origin": "openconfig",
            "mode": "SAMPLE",
            "sample_interval_ns": 10_000_000_000,
            "required_models": ["openconfig-interfaces"],
            "optional": False,
        }
    ],
}


def test_generate_doc_contains_profile_name():
    doc = generate_doc(MINIMAL_PROFILE)
    assert "dc-spine-standard" in doc


def test_generate_doc_contains_description():
    doc = generate_doc(MINIMAL_PROFILE)
    assert "DC spine standard paths" in doc


def test_generate_doc_contains_rationale():
    doc = generate_doc(MINIMAL_PROFILE)
    assert "Required for fabric telemetry." in doc


def test_generate_doc_contains_path_table():
    doc = generate_doc(MINIMAL_PROFILE)
    assert "/interfaces/interface/state" in doc
    assert "openconfig" in doc
    assert "SAMPLE" in doc


def test_generate_doc_interval_formatted_as_seconds():
    doc = generate_doc(MINIMAL_PROFILE)
    # 10_000_000_000 ns = 10s
    assert "10s" in doc


def test_generate_doc_no_paths_produces_empty_table():
    profile = {**MINIMAL_PROFILE, "paths": []}
    doc = generate_doc(profile)
    assert "Subscribed Paths" in doc


def test_generate_doc_optional_path_shows_yes():
    profile = {
        "name": "test",
        "description": "test",
        "paths": [{"path": "/interfaces", "optional": True}],
    }
    doc = generate_doc(profile)
    assert "yes" in doc


# ── load_profiles from temp directory ────────────────────────────────────────

def test_load_profiles_reads_yaml_files(tmp_path):
    import yaml
    profile_data = {
        "name": "sp-p-core",
        "description": "SP core P router paths",
        "paths": [],
    }
    (tmp_path / "sp_p_core.yaml").write_text(yaml.dump(profile_data))
    profiles = load_profiles(tmp_path)
    assert len(profiles) == 1
    assert profiles[0]["name"] == "sp-p-core"


def test_load_profiles_skips_manifest(tmp_path):
    import yaml
    (tmp_path / "MANIFEST.yaml").write_text(yaml.dump({"version": 1}))
    (tmp_path / "real_profile.yaml").write_text(yaml.dump({"name": "test", "paths": []}))
    profiles = load_profiles(tmp_path)
    assert len(profiles) == 1
    assert profiles[0]["name"] == "test"


def test_load_profiles_skips_invalid_yaml(tmp_path):
    (tmp_path / "bad.yaml").write_text("{{{{not yaml")
    profiles = load_profiles(tmp_path)
    assert profiles == [], "invalid YAML should be silently skipped"


def test_load_profiles_empty_directory(tmp_path):
    profiles = load_profiles(tmp_path)
    assert profiles == []

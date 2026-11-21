"""Deterministic replay checks for captured M6F real-NOS evidence.

These tests validate the *captured fixtures* under
docs/merger/evidence/*-m6f-*/ — they never contact a live device and
never substitute for live acceptance. They guard against evidence
corruption, accidental secret leakage and status regressions.
"""
from __future__ import annotations

import base64
import json
import re
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
EVIDENCE_GLOB = "docs/merger/evidence/*-m6f-*"
SECRET_PATTERNS = re.compile(
    r"(BEGIN [A-Z ]*PRIVATE KEY|password\"\s*:\s*\"[^\"]+\"|"
    r"NokiaSrl1!|Bearer [A-Za-z0-9._-]{10,})", re.IGNORECASE)


def evidence_dirs():
    return sorted(REPO.glob(EVIDENCE_GLOB))


@pytest.mark.parametrize("evdir", evidence_dirs(),
                         ids=lambda p: p.name)
def test_evidence_result_is_structured(evdir):
    result = json.loads((evdir / "result.json").read_text())
    assert result["vendor"]
    assert result["overall"] in {
        "PASS", "FAIL", "UNSUPPORTED", "NOT_EXECUTED"}
    # an unavailable vendor must never appear green
    if result["overall"] == "PASS":
        assert result["checks"], "PASS without executed checks"


@pytest.mark.parametrize("evdir", evidence_dirs(),
                         ids=lambda p: p.name)
def test_evidence_has_no_secrets(evdir):
    for f in evdir.rglob("*"):
        if f.is_file() and f.stat().st_size < 8 * 1024 * 1024:
            text = f.read_text(errors="replace")
            assert not SECRET_PATTERNS.search(text), \
                f"possible secret material in {f}"


SRL = REPO / "docs/merger/evidence/2026-11-21-m6f-srlinux"


@pytest.mark.skipif(not SRL.exists(), reason="SR Linux M6F evidence absent")
class TestSrlinuxCapturedEvidence:
    def test_capabilities(self):
        caps = json.loads((SRL / "capabilities.json").read_text())
        assert caps["ok"] is True
        assert caps["gnmi_version"] == "0.10.0"
        assert "JSON_IETF" in caps["supported_encodings"]
        names = {m["name"] for m in caps["supported_models"]}
        assert any("ietf-yang-library" in n for n in names)

    def test_yang_library_rfc8525_decodes(self):
        doc = json.loads((SRL / "yang-library-ietf.json").read_text())
        assert doc["ok"] is True
        notif = doc["notifications"][0]["update"][0]
        payload = base64.b64decode(notif["val"]["json_ietf_val"])
        yanglib = json.loads(payload)
        modules = {m["name"] for ms in yanglib["module-set"]
                   for m in ms["module"]}
        assert "ietf-yang-library" in modules
        assert any(n.startswith("srl_nokia") or "srlinux" in n
                   for n in modules)

    def test_subscribe_initial_sync(self):
        doc = json.loads((SRL / "subscribe-once.json").read_text())
        assert doc["ok"] is True and doc["count"] > 0
        paths = [u["path"]["elem"][0]["name"]
                 for n in doc["responses"] if "update" in n
                 for u in n["update"]]
        assert any("interface" in p for p in paths)

    def test_matrix_matches_evidence(self):
        matrix = (REPO / "lab/acceptance/m6f/MATRIX.md").read_text()
        srl_row = next(l for l in matrix.splitlines()
                       if l.startswith("| Capabilities |"))
        assert "PASS" in srl_row.split("|")[2]

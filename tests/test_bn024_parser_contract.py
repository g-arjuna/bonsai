"""BN-024-01 — parser sidecar wire-contract tests.

Dependency-free (AST-based) checks that both sidecar FastAPI apps expose the
request/response fields the Rust parser chain (``src/parser_chain.rs``) sends
and validates. The Rust side of the contract is pinned by wiremock tests in
``src/parser_chain.rs``; this file pins the Python side so the two cannot drift
without a failing test.

Wire contract (POST {sidecar}/parse):
    request : {"parser", "vendor", "command", "command_pattern", "raw_output"}
    response: {"parser" (echoes wire parser), "backend_used"?, "parsed_json", ...}
"""

from __future__ import annotations

import ast
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PYATS_APP = REPO_ROOT / "docker" / "sidecars" / "pyats" / "app.py"
NATIVE_APP = REPO_ROOT / "docker" / "sidecars" / "bonsai-native-parser" / "app.py"

# Fields the Rust SidecarParserClient always sends.
RUST_SENT_FIELDS = {"parser", "vendor", "command", "command_pattern", "raw_output"}


def _class_fields(path: Path, class_name: str) -> set[str]:
    tree = ast.parse(path.read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and node.name == class_name:
            fields = set()
            for stmt in node.body:
                if isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name):
                    fields.add(stmt.target.id)
            return fields
    raise AssertionError(f"{class_name} not found in {path}")


def _parse_endpoint_return_keys(path: Path) -> set[str]:
    """Collect top-level dict keys returned by the ``parse`` endpoint."""
    tree = ast.parse(path.read_text())
    keys: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == "parse":
            for sub in ast.walk(node):
                if isinstance(sub, ast.Return) and isinstance(sub.value, ast.Dict):
                    for key in sub.value.keys:
                        if isinstance(key, ast.Constant) and isinstance(key.value, str):
                            keys.add(key.value)
    return keys


def test_pyats_parse_request_accepts_rust_wire_fields() -> None:
    fields = _class_fields(PYATS_APP, "ParseRequest")
    # `command` is required by the pyats sidecar; the rest must be accepted.
    assert {"parser", "vendor", "command", "raw_output"} <= fields
    # `command_pattern` may be absent as a declared field (pydantic ignores
    # extras), but `command` must be declared — that was the original 422 defect.
    assert "command" in fields
    assert not (RUST_SENT_FIELDS - fields - {"command_pattern"})


def test_native_parse_request_accepts_rust_wire_fields() -> None:
    fields = _class_fields(NATIVE_APP, "ParseRequest")
    assert {"parser", "vendor", "raw_output"} <= fields
    assert "command" in fields or "command_pattern" in fields


def test_pyats_parse_response_reports_backend_used() -> None:
    keys = _parse_endpoint_return_keys(PYATS_APP)
    assert "backend_used" in keys
    assert "parsed_json" in keys
    assert "parser" in keys


def test_native_parse_response_reports_backend_used() -> None:
    keys = _parse_endpoint_return_keys(NATIVE_APP)
    assert "backend_used" in keys
    assert "parsed_json" in keys
    assert "parser" in keys

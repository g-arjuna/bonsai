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


def _function_dict_keys(path: Path, function_name: str) -> set[str]:
    """Collect string keys from every dict literal inside a function."""
    tree = ast.parse(path.read_text())
    keys: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == function_name:
            for sub in ast.walk(node):
                if isinstance(sub, ast.Dict):
                    for key in sub.keys:
                        if isinstance(key, ast.Constant) and isinstance(key.value, str):
                            keys.add(key.value)
    return keys


def _parse_endpoint_return_keys(path: Path) -> set[str]:
    """Response keys for the ``/parse`` endpoint.

    The pyats app builds responses in ``_respond``; the native app returns a
    dict literal from ``parse`` directly.
    """
    keys = _function_dict_keys(path, "parse")
    keys |= _function_dict_keys(path, "_respond")
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
    # BN-024-01 corrective: distinct parse outcomes must reach the wire so
    # empty/unsupported/failed results never collapse into fallback.
    assert "outcome" in keys


def test_pyats_parse_outcome_vocabulary_is_explicit() -> None:
    source = PYATS_APP.read_text()
    for outcome in (
        "structured_success",
        "structured_empty",
        "unsupported",
        "backend_unavailable",
        "parser_error",
        "unstructured_fallback",
        "invalid_input",
    ):
        assert f'"{outcome}"' in source, f"missing outcome '{outcome}'"


def test_pyats_genie_uses_real_get_parser_signature() -> None:
    """The Genie call must match genie.libs.parser.utils.get_parser(command,
    device) -> (parser_class, kwargs) and cli(output=...) — the previous
    get_parser(os, command) invocation could never have worked."""
    source = PYATS_APP.read_text()
    assert "_genie_parse(command, device)" in source
    assert "parser.cli(output=raw_output" in source
    assert "ParserNotFound" in source


def test_native_parse_response_reports_backend_used() -> None:
    keys = _parse_endpoint_return_keys(NATIVE_APP)
    assert "backend_used" in keys
    assert "parsed_json" in keys
    assert "parser" in keys
    assert "outcome" in keys

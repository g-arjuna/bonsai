"""Validate the playbook catalog against the current Bonsai feature/schema contract.

Checks performed:
1. Every YAML document in playbooks/library loads successfully.
2. Every precondition evaluates against an empty Features object and returns bool.
3. Every {placeholder} token in executable step strings maps to a Features field.
4. Every $token in verification Cypher maps to a Features field.
5. Every node label referenced in verification Cypher exists in src/graph.rs.

Exits non-zero if any validation fails so the script can be wired into CI later.
"""
from __future__ import annotations

import dataclasses
import importlib.util
import re
import string
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
PLAYBOOK_LIBRARY_DIR = REPO_ROOT / "playbooks" / "library"
GRAPH_RS_PATH = REPO_ROOT / "src" / "graph" / "mod.rs"
DETECTION_MODULE_PATH = REPO_ROOT / "python" / "bonsai_sdk" / "detection.py"

_NODE_LABEL_PATTERN = re.compile(r"CREATE NODE TABLE IF NOT EXISTS (\w+)\(")
_CYPHER_LABEL_PATTERN = re.compile(r"\([^)\n]*:(\w+)\b")
_DOLLAR_PLACEHOLDER_PATTERN = re.compile(r"\$([A-Za-z_]\w*)")


def _load_module(module_name: str, path: Path):
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


def load_features_class():
    detection = _load_module("bonsai_detection_validation", DETECTION_MODULE_PATH)
    return detection.Features


def load_playbook_documents(library_dir: Path = PLAYBOOK_LIBRARY_DIR) -> list[tuple[Path, dict[str, Any]]]:
    try:
        import yaml
    except ImportError as exc:  # pragma: no cover - exercised in real runtime, not unit tests
        raise RuntimeError(
            "PyYAML is required to validate playbooks. Install project Python dependencies first."
        ) from exc

    documents: list[tuple[Path, dict[str, Any]]] = []
    for path in sorted(library_dir.glob("*.yaml")):
        raw = yaml.safe_load(path.read_text(encoding="utf-8"))
        if not isinstance(raw, dict):
            raise ValueError(f"{path.name}: top-level YAML document must be a mapping")
        documents.append((path, raw))
    return documents


def schema_node_labels(graph_rs_path: Path = GRAPH_RS_PATH) -> set[str]:
    text = graph_rs_path.read_text(encoding="utf-8")
    return set(_NODE_LABEL_PATTERN.findall(text))


def feature_field_names(features_cls) -> set[str]:
    return {field.name for field in dataclasses.fields(features_cls)}


def empty_features(features_cls):
    return features_cls(device_address="", event_type="", detail={})


def extract_braced_placeholders(template: str) -> set[str]:
    placeholders: set[str] = set()
    for _, field_name, _, _ in string.Formatter().parse(template):
        if not field_name:
            continue
        placeholders.add(field_name.split(".", 1)[0].split("[", 1)[0])
    return placeholders


def extract_dollar_placeholders(template: str) -> set[str]:
    return set(_DOLLAR_PLACEHOLDER_PATTERN.findall(template))


def extract_cypher_node_labels(cypher: str) -> set[str]:
    return set(_CYPHER_LABEL_PATTERN.findall(cypher))


def _iter_step_strings(playbook: dict[str, Any]) -> list[tuple[str, str]]:
    strings: list[tuple[str, str]] = []
    for idx, step in enumerate(playbook.get("steps", [])):
        if not isinstance(step, dict):
            continue
        if "gnmi_set" in step and isinstance(step["gnmi_set"], dict):
            gnmi_set = step["gnmi_set"]
            for key in ("path", "value"):
                value = gnmi_set.get(key)
                if isinstance(value, str):
                    strings.append((f"steps[{idx}].gnmi_set.{key}", value))
    return strings


def validate_playbook_doc(
    path: Path,
    doc: dict[str, Any],
    *,
    features_cls,
    known_labels: set[str],
) -> list[str]:
    errors: list[str] = []
    fields = feature_field_names(features_cls)
    features = empty_features(features_cls)
    detection_rule_id = doc.get("detection_rule_id", "<missing detection_rule_id>")
    playbooks = doc.get("playbooks", [])

    if not isinstance(playbooks, list):
        return [f"{path.name} ({detection_rule_id}): playbooks must be a list"]

    for idx, playbook in enumerate(playbooks):
        context = f"{path.name}::{playbook.get('name', f'playbook[{idx}]')}"
        if not isinstance(playbook, dict):
            errors.append(f"{context}: playbook entry must be a mapping")
            continue

        for expr in playbook.get("preconditions", []):
            try:
                result = eval(expr, {}, {"features": features})  # noqa: S307
            except Exception as exc:
                errors.append(f"{context}: precondition {expr!r} raised {type(exc).__name__}: {exc}")
                continue
            if not isinstance(result, bool):
                errors.append(
                    f"{context}: precondition {expr!r} returned {type(result).__name__}, expected bool"
                )

        for location, value in _iter_step_strings(playbook):
            for placeholder in sorted(extract_braced_placeholders(value)):
                if placeholder not in fields:
                    errors.append(
                        f"{context}: {location} uses unknown {{placeholder}} '{placeholder}'"
                    )

        verification = playbook.get("verification", {})
        if isinstance(verification, dict):
            cypher = verification.get("expected_graph_state") or verification.get("cypher")
            if isinstance(cypher, str):
                for placeholder in sorted(extract_dollar_placeholders(cypher)):
                    if placeholder not in fields:
                        errors.append(
                            f"{context}: verification query uses unknown ${placeholder}"
                        )
                for label in sorted(extract_cypher_node_labels(cypher)):
                    if label not in known_labels:
                        errors.append(
                            f"{context}: verification query references unknown node label '{label}'"
                        )

    return errors


def validate_catalog(
    *,
    library_dir: Path = PLAYBOOK_LIBRARY_DIR,
    graph_rs_path: Path = GRAPH_RS_PATH,
) -> tuple[list[str], list[tuple[Path, dict[str, Any]]]]:
    features_cls = load_features_class()
    documents = load_playbook_documents(library_dir)
    known_labels = schema_node_labels(graph_rs_path)
    errors: list[str] = []

    for path, doc in documents:
        errors.extend(
            validate_playbook_doc(
                path,
                doc,
                features_cls=features_cls,
                known_labels=known_labels,
            )
        )

    return errors, documents


def main() -> int:
    try:
        errors, documents = validate_catalog()
    except Exception as exc:
        print(f"[validate_playbooks] ERROR: {exc}", file=sys.stderr)
        return 1

    playbook_count = sum(len(doc.get("playbooks", [])) for _, doc in documents)
    if errors:
        print(
            f"[validate_playbooks] FAILED: {len(errors)} issue(s) across "
            f"{len(documents)} document(s) / {playbook_count} playbook entries",
            file=sys.stderr,
        )
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(
        f"[validate_playbooks] OK: validated {len(documents)} document(s) / "
        f"{playbook_count} playbook entries"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

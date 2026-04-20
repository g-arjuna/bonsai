"""Playbook catalog — loads YAML from the canonical library/, exposes query API.

Canonical library location: <repo_root>/playbooks/library/
Resolved at runtime by walking up from this file's location to the repo root.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

# Walk up: .../python/bonsai_sdk/playbooks/catalog.py → repo root → playbooks/library
_REPO_ROOT   = Path(__file__).parents[3]
LIBRARY_DIR  = _REPO_ROOT / "playbooks" / "library"


class PlaybookCatalog:
    """Loads all *.yaml files from the library directory at construction time."""

    def __init__(self, library_dir: str | Path | None = None) -> None:
        self._dir = Path(library_dir) if library_dir else LIBRARY_DIR
        self._by_rule: dict[str, dict] = {}
        self._load()

    def _load(self) -> None:
        if not self._dir.exists():
            print(f"[catalog] WARNING: library directory not found: {self._dir}")
            return
        loaded = 0
        for path in sorted(self._dir.glob("*.yaml")):
            try:
                doc = yaml.safe_load(path.read_text())
            except Exception as exc:
                print(f"[catalog] failed to load {path.name}: {exc}")
                continue
            rule_id = doc.get("detection_rule_id")
            if rule_id:
                self._by_rule[rule_id] = doc
                loaded += 1
        print(f"[catalog] loaded {loaded} playbooks from {self._dir}")

    def for_detection(self, rule_id: str, vendor: str) -> list[dict[str, Any]]:
        """Return playbooks matching rule_id and vendor (or vendor="*")."""
        doc = self._by_rule.get(rule_id, {})
        return [
            p for p in doc.get("playbooks", [])
            if p.get("vendor") in (vendor, "*")
        ]

    def all_rule_ids(self) -> list[str]:
        return list(self._by_rule.keys())

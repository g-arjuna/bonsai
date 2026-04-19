"""Playbook catalog — loads YAML from library/, exposes query API."""
from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

LIBRARY_DIR = Path(__file__).parent / "library"


class PlaybookCatalog:
    """Loads all *.yaml files from the library directory at construction time."""

    def __init__(self, library_dir: str | Path | None = None) -> None:
        self._dir = Path(library_dir) if library_dir else LIBRARY_DIR
        self._by_rule: dict[str, dict] = {}
        self._load()

    def _load(self) -> None:
        for path in sorted(self._dir.glob("*.yaml")):
            try:
                doc = yaml.safe_load(path.read_text())
            except Exception as exc:
                print(f"[catalog] failed to load {path.name}: {exc}")
                continue
            rule_id = doc.get("detection_rule_id")
            if rule_id:
                self._by_rule[rule_id] = doc

    def for_detection(self, rule_id: str, vendor: str) -> list[dict[str, Any]]:
        """Return playbooks matching rule_id and vendor (or vendor="*")."""
        doc = self._by_rule.get(rule_id, {})
        return [
            p for p in doc.get("playbooks", [])
            if p.get("vendor") in (vendor, "*")
        ]

    def all_rule_ids(self) -> list[str]:
        return list(self._by_rule.keys())

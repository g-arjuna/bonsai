"""Vendor state mapping adapter — D2-2 T2.

Loads config/vendor_state_mapping/*.yaml at import time and provides
vendor-agnostic helpers for detection rules:

    from bonsai_sdk.state_mapping import is_down, is_up, to_semantic

Rules consume semantic transitions, not vendor strings:

    vendor = client.device_vendor(f.device_address)
    if not is_down(vendor, "bfd_oper_state", f.new_state):
        return None

Adding a new vendor: create config/vendor_state_mapping/<vendor>.yaml.
Adding a new leaf: add a key under state_mappings in the relevant YAML files.
No Python code changes required in detection rules.
"""
from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Optional

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Config discovery
# ---------------------------------------------------------------------------
_REPO_ROOT = Path(__file__).resolve().parent.parent.parent
_MAPPING_DIR = _REPO_ROOT / "config" / "vendor_state_mapping"

# ---------------------------------------------------------------------------
# In-memory registry: vendor -> leaf -> raw_value_lower -> semantic_state
# Also: vendor -> leaf -> "treat_as_down" | "treat_as_up" -> set[semantic]
# ---------------------------------------------------------------------------
_registry: dict[str, dict[str, dict[str, str]]] = {}
_treat_as_down: dict[str, dict[str, set[str]]] = {}
_treat_as_up: dict[str, dict[str, set[str]]] = {}


def _load_mappings() -> None:
    """Load all YAML files from config/vendor_state_mapping/."""
    if not _MAPPING_DIR.exists():
        log.warning("vendor_state_mapping dir not found at %s", _MAPPING_DIR)
        return

    try:
        import yaml  # type: ignore
    except ImportError:
        log.error(
            "PyYAML not available; vendor state mapping disabled. "
            "Install: pip install pyyaml"
        )
        return

    for path in sorted(_MAPPING_DIR.glob("*.yaml")):
        try:
            with path.open(encoding="utf-8") as fh:
                data = yaml.safe_load(fh)
        except Exception as exc:
            log.warning("Failed to load %s: %s", path.name, exc)
            continue

        vendor = data.get("vendor")
        if not vendor:
            log.warning("No 'vendor' key in %s — skipping", path.name)
            continue

        vendor_map: dict[str, dict[str, str]] = {}
        down_map: dict[str, set[str]] = {}
        up_map: dict[str, set[str]] = {}

        for leaf, leaf_data in (data.get("state_mappings") or {}).items():
            raw_to_semantic: dict[str, str] = {}
            for semantic, raw_values in (leaf_data.get("semantic_states") or {}).items():
                for raw in raw_values:
                    raw_to_semantic[raw.lower()] = semantic

            vendor_map[leaf] = raw_to_semantic
            down_map[leaf] = set(leaf_data.get("treat_as_down") or [])
            up_map[leaf] = set(leaf_data.get("treat_as_up") or [])

        _registry[vendor] = vendor_map
        _treat_as_down[vendor] = down_map
        _treat_as_up[vendor] = up_map
        log.debug("Loaded vendor state mapping: %s (%d leaves)", vendor, len(vendor_map))


_load_mappings()


# ---------------------------------------------------------------------------
# Vendor alias normalisation
# ---------------------------------------------------------------------------
_VENDOR_ALIASES: dict[str, str] = {
    "nokia_srl":       "nokia_srl",
    "nokia_srlinux":   "nokia_srl",
    "nokia":           "nokia_srl",
    "cisco_iosxr":     "cisco_iosxr",
    "cisco-iosxr":     "cisco_iosxr",
    "iosxr":           "cisco_iosxr",
    "cisco_iosxe":     "cisco_iosxe",
    "cisco-iosxe":     "cisco_iosxe",
    "iosxe":           "cisco_iosxe",
    "juniper_junos":   "juniper_junos",
    "juniper-junos":   "juniper_junos",
    "junos":           "juniper_junos",
    "juniper":         "juniper_junos",
    "arista_eos":      "arista_eos",
    "arista-eos":      "arista_eos",
    "arista":          "arista_eos",
    "eos":             "arista_eos",
    "frr":             "frr",
    "frrouting":       "frr",
}


def _normalise_vendor(vendor: str) -> str:
    return _VENDOR_ALIASES.get(vendor.lower(), vendor.lower())


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def to_semantic(vendor: str, leaf: str, raw_value: str) -> Optional[str]:
    """Translate a raw vendor state string to a semantic state name.

    Returns None if the vendor or leaf is unknown, or the raw value has no
    mapping (caller should treat as unknown, not down).
    """
    v = _normalise_vendor(vendor)
    leaf_map = _registry.get(v, {}).get(leaf)
    if leaf_map is None:
        return None
    return leaf_map.get(raw_value.lower())


def is_down(vendor: str, leaf: str, raw_value: str) -> bool:
    """Return True if raw_value maps to a treat_as_down semantic state.

    Falls back to False (not-down) if the vendor/leaf/value is unknown,
    preventing false-positive detections when mappings are incomplete.
    """
    semantic = to_semantic(vendor, leaf, raw_value)
    if semantic is None:
        return False
    v = _normalise_vendor(vendor)
    return semantic in _treat_as_down.get(v, {}).get(leaf, set())


def is_up(vendor: str, leaf: str, raw_value: str) -> bool:
    """Return True if raw_value maps to a treat_as_up semantic state."""
    semantic = to_semantic(vendor, leaf, raw_value)
    if semantic is None:
        return False
    v = _normalise_vendor(vendor)
    return semantic in _treat_as_up.get(v, {}).get(leaf, set())


def known_vendor(vendor: str) -> bool:
    """Return True if this vendor has a loaded mapping file."""
    return _normalise_vendor(vendor) in _registry


def reload() -> None:
    """Reload all mappings from disk (useful after editing YAML files)."""
    _registry.clear()
    _treat_as_down.clear()
    _treat_as_up.clear()
    _load_mappings()

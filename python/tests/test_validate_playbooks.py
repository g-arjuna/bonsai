from __future__ import annotations

import importlib.util
from pathlib import Path


REPO_ROOT = Path(__file__).parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "validate_playbooks.py"


def _load_validator_module():
    spec = importlib.util.spec_from_file_location("validate_playbooks", SCRIPT_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def test_schema_node_labels_include_bfd_session():
    validator = _load_validator_module()

    labels = validator.schema_node_labels()

    assert "BfdSession" in labels
    assert "BgpNeighbor" in labels
    assert "Device" in labels


def test_validate_known_good_catalog_has_no_errors():
    validator = _load_validator_module()

    errors, documents = validator.validate_catalog()

    assert len(documents) == 9
    assert errors == []


def test_validate_playbook_doc_flags_unknown_placeholder_and_label():
    validator = _load_validator_module()
    features_cls = validator.load_features_class()
    known_labels = validator.schema_node_labels()

    doc = {
        "detection_rule_id": "bgp_session_down",
        "playbooks": [
            {
                "name": "broken_playbook",
                "preconditions": ["features.peer_address != ''"],
                "steps": [
                    {
                        "gnmi_set": {
                            "path": "interfaces/interface[name={neighbor_ip}]/config/enabled",
                            "value": "true",
                        }
                    }
                ],
                "verification": {
                    "expected_graph_state": (
                        "MATCH (n:OspfNeighbor {peer_address: $peer_address}) "
                        "RETURN count(n) > 0"
                    )
                },
            }
        ],
    }

    errors = validator.validate_playbook_doc(
        REPO_ROOT / "playbooks" / "library" / "broken.yaml",
        doc,
        features_cls=features_cls,
        known_labels=known_labels,
    )

    assert any("unknown {placeholder} 'neighbor_ip'" in error for error in errors)
    assert any("unknown node label 'OspfNeighbor'" in error for error in errors)


def test_validate_playbook_doc_flags_bad_precondition():
    validator = _load_validator_module()
    features_cls = validator.load_features_class()
    known_labels = validator.schema_node_labels()

    doc = {
        "detection_rule_id": "interface_down",
        "playbooks": [
            {
                "name": "bad_precondition",
                "preconditions": ["features.neighbor_ip != ''"],
                "steps": [],
                "verification": {},
            }
        ],
    }

    errors = validator.validate_playbook_doc(
        REPO_ROOT / "playbooks" / "library" / "bad.yaml",
        doc,
        features_cls=features_cls,
        known_labels=known_labels,
    )

    assert any("raised AttributeError" in error for error in errors)

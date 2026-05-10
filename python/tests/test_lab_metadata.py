"""Guardrails for the CV1 lab metadata convention."""
from __future__ import annotations

from pathlib import Path

import yaml


LAB_FILES = [
    "lab/dc/dc-evpn-srv6.clab.yml",
    "lab/sp/sp-mpls-srte.clab.yml",
    "lab/cloud-dc-6node.yml",
    "lab/fast-iteration/3node-srl.clab.yml",
    "lab/fast-iteration/bonsai-phase4.clab.yml",
    "lab/fast-iteration/multivendor.clab.yml",
]

ALLOWED_ENVIRONMENTS = {"data_center", "service_provider", "campus_wired"}
ALLOWED_ROLES = {
    "super-spine",
    "spine",
    "leaf",
    "pe",
    "p",
    "rr",
    "ce",
    "access",
    "distribution",
    "core",
    "edge",
    "host",
}


def test_lab_topologies_declare_role_and_environment_metadata():
    repo_root = Path(__file__).resolve().parents[2]

    for rel_path in LAB_FILES:
        doc = yaml.safe_load((repo_root / rel_path).read_text(encoding="utf-8"))
        nodes = doc["topology"]["nodes"]
        environments = set()

        assert nodes, f"{rel_path} should declare at least one node"
        for node_name, node_data in nodes.items():
            labels = node_data.get("labels") or {}
            role = labels.get("bonsai.role")
            environment = labels.get("bonsai.environment")

            assert role in ALLOWED_ROLES, (
                f"{rel_path}:{node_name} is missing a supported bonsai.role label"
            )
            assert environment in ALLOWED_ENVIRONMENTS, (
                f"{rel_path}:{node_name} is missing a supported bonsai.environment label"
            )
            environments.add(environment)

        assert len(environments) == 1, (
            f"{rel_path} should have a single shared bonsai.environment across all nodes"
        )

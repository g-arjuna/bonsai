"""BN-024 differential oracle comparator tests.

Self-contained synthetic fixtures exercise ``scripts/bn024/
interface_oracle.py compare`` — no SuzieQ checkout or textfsm needed.
The comparator must FAIL on deliberate mutations of the Rust artifact:
incorrect typed MTU, MAC, IP address, interface type, a missing fact
and a duplicate fact. A clean baseline must pass.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "bn024" / "interface_oracle.py"

spec = importlib.util.spec_from_file_location("interface_oracle", SCRIPT)
oracle = importlib.util.module_from_spec(spec)
sys.modules["interface_oracle"] = oracle
spec.loader.exec_module(oracle)


def _upstream_record() -> dict:
    return {
        "ifname": "eth0",
        "adminState": "up",
        "state": "up",
        "type": "ethernet",
        "mtu": "1500",
        "speed": "1000",
        "macaddr": "44:38:39:00:00:05",
        "master": "",
        "ipAddressList": ["10.0.0.5/24"],
        "ip6AddressList": [],
        "vrf": None,
    }


def _known(value):
    return {"state": "known", "value": value}


def _rust_fact() -> dict:
    return {
        "ifname": "eth0",
        "subject": {
            "device_id": "dev-1",
            "namespace": "default",
            "entity": "eth0",
        },
        "payload": {
            "interface_name": "eth0",
            "administrative_state": _known("up"),
            "operational_state": _known("up"),
            "interface_type": _known("physical"),
            "mtu": _known(1500),
            "mac_address": _known("44:38:39:00:00:05"),
            "ip_addresses": _known(["10.0.0.5/24"]),
            "ipv6_addresses": _known([]),
            "speed": {"state": "known", "value": {"value": 1000, "unit": "mbit"}},
        },
        "quality": {"completeness": "complete"},
    }


def _rust_doc(facts=None) -> dict:
    return {
        "nos": "cumulus",
        "interfaces": [
            {
                "ifname": "eth0",
                "type_raw": "ether",
                "mtu_raw": "1500",
                "mac_raw": "44:38:39:00:00:05",
                "ip_addresses": ["10.0.0.5/24"],
                "ipv6_addresses": [],
            }
        ],
        "facts": facts if facts is not None else [_rust_fact()],
        "report": {},
    }


@pytest.fixture()
def paths(tmp_path: Path):
    up = tmp_path / "up.json"
    up.write_text(json.dumps([_upstream_record()]))
    return up


def _compare(up_path: Path, rust_doc: dict, tmp_path: Path) -> int:
    rp = tmp_path / "rust.json"
    rp.write_text(json.dumps(rust_doc))
    return oracle.compare(up_path, rp)


def test_clean_baseline_passes(paths: Path, tmp_path: Path) -> None:
    assert _compare(paths, _rust_doc(), tmp_path) == 0


@pytest.mark.parametrize(
    "mutate,needle",
    [
        (
            lambda f: f["payload"]["mtu"].update(value=9000),
            "mtu",
        ),
        (
            lambda f: f["payload"]["mac_address"].update(
                value="44:38:39:00:00:06"
            ),
            "macaddr",
        ),
        (
            lambda f: f["payload"]["ip_addresses"].update(
                value=["10.0.0.6/24"]
            ),
            "ipAddressList",
        ),
        (
            lambda f: f["payload"]["interface_type"].update(value="virtual"),
            "type",
        ),
        (
            lambda f: f["payload"]["interface_type"].update(state="unknown", value=None),
            "type",
        ),
    ],
)
def test_typed_mutations_fail(paths: Path, tmp_path: Path, mutate, needle: str) -> None:
    fact = _rust_fact()
    mutate(fact)
    assert _compare(paths, _rust_doc([fact]), tmp_path) == 1


def test_missing_fact_fails(paths: Path, tmp_path: Path) -> None:
    assert _compare(paths, _rust_doc([]), tmp_path) == 1


def test_duplicate_fact_fails(paths: Path, tmp_path: Path) -> None:
    doc = _rust_doc([_rust_fact(), _rust_fact()])
    assert _compare(paths, doc, tmp_path) == 1


def test_missing_interface_diagnostic_fails(paths: Path, tmp_path: Path) -> None:
    doc = _rust_doc()
    doc["interfaces"] = []
    assert _compare(paths, doc, tmp_path) == 1


def test_unmapped_upstream_type_fails_closed(paths: Path, tmp_path: Path) -> None:
    up = json.loads(paths.read_text())
    up[0]["type"] = "some_future_nos_type"
    paths.write_text(json.dumps(up))
    assert _compare(paths, _rust_doc(), tmp_path) == 1


def test_subject_identity_mutation_fails(paths: Path, tmp_path: Path) -> None:
    fact = _rust_fact()
    fact["subject"]["entity"] = "eth9"
    assert _compare(paths, _rust_doc([fact]), tmp_path) == 1

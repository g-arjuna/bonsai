"""Integration smoke test: inject a BGP session down on srl-spine1 and assert detection fires.

Prerequisites:
  - ContainerLab bonsai-phase4 topology is running
  - bonsai binary is at target/release/bonsai (or bonsai.exe on Windows)
  - SSH key auth or password-less SSH to 172.100.102.11 is configured
    (or set BONSAI_SSH_PASS env var for sshpass)

Run:
    pytest python/tests/test_integration_bgp_flap.py -v
    BONSAI_DRY_RUN=1 pytest python/tests/test_integration_bgp_flap.py -v
"""
from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

import pytest

# All tests in this module require a live ContainerLab topology and bonsai binary.
pytestmark = pytest.mark.integration

sys.path.insert(0, str(Path(__file__).parent.parent))

from bonsai_sdk import BonsaiClient, RuleEngine, RemediationExecutor
from bonsai_sdk.detection import Detection

# ── constants ─────────────────────────────────────────────────────────────────

BONSAI_ADDR     = "[::1]:50051"
SPINE1_ADDRESS  = "172.100.102.11:57400"   # as stored in the graph
SPINE1_IP       = "172.100.102.11"
SPINE1_USER     = "admin"
import platform
import os

BONSAI_EXE_NAME = "bonsai.exe" if platform.system() == "Windows" else "bonsai"
BONSAI_BINARY   = str(Path(__file__).parent.parent.parent / "target" / "release" / BONSAI_EXE_NAME)

TIMEOUT_STARTUP    = 40   # seconds to wait for bonsai to populate ≥1 device
TIMEOUT_DETECTION  = 35   # seconds to wait for bgp_session_down to fire


# ── fixtures ──────────────────────────────────────────────────────────────────

@pytest.fixture(scope="module")
def bonsai_proc():
    """Start bonsai as a subprocess; stop it after the test module."""
    if not Path(BONSAI_BINARY).exists():
        pytest.skip(f"bonsai binary not found at {BONSAI_BINARY} — build with `cargo build --release`")
    proc = subprocess.Popen(
        [BONSAI_BINARY],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    yield proc
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()


@pytest.fixture(scope="module")
def client(bonsai_proc):
    """Connect Python SDK; wait until spine1 is in the graph."""
    with BonsaiClient(BONSAI_ADDR) as c:
        deadline = time.time() + TIMEOUT_STARTUP
        while time.time() < deadline:
            try:
                devices = c.get_devices()
                if any(d.address == SPINE1_ADDRESS for d in devices):
                    break
            except Exception:
                pass
            time.sleep(2)
        else:
            pytest.fail(
                f"srl-spine1 ({SPINE1_ADDRESS}) did not appear in graph within {TIMEOUT_STARTUP}s"
                " — is the ContainerLab topology running?"
            )
        yield c


# ── test ──────────────────────────────────────────────────────────────────────

def test_bgp_session_down_detection(client: BonsaiClient):
    """Disable a BGP neighbor on srl-spine1; assert bgp_session_down fires within 30s."""

    # Discover the first established BGP neighbor on spine1 so the test is self-discovering.
    neighbors = client.get_bgp_neighbors(SPINE1_ADDRESS)
    established = [n for n in neighbors if n.session_state == "established"]
    if not established:
        pytest.skip("No established BGP sessions on spine1 — cannot inject fault")
    peer = established[0].peer_address

    detected: list[Detection] = []

    def on_detection(d: Detection) -> None:
        if d.rule_id == "bgp_session_down" and SPINE1_ADDRESS in d.features.device_address:
            detected.append(d)
            try:
                client.create_detection(
                    device_address=d.features.device_address,
                    rule_id=d.rule_id,
                    severity=d.severity,
                    features_json=d.features.to_json(),
                    fired_at_ns=d.features.occurred_at_ns or int(time.time() * 1e9),
                    state_change_event_id=d.features.state_change_event_id,
                )
            except Exception:
                pass

    engine = RuleEngine(client=client, on_detection=on_detection, dry_run=True)
    engine.start()

    try:
        _srl_cli(SPINE1_IP, SPINE1_USER,
                 f"set / network-instance default protocols bgp neighbor {peer} admin-state disable")

        deadline = time.time() + TIMEOUT_DETECTION
        while time.time() < deadline and not detected:
            time.sleep(1)

        assert detected, (
            f"bgp_session_down not detected within {TIMEOUT_DETECTION}s after disabling "
            f"BGP neighbor {peer} on {SPINE1_ADDRESS}"
        )
    finally:
        # Always restore — leave the lab in working state regardless of test outcome.
        _srl_cli(SPINE1_IP, SPINE1_USER,
                 f"set / network-instance default protocols bgp neighbor {peer} admin-state enable")
        engine.stop()


# ── helpers ───────────────────────────────────────────────────────────────────

def _srl_cli(host: str, user: str, cmd: str) -> None:
    """Run an sr_cli command on an SRL node via SSH."""
    result = subprocess.run(
        [
            "ssh",
            "-o", "StrictHostKeyChecking=no",
            "-o", "ConnectTimeout=10",
            f"{user}@{host}",
            f'sr_cli "{cmd}"',
        ],
        capture_output=True,
        text=True,
        timeout=20,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"SSH command failed on {host}: {result.stderr.strip() or result.stdout.strip()}"
        )

"""Fixed-command contract for the FRR OSPF/IS-IS SSH capture helper."""

import importlib.util
import json
import sys
from pathlib import Path

import pytest

MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "frr_igp_capture.py"
spec = importlib.util.spec_from_file_location("frr_igp_capture", MODULE_PATH)
igp_capture = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = igp_capture
spec.loader.exec_module(igp_capture)

BASE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "frr_interface_capture.py"
base_spec = importlib.util.spec_from_file_location("frr_interface_capture_testbase", BASE_PATH)
base_capture = importlib.util.module_from_spec(base_spec)
sys.modules[base_spec.name] = base_capture
base_spec.loader.exec_module(base_capture)


class FakeStdout:
    def __init__(self, channel):
        self.channel = channel


class FakeChannel:
    def __init__(self, stdout_chunks, stderr_chunks=(), exit_status=0):
        self.stdout_chunks = list(stdout_chunks)
        self.stderr_chunks = list(stderr_chunks)
        self.exit_status = exit_status
        self.closed = False
        self.eof_received = True

    def recv_ready(self):
        return bool(self.stdout_chunks)

    def recv(self, _size):
        return self.stdout_chunks.pop(0)

    def recv_stderr_ready(self):
        return bool(self.stderr_chunks)

    def recv_stderr(self, _size):
        return self.stderr_chunks.pop(0)

    def exit_status_ready(self):
        return True

    def recv_exit_status(self):
        return self.exit_status

    def close(self):
        self.closed = True


class FakeKey:
    def get_name(self):
        return "ssh-ed25519"

    def asbytes(self):
        return b"pinned-test-key"


class FakeTransport:
    def get_remote_server_key(self):
        return FakeKey()


class FakeClient:
    unsupported = set()

    def __init__(self):
        self.commands = []

    def load_host_keys(self, path):
        self.known_hosts = path

    def set_missing_host_key_policy(self, policy):
        self.policy = policy

    def connect(self, **kwargs):
        self.connect_kwargs = kwargs

    def get_transport(self):
        return FakeTransport()

    def exec_command(self, command, timeout=15):
        self.commands.append(command)
        outputs = {
            base_capture.VERSION_COMMAND: b'{"version":"10.6.1_git","hostName":"frr2"}',
            igp_capture.OSPF_NEIGHBOR_COMMAND: b'{"default":{"neighbors":{}}}',
            igp_capture.OSPF_LSDB_COMMAND: b'{"default":{"routerLinkStates":{"areas":{}}}}',
            igp_capture.ISIS_NEIGHBOR_COMMAND: b'{"areas":[]}',
            igp_capture.ISIS_LSDB_COMMAND: b'{"areas":[]}',
        }
        if command in self.unsupported:
            return None, FakeStdout(FakeChannel([b""], [b"unknown command"], exit_status=1)), None
        return None, FakeStdout(FakeChannel([outputs[command]])), None

    def close(self):
        pass


def invoke(monkeypatch, capsys, client, families=None):
    monkeypatch.setattr(igp_capture.paramiko, "SSHClient", lambda: client)
    monkeypatch.setenv("BONSAI_CAPTURE_PASSWORD", "test-password")
    argv = [
        "frr_igp_capture.py",
        "--host",
        "192.0.2.1",
        "--username",
        "obs",
        "--known-hosts",
        "/tmp/pins",
    ]
    if families is not None:
        argv.extend(["--families", families])
    monkeypatch.setattr(sys, "argv", argv)
    assert igp_capture.main() == 0
    return json.loads(capsys.readouterr().out)


def test_main_executes_exact_fixed_igp_commands(monkeypatch, capsys):
    client = FakeClient()
    envelope = invoke(monkeypatch, capsys, client)
    assert client.commands == [
        base_capture.VERSION_COMMAND,
        igp_capture.OSPF_NEIGHBOR_COMMAND,
        igp_capture.OSPF_LSDB_COMMAND,
        igp_capture.ISIS_NEIGHBOR_COMMAND,
        igp_capture.ISIS_LSDB_COMMAND,
    ]
    assert envelope["acquisition_contract"] == "frr_verified_ssh_igp_snapshot_v1"
    assert envelope["acquisition_batch"]["atomic_snapshot"] is False
    assert envelope["host_key"]["sha256"].startswith("SHA256:")
    for family in igp_capture.FAMILY_ORDER:
        assert envelope["commands"][family]["outcome"] == "complete"
        assert envelope["commands"][family]["output_sha256"]


def test_unsupported_protocol_family_does_not_fabricate_empty_success(monkeypatch, capsys):
    class IsisUnsupported(FakeClient):
        unsupported = {igp_capture.ISIS_NEIGHBOR_COMMAND}

    envelope = invoke(monkeypatch, capsys, IsisUnsupported())
    assert envelope["commands"]["ospf_neighbors"]["outcome"] == "complete"
    assert envelope["commands"]["isis_neighbors"]["outcome"] == "unsupported"
    assert envelope["commands"]["isis_neighbors"]["output"] is None


def test_unselected_family_is_explicitly_not_attempted(monkeypatch, capsys):
    envelope = invoke(monkeypatch, capsys, FakeClient(), families="ospf_neighbors")
    assert envelope["commands"]["ospf_neighbors"]["outcome"] == "complete"
    assert envelope["commands"]["ospf_lsdb"]["outcome"] == "not_attempted"
    assert envelope["commands"]["isis_neighbors"]["outcome"] == "not_attempted"
    assert envelope["commands"]["isis_lsdb"]["outcome"] == "not_attempted"


def test_unknown_family_selector_is_rejected():
    with pytest.raises(ValueError, match="unsupported fixed IGP capture family"):
        igp_capture.parse_families("ospf_neighbors,show running-config")

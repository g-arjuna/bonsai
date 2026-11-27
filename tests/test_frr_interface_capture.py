"""Bounded streaming contract for the fixed FRR SSH capture helper."""

import importlib.util
import sys
from pathlib import Path

import pytest

MODULE_PATH = Path(__file__).resolve().parents[1] / "scripts" / "frr_interface_capture.py"
spec = importlib.util.spec_from_file_location("frr_interface_capture", MODULE_PATH)
capture = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = capture
spec.loader.exec_module(capture)


class FakeStdout:
    def __init__(self, channel):
        self.channel = channel


class FakeClient:
    def __init__(self, channel):
        self.channel = channel

    def exec_command(self, command, timeout=15):
        assert command in {
            capture.VERSION_COMMAND,
            capture.INTERFACE_COMMAND,
            capture.NEIGHBOR_COMMAND,
            capture.MAC_COMMAND,
            capture.VLAN_COMMAND,
        }
        return None, FakeStdout(self.channel), None


class FakeChannel:
    def __init__(self, stdout_chunks, stderr_chunks=(), exit_ready=True, exit_status=0):
        self.stdout_chunks = list(stdout_chunks)
        self.stderr_chunks = list(stderr_chunks)
        self.exit_ready = exit_ready
        self.exit_status = exit_status
        self.closed = False
        self.eof_received = True
        self.close_calls = 0

    def recv_ready(self):
        return bool(self.stdout_chunks)

    def recv(self, _size):
        return self.stdout_chunks.pop(0)

    def recv_stderr_ready(self):
        return bool(self.stderr_chunks)

    def recv_stderr(self, _size):
        return self.stderr_chunks.pop(0)

    def exit_status_ready(self):
        return self.exit_ready

    def recv_exit_status(self):
        return self.exit_status

    def close(self):
        self.close_calls += 1
        self.closed = True


def run_with_channel(channel):
    return capture.run_command(FakeClient(channel), capture.INTERFACE_COMMAND)


def test_exit_status_does_not_complete_before_buffered_output_is_drained():
    # The exit status is already available while both streams still have
    # pending bytes. Completion must consume those bytes rather than treating
    # the status itself as end-of-output.
    channel = FakeChannel([b"part-1", b"part-2"], [b"stderr-finished"], exit_ready=True)
    assert run_with_channel(channel) == "part-1part-2"
    assert not channel.stdout_chunks
    assert not channel.stderr_chunks
    assert channel.close_calls == 0


def test_stdout_byte_bound_closes_channel_promptly():
    channel = FakeChannel([b"x" * (capture.MAX_BYTES + 1)], exit_ready=True)
    with pytest.raises(ValueError, match="exceeds 4 MiB"):
        run_with_channel(channel)
    assert channel.close_calls == 1


def test_stderr_byte_bound_closes_channel_promptly():
    channel = FakeChannel([], [b"x" * (capture.MAX_BYTES + 1)], exit_ready=True)
    with pytest.raises(ValueError, match="exceeds 4 MiB"):
        run_with_channel(channel)
    assert channel.close_calls == 1


def test_continuously_ready_stdout_cannot_starve_absolute_deadline(monkeypatch):
    class EndlessStdout(FakeChannel):
        def recv_ready(self):
            return True

        def recv(self, _size):
            return b"x"

    now = 1000.0

    def monotonic():
        nonlocal now
        now += 0.011
        return now

    monkeypatch.setattr(capture.time, "monotonic", monotonic)
    channel = EndlessStdout([], exit_ready=False)
    with pytest.raises(ValueError, match="deadline exceeded"):
        run_with_channel(channel)
    assert channel.close_calls == 1


class MainFakeKey:
    def get_name(self):
        return "ssh-ed25519"

    def asbytes(self):
        return b"test-host-key"


class MainFakeTransport:
    def get_remote_server_key(self):
        return MainFakeKey()


class MainFakeClient:
    def __init__(self):
        self.commands = []

    def load_host_keys(self, path):
        self.known_hosts = path

    def set_missing_host_key_policy(self, policy):
        self.policy = policy

    def connect(self, **kwargs):
        self.connect_kwargs = kwargs

    def get_transport(self):
        return MainFakeTransport()

    def exec_command(self, command, timeout=15):
        self.commands.append(command)
        output = {
            capture.VERSION_COMMAND: b'{"version":"10.6.1_git","hostName":"frr2"}',
            capture.INTERFACE_COMMAND: b"{}",
            capture.NEIGHBOR_COMMAND: b"",
            capture.MAC_COMMAND: b"",
            capture.VLAN_COMMAND: b"port              vlan-id  \n",
        }[command]
        return None, FakeStdout(FakeChannel([output], exit_ready=True)), None

    def close(self):
        pass


def test_main_executes_exact_fixed_multifamily_commands_and_emits_envelope(monkeypatch, capsys):
    client = MainFakeClient()
    monkeypatch.setattr(capture.paramiko, "SSHClient", lambda: client)
    monkeypatch.setenv("BONSAI_CAPTURE_PASSWORD", "test-password")
    monkeypatch.setattr(
        sys,
        "argv",
        ["frr_interface_capture.py", "--host", "192.0.2.1", "--username", "obs", "--known-hosts", "/tmp/pins"],
    )
    assert capture.main() == 0
    assert client.commands == [
        capture.VERSION_COMMAND,
        capture.INTERFACE_COMMAND,
        capture.NEIGHBOR_COMMAND,
        capture.MAC_COMMAND,
        capture.VLAN_COMMAND,
    ]
    envelope = __import__("json").loads(capsys.readouterr().out)
    assert envelope["acquisition_batch"]["atomic_snapshot"] is False
    assert envelope["acquisition_batch"]["deadline_seconds"] == capture.BATCH_DEADLINE_SECS
    assert envelope["commands"]["arpnd"]["command"] == capture.NEIGHBOR_COMMAND
    assert envelope["commands"]["mac"]["command"] == capture.MAC_COMMAND
    assert envelope["commands"]["vlan"]["command"] == capture.VLAN_COMMAND
    assert envelope["commands"]["arpnd"]["output"] == ""
    assert envelope["commands"]["mac"]["output"] == ""
    assert envelope["commands"]["vlan"]["output"].startswith("port")
    for key in ["version", "interfaces", "arpnd", "mac", "vlan"]:
        command = envelope["commands"][key]
        assert command["outcome"] == "complete"
        assert command["started_at_ns"] is not None
        assert command["completed_at_ns"] is not None
        assert command["output_sha256"]


def test_optional_family_failure_preserves_other_command_results(monkeypatch, capsys):
    class VlanUnsupportedClient(MainFakeClient):
        def exec_command(self, command, timeout=15):
            self.commands.append(command)
            if command == capture.VLAN_COMMAND:
                channel = FakeChannel(
                    [b""], [b"bridge: command not found"], exit_ready=True, exit_status=1
                )
                return None, FakeStdout(channel), None
            return super().exec_command(command, timeout)

    client = VlanUnsupportedClient()
    monkeypatch.setattr(capture.paramiko, "SSHClient", lambda: client)
    monkeypatch.setenv("BONSAI_CAPTURE_PASSWORD", "test-password")
    monkeypatch.setattr(
        sys,
        "argv",
        ["frr_interface_capture.py", "--host", "192.0.2.1", "--username", "obs", "--known-hosts", "/tmp/pins"],
    )
    assert capture.main() == 0
    envelope = __import__("json").loads(capsys.readouterr().out)
    assert envelope["commands"]["interfaces"]["outcome"] == "complete"
    assert envelope["commands"]["arpnd"]["outcome"] == "complete"
    assert envelope["commands"]["mac"]["outcome"] == "complete"
    assert envelope["commands"]["vlan"]["outcome"] == "unsupported"
    assert envelope["commands"]["vlan"]["output"] is None

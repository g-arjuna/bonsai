import importlib.util
import sys
from pathlib import Path

import pytest

MODULE_PATH = Path(__file__).resolve().parents[2] / "scripts" / "frr_interface_capture.py"
spec = importlib.util.spec_from_file_location("frr_interface_capture", MODULE_PATH)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def test_fixed_capture_contract_is_not_parameterized():
    assert capture.INTERFACE_COMMAND == "vtysh -c 'show interface json'"
    assert capture.VERSION_COMMAND == "vtysh -c 'show version json'"
    assert capture.MAX_BYTES == 4 * 1024 * 1024


def test_bounded_stream_rejects_oversize_output():
    class Stream:
        def read(self, _n):
            return b"x" * (capture.MAX_BYTES + 1)

    with pytest.raises(ValueError, match="exceeds 4 MiB"):
        capture.read_bounded(Stream())


def test_bounded_stream_preserves_valid_utf8_bytes():
    class Stream:
        def read(self, _n):
            return '{"ok": true}'.encode()

    assert capture.read_bounded(Stream()) == '{"ok": true}'


def test_missing_password_fails_before_network_use(monkeypatch):
    monkeypatch.delenv("BONSAI_CAPTURE_PASSWORD", raising=False)
    monkeypatch.setattr(
        sys,
        "argv",
        ["frr_interface_capture.py", "--host", "192.0.2.1", "--username", "obs"],
    )
    with pytest.raises(ValueError, match="credential"):
        capture.main()


class FakeChannel:
    def __init__(self, stdout_chunks, stderr_chunks, status=0):
        self.stdout_chunks = list(stdout_chunks)
        self.stderr_chunks = list(stderr_chunks)
        self.status = status
        self.closed = False

    def recv_ready(self):
        return bool(self.stdout_chunks)

    def recv(self, _n):
        return self.stdout_chunks.pop(0)

    def recv_stderr_ready(self):
        return bool(self.stderr_chunks)

    def recv_stderr(self, _n):
        return self.stderr_chunks.pop(0)

    def exit_status_ready(self):
        return not self.stdout_chunks and not self.stderr_chunks

    def recv_exit_status(self):
        return self.status

    def close(self):
        self.closed = True


class FakeStdout:
    def __init__(self, channel):
        self.channel = channel


class FakeClient:
    def __init__(self, channel):
        self.channel = channel
        self.commands = []

    def exec_command(self, command, timeout=0):
        self.commands.append((command, timeout))
        return None, FakeStdout(self.channel), None


def test_remote_stdout_and_stderr_are_drained_independently():
    channel = FakeChannel([b'{"ok": true}'], [b'diagnostic noise'])
    client = FakeClient(channel)
    assert capture.run_command(client, capture.VERSION_COMMAND) == '{"ok": true}'
    assert client.commands == [(capture.VERSION_COMMAND, 15)]


def test_remote_stderr_output_limit_is_an_explicit_refusal():
    channel = FakeChannel([b'{}'], [b'x' * (capture.MAX_BYTES + 1)])
    client = FakeClient(channel)
    with pytest.raises(ValueError, match='exceeds 4 MiB'):
        capture.run_command(client, capture.VERSION_COMMAND)


def test_remote_command_failure_is_rejected():
    channel = FakeChannel([b'{}'], [], status=1)
    client = FakeClient(channel)
    with pytest.raises(ValueError, match='command failed'):
        capture.run_command(client, capture.VERSION_COMMAND)


class HangingChannel(FakeChannel):
    def __init__(self):
        super().__init__([], [])

    def exit_status_ready(self):
        return False


def test_remote_command_deadline_closes_channel_explicitly(monkeypatch):
    channel = HangingChannel()
    client = FakeClient(channel)
    ticks = iter([0.0, 0.0, 16.0])
    monkeypatch.setattr(capture.time, "monotonic", lambda: next(ticks, 16.0))
    with pytest.raises(ValueError, match="deadline exceeded"):
        capture.run_command(client, capture.VERSION_COMMAND)
    assert channel.closed

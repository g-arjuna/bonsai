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
        assert command in {capture.VERSION_COMMAND, capture.INTERFACE_COMMAND}
        return None, FakeStdout(self.channel), None


class FakeChannel:
    def __init__(self, stdout_chunks, stderr_chunks=(), exit_ready=True):
        self.stdout_chunks = list(stdout_chunks)
        self.stderr_chunks = list(stderr_chunks)
        self.exit_ready = exit_ready
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
        return 0

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

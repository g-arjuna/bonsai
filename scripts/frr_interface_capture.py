#!/usr/bin/env python3
"""Fixed read-only FRR interface-state capture over verified SSH.

The command set is intentionally hard-coded: this helper is not a general
remote-command executor. Passwords arrive only through BONSAI_CAPTURE_PASSWORD
so they never appear in process arguments. Unknown host keys are rejected.
"""
import argparse
import base64
import hashlib
import json
import os
import sys
import time

import paramiko

MAX_BYTES = 4 * 1024 * 1024
VERSION_COMMAND = "vtysh -c 'show version json'"
INTERFACE_COMMAND = "vtysh -c 'show interface json'"


def read_bounded(stream):
    data = stream.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("frr capture exceeds 4 MiB")
    return data.decode("utf-8", errors="strict")


def _append_bounded(retained, chunk, total):
    total += len(chunk)
    if len(retained) <= MAX_BYTES:
        remaining = (MAX_BYTES + 1) - len(retained)
        if remaining > 0:
            retained.extend(chunk[:remaining])
    return total


def run_command(client, command):
    """Run one fixed command while draining stdout and stderr independently.

    Both streams share the SSH channel window, so neither may starve the other.
    Reads are bounded per iteration and the absolute deadline is evaluated even
    when one stream remains continuously ready. Remote exit-status availability
    is not treated as proof that all pending output has been consumed.
    """
    _, stdout, _stderr = client.exec_command(command, timeout=15)
    channel = stdout.channel
    out = bytearray()
    err = bytearray()
    out_total = 0
    err_total = 0
    deadline = time.monotonic() + 15
    exit_status_ready = False
    while True:
        if time.monotonic() > deadline:
            channel.close()
            raise ValueError("fixed read-only FRR command deadline exceeded")
        progressed = False

        # At most one bounded chunk per stream per iteration prevents a
        # continuously-ready stdout stream from starving stderr or deadline
        # evaluation (and vice versa).
        if channel.recv_ready():
            progressed = True
            out_total = _append_bounded(out, channel.recv(65536), out_total)
            if out_total > MAX_BYTES:
                channel.close()
                raise ValueError("frr capture exceeds 4 MiB")
            if time.monotonic() > deadline:
                channel.close()
                raise ValueError("fixed read-only FRR command deadline exceeded")
        if channel.recv_stderr_ready():
            progressed = True
            err_total = _append_bounded(err, channel.recv_stderr(65536), err_total)
            if err_total > MAX_BYTES:
                channel.close()
                raise ValueError("frr capture exceeds 4 MiB")

        if channel.exit_status_ready():
            exit_status_ready = True
            # EOF/close with no pending bytes is the completion condition.
            # Merely observing an exit status is insufficient: final command
            # output can still be buffered in either stream.
            stream_closed = getattr(channel, "closed", False) or getattr(
                channel, "eof_received", False
            )
            if stream_closed and not channel.recv_ready() and not channel.recv_stderr_ready():
                break
        if not progressed:
            time.sleep(0.02)

    if not exit_status_ready:
        channel.close()
        raise ValueError("fixed read-only FRR command ended before exit status was available")
    status = channel.recv_exit_status()
    if status != 0:
        raise ValueError("fixed read-only FRR command failed")
    return bytes(out).decode("utf-8", errors="strict")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--username", required=True)
    parser.add_argument("--known-hosts")
    args = parser.parse_args()

    password = os.environ.get("BONSAI_CAPTURE_PASSWORD")
    if not password:
        raise ValueError("capture credential is unavailable")

    client = paramiko.SSHClient()
    # Endpoint trust must come only from the explicitly configured pin set.
    # Ambient system/user known_hosts are intentionally not inherited: they
    # are not the approved Bonsai acquisition trust anchor.
    if not args.known_hosts:
        raise ValueError("pinned SSH host-key database is required")
    client.load_host_keys(args.known_hosts)
    client.set_missing_host_key_policy(paramiko.RejectPolicy())
    try:
        client.connect(
            hostname=args.host,
            port=args.port,
            username=args.username,
            password=password,
            look_for_keys=False,
            allow_agent=False,
            timeout=10,
            banner_timeout=10,
            auth_timeout=10,
        )
        key = client.get_transport().get_remote_server_key()
        host_key = {
            "algorithm": key.get_name(),
            "sha256": "SHA256:"
            + base64.b64encode(hashlib.sha256(key.asbytes()).digest())
            .decode("ascii")
            .rstrip("="),
        }
        version_raw = run_command(client, VERSION_COMMAND)
        interface_raw = run_command(client, INTERFACE_COMMAND)
        version = json.loads(version_raw)
        # Parse the interface response only to prove the structured contract;
        # the raw bytes are preserved verbatim for the Rust normalizer.
        json.loads(interface_raw)
        print(
            json.dumps(
                {
                    "host": args.host,
                    "port": args.port,
                    "transport": "verified_ssh_read_only_cli",
                    "host_key": host_key,
                    "version_command": VERSION_COMMAND,
                    "interface_command": INTERFACE_COMMAND,
                    "version_output": version_raw,
                    "interface_output": interface_raw,
                    "release_raw": version.get("version"),
                    "device_hostname": version.get("hostName"),
                }
            )
        )
    finally:
        client.close()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        # Deliberately omit device output/stderr, which may contain operational
        # data; the Rust boundary maps this to a typed capture failure.
        print(f"frr interface collection failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)

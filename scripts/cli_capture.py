#!/usr/bin/env python3
"""SSH CLI capture helper for parser-chain enrichment.

The Rust runtime invokes this helper so Sprint 1 can activate parser-chain
capture without adding a new Rust SSH dependency surface. Passwords are read
from BONSAI_CAPTURE_PASSWORD to avoid leaking them into process args.
"""

from __future__ import annotations

import argparse
import json
import os
import sys

import paramiko

# Hard cap on captured device output. Running-config captures are typically
# well under 1 MiB; anything larger is rejected explicitly rather than being
# buffered and parsed without bound downstream.
MAX_CAPTURE_BYTES = 4 * 1024 * 1024


def read_bounded(stream, max_bytes: int) -> bytes:
    # Read via channel.recv so each call returns promptly with available data
    # (ChannelFile.read(n) may block trying to fill the whole buffer).
    channel = stream.channel
    chunks = []
    total = 0
    while True:
        chunk = channel.recv(65536)
        if not chunk:
            break
        total += len(chunk)
        if total > max_bytes:
            raise ValueError(f"capture exceeded {max_bytes} bytes")
        chunks.append(chunk)
    return b"".join(chunks)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--username", required=True)
    parser.add_argument("--command", required=True)
    parser.add_argument("--timeout-secs", type=int, default=20)
    parser.add_argument("--max-bytes", type=int, default=MAX_CAPTURE_BYTES)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    password = os.environ.get("BONSAI_CAPTURE_PASSWORD", "")
    if not password:
        print("BONSAI_CAPTURE_PASSWORD is not set", file=sys.stderr)
        return 2

    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        client.connect(
            hostname=args.host,
            port=args.port,
            username=args.username,
            password=password,
            look_for_keys=False,
            allow_agent=False,
            timeout=args.timeout_secs,
            banner_timeout=args.timeout_secs,
            auth_timeout=args.timeout_secs,
        )
        _, stdout, stderr = client.exec_command(args.command, timeout=args.timeout_secs)
        try:
            output = read_bounded(stdout, args.max_bytes).decode("utf-8", errors="replace")
            error_output = (
                read_bounded(stderr, args.max_bytes)
                .decode("utf-8", errors="replace")
                .strip()
            )
        except ValueError as exc:
            print(str(exc), file=sys.stderr)
            return 4
        exit_status = stdout.channel.recv_exit_status()
        if exit_status != 0:
            print(
                f"remote command failed with exit status {exit_status}: {error_output}",
                file=sys.stderr,
            )
            return 3
        print(
            json.dumps(
                {
                    "host": args.host,
                    "port": args.port,
                    "command": args.command,
                    "raw_output": output,
                }
            )
        )
        return 0
    except Exception as exc:  # pragma: no cover - exercised via runtime smoke
        print(str(exc), file=sys.stderr)
        return 1
    finally:
        client.close()


if __name__ == "__main__":
    raise SystemExit(main())

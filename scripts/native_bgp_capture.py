#!/usr/bin/env python3
"""Fixed read-only SR Linux state collection, with verified SSH host keys."""
import argparse
import json
import os
import sys
import paramiko

MAX_BYTES = 4 * 1024 * 1024
COMMANDS = {
    "system_information": "info from state system information | as json",
    "neighbors": "info from state with-context network-instance * protocols bgp neighbor * | as json",
}

def read_bounded(stream):
    data = stream.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("native capture exceeds 4 MiB")
    return data.decode("utf-8", errors="strict")

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--host", required=True)
    p.add_argument("--username", required=True)
    p.add_argument("--known-hosts")
    args = p.parse_args()
    password = os.environ.get("BONSAI_CAPTURE_PASSWORD")
    if not password:
        raise ValueError("capture credential is unavailable")
    client = paramiko.SSHClient()
    client.load_system_host_keys()
    if args.known_hosts:
        client.load_host_keys(args.known_hosts)
    client.set_missing_host_key_policy(paramiko.RejectPolicy())
    try:
        client.connect(args.host, username=args.username, password=password,
            look_for_keys=False, allow_agent=False, timeout=10, banner_timeout=10, auth_timeout=10)
        result = {}
        for name, command in COMMANDS.items():
            _, stdout, stderr = client.exec_command(command, timeout=12)
            raw = read_bounded(stdout)
            # Device output is not echoed into errors; SR CLI may exit zero on syntax errors.
            if stdout.channel.recv_exit_status() != 0:
                raise ValueError("native read-only CLI command failed")
            json.loads(raw)
            result[name] = raw
        print(json.dumps(result))
    finally:
        client.close()
    return 0

if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"native collection failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Fixed read-only FRR OSPF/IS-IS capture over verified SSH.

This helper intentionally exposes only four approved operational JSON commands.
It is not a general remote-command executor. Passwords arrive only through
BONSAI_CAPTURE_PASSWORD, unknown host keys are rejected, and optional protocol
family failures are represented independently rather than fabricating empty
successful populations.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import sys
import time

# Reuse the reviewed bounded-streaming, deadline and command-envelope
# implementation rather than maintaining a second SSH draining contract.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import paramiko  # noqa: E402

from frr_interface_capture import (  # noqa: E402
    BATCH_DEADLINE_SECS,
    COMMAND_DEADLINE_SECS,
    VERSION_COMMAND,
    execute_command,
    rfc3339_now,
)

OSPF_NEIGHBOR_COMMAND = "vtysh -c 'show ip ospf vrf all neighbor detail json'"
OSPF_LSDB_COMMAND = "vtysh -c 'show ip ospf vrf all database detail json'"
ISIS_NEIGHBOR_COMMAND = "vtysh -c 'show isis vrf all neighbor detail json'"
ISIS_LSDB_COMMAND = "vtysh -c 'show isis vrf all database detail json'"

COMMAND_BY_FAMILY = {
    "ospf_neighbors": OSPF_NEIGHBOR_COMMAND,
    "ospf_lsdb": OSPF_LSDB_COMMAND,
    "isis_neighbors": ISIS_NEIGHBOR_COMMAND,
    "isis_lsdb": ISIS_LSDB_COMMAND,
}
FAMILY_ORDER = list(COMMAND_BY_FAMILY)


def parse_families(raw):
    if raw is None or raw.strip() in {"", "all"}:
        return list(FAMILY_ORDER)
    families = []
    for item in raw.split(","):
        family = item.strip()
        if family not in COMMAND_BY_FAMILY:
            raise ValueError(f"unsupported fixed IGP capture family selector: {family}")
        if family not in families:
            families.append(family)
    return [family for family in FAMILY_ORDER if family in families]


def not_attempted_result(command_key, command, detail):
    from frr_interface_capture import command_result

    now_ns = time.time_ns()
    now = rfc3339_now()
    return command_result(
        command_key,
        command,
        "not_attempted",
        now,
        now,
        now_ns,
        now_ns,
        detail=detail,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--username", required=True)
    parser.add_argument("--known-hosts", required=True)
    parser.add_argument(
        "--families",
        default="all",
        help="comma-separated subset of: " + ",".join(FAMILY_ORDER),
    )
    args = parser.parse_args()

    password = os.environ.get("BONSAI_CAPTURE_PASSWORD")
    if not password:
        raise ValueError("capture credential is unavailable")
    families = parse_families(args.families)

    client = paramiko.SSHClient()
    # Endpoint trust comes only from the explicitly configured pin set; ambient
    # system/user known_hosts are not an approved Bonsai acquisition anchor.
    client.load_host_keys(args.known_hosts)
    client.set_missing_host_key_policy(paramiko.RejectPolicy())
    try:
        batch_started_at = rfc3339_now()
        batch_started_at_ns = time.time_ns()
        batch_deadline = time.monotonic() + BATCH_DEADLINE_SECS

        def remaining_budget():
            return max(0.0, batch_deadline - time.monotonic())

        client.connect(
            hostname=args.host,
            port=args.port,
            username=args.username,
            password=password,
            look_for_keys=False,
            allow_agent=False,
            timeout=min(10, max(1, int(remaining_budget()))),
            banner_timeout=min(10, max(1, int(remaining_budget()))),
            auth_timeout=min(10, max(1, int(remaining_budget()))),
        )
        key = client.get_transport().get_remote_server_key()
        host_key = {
            "algorithm": key.get_name(),
            "sha256": "SHA256:"
            + base64.b64encode(hashlib.sha256(key.asbytes()).digest())
            .decode("ascii")
            .rstrip("="),
        }

        commands = {}
        version_result = execute_command(client, "version", VERSION_COMMAND, remaining_budget)
        commands["version"] = version_result
        if version_result["outcome"] != "complete" or not version_result.get("output"):
            raise ValueError("mandatory FRR version/release identity capture failed")
        version = json.loads(version_result["output"])
        if not version.get("version"):
            raise ValueError("mandatory FRR release identity is unavailable")

        for family in FAMILY_ORDER:
            command = COMMAND_BY_FAMILY[family]
            if family not in families:
                commands[family] = not_attempted_result(
                    family,
                    command,
                    "protocol family was not applicable in the approved acquisition plan",
                )
                continue
            commands[family] = execute_command(client, family, command, remaining_budget)

        batch_completed_at_ns = time.time_ns()
        print(
            json.dumps(
                {
                    "host": args.host,
                    "port": args.port,
                    "transport": "verified_ssh_read_only_cli",
                    "host_key": host_key,
                    "acquisition_contract": "frr_verified_ssh_igp_snapshot_v1",
                    "acquisition_batch": {
                        "started_at": batch_started_at,
                        "started_at_ns": batch_started_at_ns,
                        "completed_at": rfc3339_now(),
                        "completed_at_ns": batch_completed_at_ns,
                        "deadline_seconds": BATCH_DEADLINE_SECS,
                        "command_deadline_seconds": COMMAND_DEADLINE_SECS,
                        "atomic_snapshot": False,
                        "approved_families": families,
                    },
                    "commands": commands,
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
        # data; callers classify this as a failed capture rather than empty IGP.
        print(f"frr IGP collection failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)

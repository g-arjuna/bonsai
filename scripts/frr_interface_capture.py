#!/usr/bin/env python3
"""Fixed read-only FRR Layer-2 capture over verified SSH.

The command set is intentionally hard-coded: this helper is not a general
remote-command executor. Passwords arrive only through BONSAI_CAPTURE_PASSWORD
so they never appear in process arguments. Unknown host keys are rejected.
Optional feature-family command failures are reported per family; transport,
host-key and version/release identity failures remain fail-closed.
"""
import argparse
import base64
import hashlib
import json
import os
import sys
import time
from datetime import datetime, timezone

import paramiko

MAX_BYTES = 4 * 1024 * 1024
COMMAND_DEADLINE_SECS = 15
BATCH_DEADLINE_SECS = 40
VERSION_COMMAND = "vtysh -c 'show version json'"
INTERFACE_COMMAND = "vtysh -c 'show interface json'"
NEIGHBOR_COMMAND = "ip neigh show"
MAC_COMMAND = "bridge fdb show"
VLAN_COMMAND = "bridge -c vlan show"

COMMAND_BY_FAMILY = {
    "interfaces": INTERFACE_COMMAND,
    "arpnd": NEIGHBOR_COMMAND,
    "mac": MAC_COMMAND,
    "vlan": VLAN_COMMAND,
}
FAMILY_ORDER = ["interfaces", "arpnd", "mac", "vlan"]


class CommandFailure(ValueError):
    def __init__(self, kind, message, stderr_bytes=0):
        super().__init__(message)
        self.kind = kind
        self.stderr_bytes = stderr_bytes


def rfc3339_now():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def digest_text(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _append_bounded(retained, chunk, total):
    total += len(chunk)
    if len(retained) <= MAX_BYTES:
        remaining = (MAX_BYTES + 1) - len(retained)
        if remaining > 0:
            retained.extend(chunk[:remaining])
    return total


def run_command(client, command, timeout_seconds=COMMAND_DEADLINE_SECS):
    """Run one fixed command while draining stdout and stderr independently.

    Both streams share the SSH channel window, so neither may starve the other.
    Reads are bounded per iteration and the absolute deadline is evaluated even
    when one stream remains continuously ready. Remote exit-status availability
    is not treated as proof that all pending output has been consumed.
    """
    timeout_seconds = max(0.1, min(COMMAND_DEADLINE_SECS, timeout_seconds))
    _, stdout, _stderr = client.exec_command(command, timeout=timeout_seconds)
    channel = stdout.channel
    out = bytearray()
    err = bytearray()
    out_total = 0
    err_total = 0
    deadline = time.monotonic() + timeout_seconds
    exit_status_ready = False
    while True:
        if time.monotonic() > deadline:
            channel.close()
            raise CommandFailure(
                "timed_out",
                "fixed read-only FRR command deadline exceeded",
                err_total,
            )
        progressed = False

        # At most one bounded chunk per stream per iteration prevents a
        # continuously-ready stdout stream from starving stderr or deadline
        # evaluation (and vice versa).
        if channel.recv_ready():
            progressed = True
            out_total = _append_bounded(out, channel.recv(65536), out_total)
            if out_total > MAX_BYTES:
                channel.close()
                raise CommandFailure("truncated", "frr capture exceeds 4 MiB", err_total)
            if time.monotonic() > deadline:
                channel.close()
                raise CommandFailure(
                    "timed_out",
                    "fixed read-only FRR command deadline exceeded",
                    err_total,
                )
        if channel.recv_stderr_ready():
            progressed = True
            err_total = _append_bounded(err, channel.recv_stderr(65536), err_total)
            if err_total > MAX_BYTES:
                channel.close()
                raise CommandFailure("truncated", "frr capture exceeds 4 MiB", err_total)

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
        raise CommandFailure(
            "failed",
            "fixed read-only FRR command ended before exit status was available",
            err_total,
        )
    status = channel.recv_exit_status()
    if status != 0:
        stderr_text = bytes(err).decode("utf-8", errors="ignore").lower()
        unsupported_markers = (
            "command not found",
            "unknown command",
            "not supported",
            "no such file",
            "operation not supported",
        )
        kind = "unsupported" if any(marker in stderr_text for marker in unsupported_markers) else "failed"
        raise CommandFailure(kind, "fixed read-only FRR command failed", err_total)
    return bytes(out).decode("utf-8", errors="strict")


def command_result(command_key, command, outcome, started_at, completed_at, started_at_ns, completed_at_ns, output=None, detail=None, stderr_bytes=None):
    result = {
        "command_key": command_key,
        "command": command,
        "outcome": outcome,
        "started_at": started_at,
        "completed_at": completed_at,
        "started_at_ns": started_at_ns,
        "completed_at_ns": completed_at_ns,
    }
    if output is not None:
        result["output"] = output
        result["output_bytes"] = len(output.encode("utf-8"))
        result["output_sha256"] = digest_text(output)
    else:
        result["output"] = None
        result["output_bytes"] = None
        result["output_sha256"] = None
    if detail:
        result["detail"] = detail
    if stderr_bytes is not None:
        result["stderr_bytes"] = stderr_bytes
    return result


def execute_command(client, command_key, command, remaining_budget):
    started_at = rfc3339_now()
    started_at_ns = time.time_ns()
    if remaining_budget() <= 0.25:
        completed_at = rfc3339_now()
        return command_result(
            command_key,
            command,
            "not_attempted",
            started_at,
            completed_at,
            started_at_ns,
            time.time_ns(),
            detail="acquisition batch deadline budget exhausted",
        )
    try:
        output = run_command(client, command, min(COMMAND_DEADLINE_SECS, remaining_budget()))
        completed_at_ns = time.time_ns()
        return command_result(
            command_key,
            command,
            "complete",
            started_at,
            rfc3339_now(),
            started_at_ns,
            completed_at_ns,
            output=output,
        )
    except CommandFailure as error:
        completed_at_ns = time.time_ns()
        return command_result(
            command_key,
            command,
            error.kind,
            started_at,
            rfc3339_now(),
            started_at_ns,
            completed_at_ns,
            detail=str(error),
            stderr_bytes=error.stderr_bytes,
        )
    except (UnicodeDecodeError, paramiko.SSHException, OSError, ValueError) as error:
        completed_at_ns = time.time_ns()
        return command_result(
            command_key,
            command,
            "failed",
            started_at,
            rfc3339_now(),
            started_at_ns,
            completed_at_ns,
            detail=f"{type(error).__name__}",
        )


def not_attempted_result(command_key, command, detail):
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


def parse_families(raw):
    if raw is None or raw.strip() in {"", "all"}:
        return list(FAMILY_ORDER)
    families = []
    for item in raw.split(","):
        family = item.strip()
        if family not in COMMAND_BY_FAMILY:
            raise ValueError(f"unsupported fixed capture family selector: {family}")
        if family not in families:
            families.append(family)
    return [family for family in FAMILY_ORDER if family in families]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--port", type=int, default=22)
    parser.add_argument("--username", required=True)
    parser.add_argument("--known-hosts")
    parser.add_argument(
        "--families",
        default="all",
        help="comma-separated subset of the fixed approved families: interfaces,arpnd,mac,vlan",
    )
    args = parser.parse_args()

    password = os.environ.get("BONSAI_CAPTURE_PASSWORD")
    if not password:
        raise ValueError("capture credential is unavailable")
    families = parse_families(args.families)

    client = paramiko.SSHClient()
    # Endpoint trust must come only from the explicitly configured pin set.
    # Ambient system/user known_hosts are intentionally not inherited: they
    # are not the approved Bonsai acquisition trust anchor.
    if not args.known_hosts:
        raise ValueError("pinned SSH host-key database is required")
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
                    "feature family was not applicable in the approved acquisition plan",
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
                    "acquisition_batch": {
                        "started_at": batch_started_at,
                        "started_at_ns": batch_started_at_ns,
                        "completed_at": rfc3339_now(),
                        "completed_at_ns": batch_completed_at_ns,
                        "deadline_seconds": BATCH_DEADLINE_SECS,
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
        # data; the Rust boundary maps this to a typed capture failure.
        print(f"frr interface collection failed: {type(error).__name__}", file=sys.stderr)
        sys.exit(1)

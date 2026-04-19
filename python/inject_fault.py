#!/usr/bin/env python3
"""Programmatic fault injector for the bonsai-p4 lab.

Reads node credentials from bonsai.toml so nothing is hardcoded.
Supports BGP session kill/restore, interface down/up, and gradual netem impairment.

Usage:
    # Disable one BGP neighbor on spine1
    python python/inject_fault.py bgp-down srl-spine1 10.0.12.1

    # Re-enable it
    python python/inject_fault.py bgp-up srl-spine1 10.0.12.1

    # Take an interface down (and back up)
    python python/inject_fault.py iface-down srl-spine1 ethernet-1/1
    python python/inject_fault.py iface-up   srl-spine1 ethernet-1/1

    # Apply 5% packet loss via ContainerLab netem (run from clab host)
    python python/inject_fault.py netem-loss srl-spine1 e1-1 5
    python python/inject_fault.py netem-clear srl-spine1 e1-1

    # Quick BGP flap: down then up after N seconds
    python python/inject_fault.py bgp-flap srl-spine1 10.0.12.1 --hold 10

Vendor detection: reads the `vendor` field from bonsai.toml [[target]] blocks.
  nokia_srl  → uses sr_cli wrapper
  cisco_xrd  → uses XR EXEC / config mode
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import time
import tomllib
from pathlib import Path

import paramiko


CONFIG_PATH   = "bonsai.toml"
TOPOLOGY_NAME = "bonsai-p4"          # clab topology name for netem commands
SSH_TIMEOUT   = 10                    # seconds
CMD_TIMEOUT   = 15                    # seconds for SSH channel read


# ── bonsai.toml loader ────────────────────────────────────────────────────────

def _load_targets(cfg_path: str = CONFIG_PATH) -> dict[str, dict]:
    """Return {hostname: {address, username, password, vendor}} from bonsai.toml."""
    path = Path(cfg_path)
    if not path.exists():
        sys.exit(f"ERROR: {cfg_path} not found — copy bonsai.toml.example and fill in credentials")
    with open(path, "rb") as f:
        cfg = tomllib.load(f)
    targets = {}
    for t in cfg.get("target", []):
        hostname = t.get("hostname", "")
        if not hostname:
            continue
        # Extract mgmt IP from address field (strip port)
        addr = t.get("address", "").split(":")[0]
        targets[hostname] = {
            "address":  addr,
            "username": t.get("username", "admin"),
            "password": t.get("password", ""),
            "vendor":   t.get("vendor", _guess_vendor(t)),
        }
    return targets


def _guess_vendor(target: dict) -> str:
    """Guess vendor from hostname when `vendor` field is absent."""
    hostname = target.get("hostname", "").lower()
    if "xrd" in hostname or "xr" in hostname:
        return "cisco_xrd"
    return "nokia_srl"


# ── SSH helpers ───────────────────────────────────────────────────────────────

def _ssh_connect(address: str, username: str, password: str) -> paramiko.SSHClient:
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        address, port=22, username=username, password=password,
        timeout=SSH_TIMEOUT, look_for_keys=False, allow_agent=False,
    )
    return client


def _run_srl(address: str, username: str, password: str, command: str) -> str:
    """Run a configuration command on an SRL node.

    SSH to SRL drops directly into the SRL CLI (not bash), so set commands
    require entering candidate mode first then committing.
    """
    import re
    _ansi = re.compile(r'\x1b\[[0-9;?]*[a-zA-Z]|\x1b\([a-zA-Z]|\x1b[=>]|\r')

    client = _ssh_connect(address, username, password)
    try:
        shell = client.invoke_shell()
        time.sleep(1.2)
        shell.recv(8192)  # drain banner/prompt

        # Exclusive candidate prevents conflicts from stale shared-candidate changes
        for cmd in ["enter candidate exclusive", command, "commit now"]:
            shell.send(cmd + "\n")
            time.sleep(0.8)

        time.sleep(1.5)  # let commit complete
        chunks = []
        while shell.recv_ready():
            chunks.append(shell.recv(8192).decode(errors="replace"))
        out = "".join(chunks)
        shell.close()
        # Return last non-empty line as the status indicator
        clean = _ansi.sub("", out).strip()
        last_line = next((l.strip() for l in reversed(clean.splitlines()) if l.strip()), "ok")
        return last_line
    finally:
        client.close()


def _run_xrd(address: str, username: str, password: str, commands: list[str]) -> str:
    """Run a list of XR exec/config commands in an interactive shell."""
    client = _ssh_connect(address, username, password)
    try:
        shell = client.invoke_shell()
        time.sleep(1)
        shell.recv(4096)   # drain banner

        output = []
        for cmd in commands:
            shell.send(cmd + "\n")
            time.sleep(0.8)
            chunk = shell.recv(4096).decode(errors="replace")
            output.append(chunk)

        shell.close()
        return "\n".join(output)
    finally:
        client.close()


# ── SRL fault actions ─────────────────────────────────────────────────────────

def srl_bgp_disable(address: str, username: str, password: str, peer: str) -> None:
    cmd = f"set / network-instance default protocols bgp neighbor {peer} admin-state disable"
    out = _run_srl(address, username, password, cmd)
    print(f"  SRL BGP disable {peer}: {out or 'ok'}")


def srl_bgp_enable(address: str, username: str, password: str, peer: str) -> None:
    cmd = f"set / network-instance default protocols bgp neighbor {peer} admin-state enable"
    out = _run_srl(address, username, password, cmd)
    print(f"  SRL BGP enable {peer}: {out or 'ok'}")


def srl_iface_down(address: str, username: str, password: str, iface: str) -> None:
    cmd = f"set / interface {iface} admin-state disable"
    out = _run_srl(address, username, password, cmd)
    print(f"  SRL interface down {iface}: {out or 'ok'}")


def srl_iface_up(address: str, username: str, password: str, iface: str) -> None:
    cmd = f"set / interface {iface} admin-state enable"
    out = _run_srl(address, username, password, cmd)
    print(f"  SRL interface up {iface}: {out or 'ok'}")


# ── XRd fault actions ─────────────────────────────────────────────────────────

def xrd_bgp_disable(address: str, username: str, password: str, peer: str) -> None:
    cmds = ["configure", f"router bgp 65100 neighbor {peer} shutdown", "commit", "end"]
    out = _run_xrd(address, username, password, cmds)
    print(f"  XRd BGP disable {peer}: configured")


def xrd_bgp_enable(address: str, username: str, password: str, peer: str) -> None:
    cmds = ["configure", f"router bgp 65100 neighbor {peer} no shutdown", "commit", "end"]
    out = _run_xrd(address, username, password, cmds)
    print(f"  XRd BGP enable {peer}: configured")


def xrd_iface_down(address: str, username: str, password: str, iface: str) -> None:
    cmds = ["configure", f"interface {iface} shutdown", "commit", "end"]
    out = _run_xrd(address, username, password, cmds)
    print(f"  XRd interface down {iface}: configured")


def xrd_iface_up(address: str, username: str, password: str, iface: str) -> None:
    cmds = ["configure", f"interface {iface} no shutdown", "commit", "end"]
    out = _run_xrd(address, username, password, cmds)
    print(f"  XRd interface up {iface}: configured")


# ── netem (runs clab on host, not via SSH) ────────────────────────────────────

def netem_loss(node_name: str, iface: str, loss_pct: float, topology: str = TOPOLOGY_NAME) -> None:
    """Apply packet loss via `clab tools netem`. Requires clab on PATH."""
    cmd = ["clab", "tools", "netem", "set", topology, node_name, iface,
           "--loss", str(loss_pct)]
    print(f"  netem: {' '.join(cmd)}")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"  [netem error] {result.stderr.strip()}", file=sys.stderr)
    else:
        print(f"  netem loss {loss_pct}% applied to {node_name}:{iface}")


def netem_delay(node_name: str, iface: str, delay_ms: int, topology: str = TOPOLOGY_NAME) -> None:
    cmd = ["clab", "tools", "netem", "set", topology, node_name, iface,
           "--delay", f"{delay_ms}ms"]
    print(f"  netem: {' '.join(cmd)}")
    subprocess.run(cmd, capture_output=True, text=True)
    print(f"  netem delay {delay_ms}ms applied to {node_name}:{iface}")


def netem_clear(node_name: str, iface: str, topology: str = TOPOLOGY_NAME) -> None:
    cmd = ["clab", "tools", "netem", "del", topology, node_name, iface]
    print(f"  netem: {' '.join(cmd)}")
    result = subprocess.run(cmd, capture_output=True, text=True)
    print(f"  netem cleared on {node_name}:{iface}")


# ── dispatch ──────────────────────────────────────────────────────────────────

def _get_target(targets: dict, hostname: str) -> dict:
    if hostname not in targets:
        sys.exit(f"ERROR: hostname '{hostname}' not found in bonsai.toml — "
                 f"available: {list(targets)}")
    return targets[hostname]


def dispatch_bgp_down(targets: dict, hostname: str, peer: str) -> None:
    t = _get_target(targets, hostname)
    print(f"[{time.strftime('%H:%M:%S')}] BGP DOWN: {hostname} peer {peer}")
    if "xrd" in t["vendor"]:
        xrd_bgp_disable(t["address"], t["username"], t["password"], peer)
    else:
        srl_bgp_disable(t["address"], t["username"], t["password"], peer)


def dispatch_bgp_up(targets: dict, hostname: str, peer: str) -> None:
    t = _get_target(targets, hostname)
    print(f"[{time.strftime('%H:%M:%S')}] BGP UP: {hostname} peer {peer}")
    if "xrd" in t["vendor"]:
        xrd_bgp_enable(t["address"], t["username"], t["password"], peer)
    else:
        srl_bgp_enable(t["address"], t["username"], t["password"], peer)


def dispatch_iface_down(targets: dict, hostname: str, iface: str) -> None:
    t = _get_target(targets, hostname)
    print(f"[{time.strftime('%H:%M:%S')}] IFACE DOWN: {hostname} {iface}")
    if "xrd" in t["vendor"]:
        xrd_iface_down(t["address"], t["username"], t["password"], iface)
    else:
        srl_iface_down(t["address"], t["username"], t["password"], iface)


def dispatch_iface_up(targets: dict, hostname: str, iface: str) -> None:
    t = _get_target(targets, hostname)
    print(f"[{time.strftime('%H:%M:%S')}] IFACE UP: {hostname} {iface}")
    if "xrd" in t["vendor"]:
        xrd_iface_up(t["address"], t["username"], t["password"], iface)
    else:
        srl_iface_up(t["address"], t["username"], t["password"], iface)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    ap = argparse.ArgumentParser(
        description="Inject faults into the bonsai-p4 ContainerLab topology"
    )
    ap.add_argument("--config", default=CONFIG_PATH, help="Path to bonsai.toml")
    ap.add_argument("--topology", default=TOPOLOGY_NAME, help="ContainerLab topology name")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("bgp-down",  help="Disable a BGP neighbor")
    p.add_argument("hostname"); p.add_argument("peer")

    p = sub.add_parser("bgp-up",    help="Re-enable a BGP neighbor")
    p.add_argument("hostname"); p.add_argument("peer")

    p = sub.add_parser("bgp-flap",  help="Disable then re-enable after --hold seconds")
    p.add_argument("hostname"); p.add_argument("peer")
    p.add_argument("--hold", type=int, default=15, help="Seconds to hold down (default 15)")

    p = sub.add_parser("iface-down", help="Set interface admin-state disable")
    p.add_argument("hostname"); p.add_argument("iface")

    p = sub.add_parser("iface-up",   help="Set interface admin-state enable")
    p.add_argument("hostname"); p.add_argument("iface")

    p = sub.add_parser("iface-flap", help="Interface down then up after --hold seconds")
    p.add_argument("hostname"); p.add_argument("iface")
    p.add_argument("--hold", type=int, default=15)

    p = sub.add_parser("netem-loss",  help="Apply packet loss (clab tools netem)")
    p.add_argument("hostname"); p.add_argument("iface"); p.add_argument("loss_pct", type=float)

    p = sub.add_parser("netem-delay", help="Apply delay (clab tools netem)")
    p.add_argument("hostname"); p.add_argument("iface"); p.add_argument("delay_ms", type=int)

    p = sub.add_parser("netem-clear", help="Remove netem impairment")
    p.add_argument("hostname"); p.add_argument("iface")

    args = ap.parse_args()
    targets = _load_targets(args.config)

    if args.cmd == "bgp-down":
        dispatch_bgp_down(targets, args.hostname, args.peer)

    elif args.cmd == "bgp-up":
        dispatch_bgp_up(targets, args.hostname, args.peer)

    elif args.cmd == "bgp-flap":
        dispatch_bgp_down(targets, args.hostname, args.peer)
        print(f"  holding down for {args.hold}s...")
        time.sleep(args.hold)
        dispatch_bgp_up(targets, args.hostname, args.peer)

    elif args.cmd == "iface-down":
        dispatch_iface_down(targets, args.hostname, args.iface)

    elif args.cmd == "iface-up":
        dispatch_iface_up(targets, args.hostname, args.iface)

    elif args.cmd == "iface-flap":
        dispatch_iface_down(targets, args.hostname, args.iface)
        print(f"  holding down for {args.hold}s...")
        time.sleep(args.hold)
        dispatch_iface_up(targets, args.hostname, args.iface)

    elif args.cmd == "netem-loss":
        netem_loss(args.hostname, args.iface, args.loss_pct, args.topology)

    elif args.cmd == "netem-delay":
        netem_delay(args.hostname, args.iface, args.delay_ms, args.topology)

    elif args.cmd == "netem-clear":
        netem_clear(args.hostname, args.iface, args.topology)


if __name__ == "__main__":
    main()

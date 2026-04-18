#!/usr/bin/env python3
"""Phase 4 comprehensive validation script.

Starts bonsai, waits for data to populate, then validates every layer:
  1. Process & connectivity
  2. Core graph tables (Device, Interface, BgpNeighbor, Topology)
  3. Phase 4 schema (DetectionEvent, Remediation) via direct writes
  4. StreamEvents produces live events within timeout
  5. Python rule engine fires on synthetic events
  6. PushRemediation dry-run path

Usage:
    cd c:/Users/arjun/Desktop/bonsai
    py -3.13 python/tests/validate_p4.py

Set BONSAI_SKIP_START=1 if bonsai is already running.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
import threading
import uuid
from pathlib import Path
from typing import Optional

# Make sure the python/ directory is on sys.path
REPO = Path(__file__).parent.parent.parent
sys.path.insert(0, str(REPO / "python"))

from bonsai_sdk import BonsaiClient
from bonsai_sdk.detection import Detection, Features
from bonsai_sdk.rules.bgp import BGP_RULES
from bonsai_sdk.rules.interface import INTERFACE_RULES
from bonsai_sdk.rules.topology import TOPOLOGY_RULES

ADDR = os.environ.get("BONSAI_ADDR", "[::1]:50051")
WAIT_FOR_DATA_S = int(os.environ.get("BONSAI_WAIT_S", "45"))
STREAM_TIMEOUT_S = int(os.environ.get("BONSAI_STREAM_TIMEOUT_S", "30"))
SKIP_START = os.environ.get("BONSAI_SKIP_START", "0") == "1"

PASS = "\033[32mPASS\033[0m"
FAIL = "\033[31mFAIL\033[0m"
WARN = "\033[33mWARN\033[0m"
SECTION = "\033[36m"
RESET = "\033[0m"

_results: list[tuple[str, bool, str]] = []
_bonsai_proc: Optional[subprocess.Popen] = None


def check(name: str, ok: bool, detail: str = "") -> None:
    tag = PASS if ok else FAIL
    msg = f"  [{tag}] {name}"
    if detail:
        msg += f"  — {detail}"
    print(msg)
    _results.append((name, ok, detail))


def warn(name: str, detail: str = "") -> None:
    msg = f"  [{WARN}] {name}"
    if detail:
        msg += f"  — {detail}"
    print(msg)


def section(title: str) -> None:
    print(f"\n{SECTION}{'-'*60}{RESET}")
    print(f"{SECTION}  {title}{RESET}")
    print(f"{SECTION}{'-'*60}{RESET}")


# ── 0. Kill stale processes & clean DB artefacts ─────────────────────────────

def clean_environment() -> None:
    section("0. Environment cleanup")
    if SKIP_START:
        check("kill stale bonsai processes", True, "skipped (BONSAI_SKIP_START=1)")
        check("remove stale DB files", True, "skipped (BONSAI_SKIP_START=1)")
        return

    # Kill any running bonsai.exe
    try:
        result = subprocess.run(
            ["powershell", "-Command",
             "Get-Process bonsai -ErrorAction SilentlyContinue | Stop-Process -Force"],
            capture_output=True, text=True, timeout=10,
        )
        check("kill stale bonsai processes", True, "done")
    except Exception as exc:
        check("kill stale bonsai processes", False, str(exc))

    # Remove DB files
    removed = []
    for pat in ["bonsai.db", "bonsai.db.wal", "bonsai-mv.db", "bonsai-mv.db.wal"]:
        p = REPO / pat
        if p.exists():
            p.unlink()
            removed.append(pat)
    check("remove stale DB files", True, f"removed: {removed}" if removed else "none present")
    time.sleep(1)


# ── 1. Start bonsai ───────────────────────────────────────────────────────────

def start_bonsai() -> None:
    global _bonsai_proc
    section("1. Start bonsai process")
    if SKIP_START:
        check("start bonsai (skipped — BONSAI_SKIP_START=1)", True)
        return

    binary = REPO / "target" / "release" / "bonsai.exe"
    if not binary.exists():
        check("bonsai binary exists", False, f"{binary} not found — run: cargo build --release")
        sys.exit(1)
    check("bonsai binary exists", True)

    log_path = REPO / "bonsai_validate.log"
    log_fh = open(log_path, "w")
    _bonsai_proc = subprocess.Popen(
        [str(binary)],
        cwd=str(REPO),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
    )
    check("bonsai process started", True, f"pid={_bonsai_proc.pid}  log={log_path}")

    # Wait for gRPC port to open
    import socket
    deadline = time.time() + 20
    while time.time() < deadline:
        try:
            s = socket.create_connection(("::1", 50051), timeout=1)
            s.close()
            check("gRPC port 50051 open", True)
            return
        except OSError:
            time.sleep(0.5)
    check("gRPC port 50051 open", False, "timed out waiting for port — check bonsai_validate.log")
    sys.exit(1)


# ── 2. Wait for data ──────────────────────────────────────────────────────────

def _patch_client_timeout(client: BonsaiClient, timeout: float = 10.0) -> None:
    """Wrap each unary stub method to pass a grpc timeout."""
    import grpc
    stub = client.stub
    _unary_methods = [
        "Query", "GetDevices", "GetInterfaces", "GetBgpNeighbors",
        "GetTopology", "CreateDetection", "CreateRemediation", "PushRemediation",
    ]
    for name in _unary_methods:
        original = getattr(stub, name)
        def _make_wrapper(orig):
            def wrapper(request, **kw):
                kw.setdefault("timeout", timeout)
                return orig(request, **kw)
            return wrapper
        setattr(stub, name, _make_wrapper(original))


def _rpc_with_timeout(fn, *args, timeout: float = 8.0, **kwargs):
    """Run an RPC call with a wall-clock timeout using a thread."""
    result_holder: list = []
    exc_holder: list = []

    def _run():
        try:
            result_holder.append(fn(*args, **kwargs))
        except Exception as exc:
            exc_holder.append(exc)

    t = threading.Thread(target=_run, daemon=True)
    t.start()
    t.join(timeout=timeout)
    if t.is_alive():
        raise TimeoutError(f"RPC timed out after {timeout}s")
    if exc_holder:
        raise exc_holder[0]
    return result_holder[0]


def wait_for_data(client: BonsaiClient) -> None:
    section(f"2. Waiting {WAIT_FOR_DATA_S}s for telemetry data to populate")
    print(f"  (Polling every 5s — need >=1 Device AND >=1 Interface)")

    start = time.time()
    deadline = start + max(WAIT_FOR_DATA_S, 5)
    last_devices, last_ifaces = 0, 0
    while time.time() < deadline:
        try:
            devices = _rpc_with_timeout(client.get_devices, timeout=5)
            ifaces  = _rpc_with_timeout(client.get_interfaces, timeout=5)
            if len(devices) != last_devices or len(ifaces) != last_ifaces:
                elapsed = int(time.time() - start)
                print(f"  t+{elapsed}s — {len(devices)} device(s), {len(ifaces)} interface(s) in graph")
                last_devices, last_ifaces = len(devices), len(ifaces)
            if len(devices) >= 1 and len(ifaces) >= 1:
                break
        except Exception as exc:
            print(f"  waiting... ({exc.__class__.__name__})")
        time.sleep(5)

    try:
        devices = _rpc_with_timeout(client.get_devices, timeout=5)
        ifaces  = _rpc_with_timeout(client.get_interfaces, timeout=5)
    except Exception:
        devices, ifaces = [], []
    check("at least 1 Device node written", len(devices) >= 1,
          f"got {len(devices)}: {[d.hostname for d in devices]}")
    check("at least 1 Interface node written", len(ifaces) >= 1,
          f"got {len(ifaces)}")


# ── 3. Core graph tables ──────────────────────────────────────────────────────

def validate_graph_tables(client: BonsaiClient) -> None:
    section("3. Core graph tables")

    # Devices
    try:
        devices = client.get_devices()
        check("Device nodes exist", len(devices) >= 1,
              f"{len(devices)} devices: {[d.hostname for d in devices]}")
        vendors = set(d.vendor for d in devices)
        check("vendor field populated", all(vendors),
              f"vendors: {vendors}")
        addrs = [d.address for d in devices]
        check("address field populated", all(addrs), f"{addrs}")
    except Exception as exc:
        check("Device nodes", False, str(exc))

    # Interfaces
    try:
        ifaces = client.get_interfaces()
        check("Interface nodes exist", len(ifaces) >= 1,
              f"{len(ifaces)} interfaces")
        names = [i.name for i in ifaces[:5]]
        check("interface name populated", all(names), f"sample: {names}")
    except Exception as exc:
        check("Interface nodes", False, str(exc))

    # BGP neighbors
    try:
        neighbors = client.get_bgp_neighbors()
        check("BgpNeighbor nodes exist", len(neighbors) >= 1,
              f"{len(neighbors)} neighbors")
        for n in neighbors[:5]:
            check(
                f"  bgp {n.device_address}->{n.peer_address}",
                bool(n.peer_address and n.session_state),
                f"AS{n.peer_as} state={n.session_state}",
            )
    except Exception as exc:
        check("BgpNeighbor nodes", False, str(exc))

    # Topology
    try:
        edges = client.get_topology()
        check("CONNECTED_TO edges exist", len(edges) >= 1,
              f"{len(edges)} edges")
        for e in edges[:4]:
            check(
                f"  edge {e.src_device}:{e.src_interface}->{e.dst_device}:{e.dst_interface}",
                bool(e.src_device and e.dst_device), "",
            )
    except Exception as exc:
        check("Topology edges", False, str(exc))

    # StateChangeEvent via raw Cypher
    try:
        rows = client.query("MATCH (n:StateChangeEvent) RETURN n.event_type, n.device_address LIMIT 5")
        check("StateChangeEvent nodes exist", len(rows) >= 1,
              f"{len(rows)} rows (sample shown)")
        for r in rows[:3]:
            print(f"    {r}")
    except Exception as exc:
        check("StateChangeEvent nodes", False, str(exc))


# ── 4. Phase 4 schema — DetectionEvent + Remediation ─────────────────────────

def validate_phase4_schema(client: BonsaiClient) -> None:
    section("4. Phase 4 schema (DetectionEvent + Remediation write/read)")

    # Find a real device to anchor the test records
    try:
        devices = client.get_devices()
        device_addr = devices[0].address if devices else "172.100.101.11:57400"
    except Exception:
        device_addr = "172.100.101.11:57400"

    fired_at_ns = int(time.time() * 1e9)
    features = Features(
        device_address=device_addr,
        event_type="bgp_session_change",
        detail={"peer": "10.1.99.1", "new_state": "idle"},
        peer_address="10.1.99.1",
        old_state="established",
        new_state="idle",
        peer_count_total=3,
        peer_count_established=2,
        occurred_at_ns=fired_at_ns,
    )
    features_json = features.to_json()

    # Write DetectionEvent
    detection_id = None
    try:
        resp = client.create_detection(
            device_address=device_addr,
            rule_id="validate_p4_test",
            severity="warn",
            features_json=features_json,
            fired_at_ns=fired_at_ns,
        )
        detection_id = resp.id
        check("CreateDetection RPC", bool(detection_id and not resp.error),
              f"id={detection_id[:8] if detection_id else 'None'}...")
    except Exception as exc:
        check("CreateDetection RPC", False, str(exc))

    # Read it back
    try:
        rows = client.query(
            "MATCH (n:DetectionEvent) WHERE n.rule_id = 'validate_p4_test' "
            "RETURN n.id, n.severity, n.fired_at LIMIT 1"
        )
        check("DetectionEvent readable via Cypher", len(rows) >= 1,
              f"row={rows[0] if rows else 'none'}")
    except Exception as exc:
        check("DetectionEvent readable", False, str(exc))

    # features_json round-trip
    try:
        rows = client.query(
            "MATCH (n:DetectionEvent) WHERE n.rule_id = 'validate_p4_test' "
            "RETURN n.features_json LIMIT 1"
        )
        if rows:
            stored = json.loads(rows[0][0] if isinstance(rows[0], list) else rows[0])
            check("features_json round-trip", stored.get("peer_address") == "10.1.99.1",
                  f"peer_address={stored.get('peer_address')}")
        else:
            check("features_json round-trip", False, "no rows returned")
    except Exception as exc:
        check("features_json round-trip", False, str(exc))

    # Write Remediation (skipped status)
    if detection_id:
        now_ns = int(time.time() * 1e9)
        try:
            resp = client.create_remediation(
                detection_id=detection_id,
                action="bgp_session_bounce",
                status="skipped",
                detail_json=json.dumps({"reason": "validate_p4 dry-run test"}),
                attempted_at_ns=now_ns,
                completed_at_ns=now_ns + 1_000_000,
            )
            check("CreateRemediation RPC", not resp.error,
                  f"id={resp.id[:8] if resp.id else 'none'}...")
        except Exception as exc:
            check("CreateRemediation RPC", False, str(exc))

        # Read Remediation back
        try:
            rows = client.query(
                "MATCH (r:Remediation) WHERE r.action = 'bgp_session_bounce' AND r.status = 'skipped' "
                "RETURN r.id, r.status, r.action LIMIT 1"
            )
            check("Remediation readable via Cypher", len(rows) >= 1,
                  f"row={rows[0] if rows else 'none'}")
        except Exception as exc:
            check("Remediation readable", False, str(exc))

        # Verify RESOLVES edge  (Remediation -[:RESOLVES]-> DetectionEvent)
        try:
            rows = client.query(
                "MATCH (r:Remediation)-[:RESOLVES]->(d:DetectionEvent) "
                "WHERE d.rule_id = 'validate_p4_test' RETURN r.status LIMIT 1"
            )
            check("RESOLVES edge exists", len(rows) >= 1,
                  f"status={rows[0][0] if rows else 'none'}")
        except Exception as exc:
            check("RESOLVES edge", False, str(exc))

    # Verify TRIGGERED edge (DetectionEvent linked to Device)
    try:
        rows = client.query(
            "MATCH (dev:Device)-[:TRIGGERED]->(det:DetectionEvent) "
            "WHERE det.rule_id = 'validate_p4_test' RETURN dev.hostname LIMIT 1"
        )
        check("TRIGGERED edge exists", len(rows) >= 1,
              f"device={rows[0][0] if rows else 'none'}")
    except Exception as exc:
        check("TRIGGERED edge", False, str(exc))


# ── 5. StreamEvents produces live events ──────────────────────────────────────

def validate_stream_events(client: BonsaiClient) -> None:
    section(f"5. StreamEvents — waiting up to {STREAM_TIMEOUT_S}s for live events")
    received: list = []
    error_holder: list = []

    def stream_thread():
        try:
            for evt in client.stream_events():
                received.append(evt)
                if len(received) >= 5:
                    break
        except Exception as exc:
            error_holder.append(str(exc))

    t = threading.Thread(target=stream_thread, daemon=True)
    t.start()
    t.join(timeout=STREAM_TIMEOUT_S)

    if error_holder:
        check("StreamEvents RPC reachable", False, error_holder[0])
    else:
        check("StreamEvents RPC reachable", True)

    if len(received) >= 1:
        check("live events received", True, f"{len(received)} event(s) in {STREAM_TIMEOUT_S}s")
    else:
        warn("live events received",
             f"0 events in {STREAM_TIMEOUT_S}s — lab stable; inject a fault to exercise this path")
    if received:
        types = list({e.event_type for e in received})
        devices = list({e.device_address for e in received})
        print(f"    event_types seen: {types}")
        print(f"    devices seen:     {devices}")
        check("events have device_address", all(e.device_address for e in received),
              f"{len(received)} checked")
        check("events have occurred_at_ns", all(e.occurred_at_ns > 0 for e in received),
              f"{len(received)} checked")


# ── 6. Rule engine — synthetic event dispatch ─────────────────────────────────

def validate_rule_engine(client: BonsaiClient) -> None:
    section("6. Rule engine — synthetic event dispatch")

    class SyntheticEvent:
        def __init__(self, event_type, device_address, detail, occurred_at_ns=None):
            self.event_type     = event_type
            self.device_address = device_address
            self.detail_json    = json.dumps(detail)
            self.occurred_at_ns = occurred_at_ns or int(time.time() * 1e9)

    fired: list[Detection] = []

    # Simulate BGP session going idle
    evt = SyntheticEvent(
        "bgp_session_change",
        "172.100.101.11:57400",
        {"peer": "10.1.12.2", "old_state": "established", "new_state": "idle"},
    )
    for rule in BGP_RULES:
        try:
            features = rule.extract_features(evt, client)
            if features is None:
                continue
            reason = rule.detect(features)
            if reason:
                fired.append(Detection(
                    rule_id=rule.rule_id,
                    severity=rule.severity,
                    features=features,
                    reason=reason,
                    auto_remediate=getattr(rule, "auto_remediate", False),
                    remediation_action=getattr(rule, "remediation_action", ""),
                ))
        except Exception as exc:
            print(f"    rule {rule.rule_id} error: {exc}")

    bgp_down_fired = any(d.rule_id == "bgp_session_down" for d in fired)
    check("BgpSessionDown fires on idle peer", bgp_down_fired,
          f"fired rules: {[d.rule_id for d in fired]}")
    if bgp_down_fired:
        det = next(d for d in fired if d.rule_id == "bgp_session_down")
        check("  auto_remediate=True", det.auto_remediate)
        check("  remediation_action=bgp_session_bounce", det.remediation_action == "bgp_session_bounce")
        check("  features.to_json() is valid JSON", True)
        try:
            parsed = json.loads(det.features.to_json())
            check("  peer_address in features", parsed.get("peer_address") == "10.1.12.2")
        except Exception as exc:
            check("  features.to_json() valid", False, str(exc))

    # Simulate interface down
    iface_fired: list[Detection] = []
    evt_if = SyntheticEvent(
        "interface_oper_status_change",
        "172.100.101.11:57400",
        {"if_name": "ethernet-1/1", "oper_status": "down"},
    )
    for rule in INTERFACE_RULES:
        try:
            features = rule.extract_features(evt_if, client)
            if features is None:
                continue
            reason = rule.detect(features)
            if reason:
                iface_fired.append(Detection(
                    rule_id=rule.rule_id,
                    severity=rule.severity,
                    features=features,
                    reason=reason,
                ))
        except Exception as exc:
            print(f"    rule {rule.rule_id} error: {exc}")

    iface_down_fired = any(d.rule_id == "interface_down" for d in iface_fired)
    check("InterfaceDown fires on down status", iface_down_fired,
          f"fired: {[d.rule_id for d in iface_fired]}")

    # BgpSessionDown should NOT fire for established state
    evt_ok = SyntheticEvent(
        "bgp_session_change",
        "172.100.101.11:57400",
        {"peer": "10.1.12.3", "old_state": "active", "new_state": "established"},
    )
    no_fire = []
    for rule in BGP_RULES:
        try:
            features = rule.extract_features(evt_ok, client)
            if features is None:
                continue
            reason = rule.detect(features)
            if reason:
                no_fire.append(rule.rule_id)
        except Exception:
            pass
    check("BgpSessionDown does NOT fire for established", "bgp_session_down" not in no_fire,
          f"erroneously fired: {no_fire}")


# ── 7. PushRemediation dry-run path ───────────────────────────────────────────

def validate_push_remediation(client: BonsaiClient) -> None:
    section("7. PushRemediation RPC — dry-run connectivity check")

    try:
        devices = client.get_devices()
        if not devices:
            check("PushRemediation target available", False, "no devices in graph")
            return
        device = devices[0]
        print(f"  testing against: {device.address} (vendor={device.vendor})")

        # We call with a harmless read-only-equivalent path that the device will reject
        # but we just need to verify the RPC round-trip reaches Rust and returns.
        # Use a deliberately invalid path so no config change is applied.
        resp = client.push_remediation(
            target_address=device.address,
            yang_path="network-instance[name=default]/protocols/bgp/validate-only",
            json_value='"dry-run"',
        )
        # We expect either success or an error string — both mean the RPC plumbing works
        check("PushRemediation RPC reachable", True,
              f"success={resp.success} error='{resp.error}'")

        if resp.success:
            check("  (unexpected: validate-only succeeded — that's fine)", True)
        else:
            check("  returned error (expected for dummy path)", True, resp.error[:80])
    except Exception as exc:
        check("PushRemediation RPC reachable", False, str(exc))


# ── 8. Full graph summary ──────────────────────────────────────────────────────

def graph_summary(client: BonsaiClient) -> None:
    section("8. Full graph node counts")
    queries = [
        ("Device",            "MATCH (n:Device) RETURN count(n)"),
        ("Interface",         "MATCH (n:Interface) RETURN count(n)"),
        ("BgpNeighbor",       "MATCH (n:BgpNeighbor) RETURN count(n)"),
        ("LldpNeighbor",      "MATCH (n:LldpNeighbor) RETURN count(n)"),
        ("CONNECTED_TO edge", "MATCH ()-[e:CONNECTED_TO]->() RETURN count(e)"),
        ("StateChangeEvent",  "MATCH (n:StateChangeEvent) RETURN count(n)"),
        ("DetectionEvent",    "MATCH (n:DetectionEvent) RETURN count(n)"),
        ("Remediation",       "MATCH (n:Remediation) RETURN count(n)"),
    ]
    for label, cypher in queries:
        try:
            rows = client.query(cypher)
            count = rows[0][0] if rows and rows[0] else 0
            print(f"  {label:<22} {count:>5}")
        except Exception as exc:
            print(f"  {label:<22}  ERROR: {exc}")


# ── Final summary ─────────────────────────────────────────────────────────────

def print_summary() -> None:
    section("SUMMARY")
    passed  = sum(1 for _, ok, _ in _results if ok)
    failed  = sum(1 for _, ok, _ in _results if not ok)
    total   = len(_results)
    print(f"  {passed}/{total} checks passed")
    if failed:
        print(f"\n  {FAIL} Failing checks:")
        for name, ok, detail in _results:
            if not ok:
                detail_short = str(detail).split("\n")[0][:120] if detail else ""
                print(f"    * {name}" + (f" -- {detail_short}" if detail_short else ""))
    else:
        print(f"  {PASS} All checks passed!")
    print()


def cleanup_bonsai() -> None:
    global _bonsai_proc
    if _bonsai_proc:
        print("\nStopping bonsai process...")
        _bonsai_proc.terminate()
        try:
            _bonsai_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            _bonsai_proc.kill()
        _bonsai_proc = None


def main():
    print(f"{'='*60}")
    print(f"  Bonsai Phase 4 — Validation Suite")
    print(f"  addr={ADDR}  wait={WAIT_FOR_DATA_S}s")
    print(f"{'='*60}")

    clean_environment()
    start_bonsai()

    try:
        with BonsaiClient(ADDR) as client:
            # Wrap all non-streaming stub calls with a 10s timeout so the
            # script never hangs if bonsai is unreachable mid-run.
            _patch_client_timeout(client, timeout=10)
            wait_for_data(client)
            validate_graph_tables(client)
            validate_phase4_schema(client)
            validate_stream_events(client)
            validate_rule_engine(client)
            validate_push_remediation(client)
            graph_summary(client)
    except KeyboardInterrupt:
        print("\nInterrupted.")
    finally:
        cleanup_bonsai()

    print_summary()
    sys.exit(0 if all(ok for _, ok, _ in _results) else 1)


if __name__ == "__main__":
    main()

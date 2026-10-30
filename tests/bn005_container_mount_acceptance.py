"""Explicit Docker gate: mount the reviewed binary and replace its container, retaining only its named volume.
Run: DOCKER_HOST=unix:///var/run/docker.sock python3 tests/bn005_container_mount_acceptance.py EVIDENCE_DIR
Uses only uniquely named temporary containers/volume. Traffic is a synthetic BMP fixture.
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = Path(sys.argv[1]).resolve()
OUTPUT.mkdir(parents=True, exist_ok=True)
RUN = "bn005-" + uuid.uuid4().hex[:12]
VOLUME = RUN + "-runtime"
NAMES = [RUN + "-first", RUN + "-replacement"]
TOKEN = "bn005-isolated-mount-test"
RESULT = {"status": "running", "fixture_provenance": "synthetic BMP; not a live NOS test", "containers": NAMES, "volume": VOLUME}


def docker(*args, check=True):
    proc = subprocess.run(["docker", *args], text=True, capture_output=True, timeout=30)
    if check and proc.returncode:
        raise RuntimeError(f"docker {args[0]} failed: {proc.stderr}")
    return proc.stdout.strip()


def wait_for(action, seconds=15):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            result = action()
            if result:
                return result
        except (OSError, urllib.error.URLError) as exc:
            last = str(exc)
        time.sleep(0.025)
    raise AssertionError(f"deadline waiting for fixture: {last}")


def api(http, path, token=TOKEN):
    request = urllib.request.Request(f"http://{http}/internal/routing/v1/{path}", headers={"x-bonsai-routing-token": token})
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.load(response)


def frame(kind, payload):
    return struct.pack("!BIB", 3, len(payload) + 6, kind) + payload


def frames(local_pref):
    peer = socket.inet_aton("192.0.2.2")
    header = bytes(10) + bytes(12) + peer + struct.pack("!I", 65002) + peer + struct.pack("!II", 1700000000, 0)
    bgp_open = bytes([255]) * 16 + struct.pack("!HB BHH", 29, 1, 4, 65002, 90) + peer + bytes(1)
    initiation = frame(4, struct.pack("!HH", 2, 5) + b"bn005")
    up = frame(3, header + bytes(12) + socket.inet_aton("192.0.2.254") + struct.pack("!HH", 179, 50000) + bgp_open * 2)
    attrs = bytes([0x40, 1, 1, 0, 0x40, 2, 6, 2, 1]) + struct.pack("!I", 65002)
    attrs += bytes([0x40, 3, 4]) + peer + bytes([0x40, 5, 4]) + struct.pack("!I", local_pref)
    body = bytes(2) + struct.pack("!H", len(attrs)) + attrs + bytes([24, 10, 77, 10])
    route = frame(0, header + bytes([255]) * 16 + struct.pack("!HB", 19 + len(body), 2) + body)
    return initiation + up + route


def launch(name, image):
    docker("run", "-d", "--name", name, "--network", "bridge", "-p", "127.0.0.1::8081", "-p", "127.0.0.1::11019",
           "--mount", f"type=volume,source={VOLUME},target=/app/runtime",
           "--mount", f"type=bind,source={ROOT / 'target/debug/bonsai-routing'},target=/tmp/bn005-routing,readonly",
           "-e", f"BONSAI_ROUTING_SERVICE_TOKEN={TOKEN}", "-e", "BONSAI_ROUTING_HTTP_ADDR=0.0.0.0:8081",
           "-e", "BONSAI_ROUTING_BMP_ADDR=0.0.0.0:11019", "-e", "BONSAI_ROUTING_BMP_JOURNAL=/app/runtime/bmp.bin",
           "-e", "BONSAI_ROUTING_EVENT_JOURNAL=/app/runtime/events.jsonl", "--entrypoint", "/tmp/bn005-routing", image)
    http = docker("port", name, "8081/tcp").splitlines()[0]
    bmp = docker("port", name, "11019/tcp").splitlines()[0]
    wait_for(lambda: api(http, "health"))
    mounts = json.loads(docker("inspect", name))[0]["Mounts"]
    assert any(m["Type"] == "volume" and m["Name"] == VOLUME and m["Destination"] == "/app/runtime" for m in mounts)
    return http, bmp


def ingest(bmp, lp):
    host, port = bmp.rsplit(":", 1)
    stream = socket.create_connection((host, int(port)), timeout=3)
    stream.sendall(frames(lp))
    return stream


def history(http, lp):
    result = api(http, "prefix-history?prefix=10.77.10.0/24")
    return result if result.get("events") and result["events"][0]["after_attributes"]["local_pref"] == lp else None


try:
    image = json.loads(docker("image", "inspect", "bonsai:latest"))[0]["Id"]
    RESULT["runtime_image_id"] = image
    RESULT["binary_sha256"] = hashlib.sha256((ROOT / "target/debug/bonsai-routing").read_bytes()).hexdigest()
    docker("volume", "create", VOLUME)
    http, bmp = launch(NAMES[0], image)
    try:
        api(http, "health", "incorrect")
        raise AssertionError("incorrect token succeeded")
    except urllib.error.HTTPError as exc:
        assert exc.code == 401
    connection = ingest(bmp, 100)
    before = wait_for(lambda: history(http, 100))
    original_routes = api(http, "routes?prefix=10.77.10.0/24")["routes"]
    assert len(original_routes) == 1
    assert original_routes[0]["local_pref"] == 100
    docker("exec", NAMES[0], "/bin/sh", "-c", "test -s /app/runtime/bmp.bin && test -s /app/runtime/events.jsonl")
    (OUTPUT / (NAMES[0] + ".log")).write_text(docker("logs", NAMES[0]))
    docker("kill", "--signal", "KILL", NAMES[0])
    connection.close()
    docker("rm", NAMES[0])
    http2, bmp2 = launch(NAMES[1], image)
    assert api(http2, "prefix-history?prefix=10.77.10.0/24")["events"] == before["events"]
    assert api(http2, "routes?prefix=10.77.10.0/24")["routes"] == original_routes
    connection2 = ingest(bmp2, 250)
    after = wait_for(lambda: history(http2, 250))
    assert after["durable_sequence"] > before["durable_sequence"]
    updated = api(http2, "routes?prefix=10.77.10.0/24")["routes"]
    assert updated[0]["source"]["session_epoch"] > original_routes[0]["source"]["session_epoch"]
    connection2.close()
    RESULT.update(status="passed", before=before, after=after, named_volume_reused_after_container_removal=True)
except BaseException as exc:
    RESULT.update(status="failed", error=str(exc))
    raise
finally:
    for name in NAMES:
        logs = docker("logs", name, check=False)
        if logs or not (OUTPUT / (name + ".log")).exists():
            (OUTPUT / (name + ".log")).write_text(logs)
        docker("rm", "-f", name, check=False)
    docker("volume", "rm", VOLUME, check=False)
    (OUTPUT / "result.json").write_text(json.dumps(RESULT, indent=2) + "\n")
print("BN-005 named-volume container replacement: passed")

#!/usr/bin/env python3
"""Dedicated BN-006 cEOS packet forwarding and withdrawal acceptance."""
import argparse
import json
from pathlib import Path
import subprocess
import time
import urllib.parse
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("--lab-root", required=True)
parser.add_argument("--source-url", required=True)
parser.add_argument("--speaker", required=True)
parser.add_argument("--evidence-name", default="fib-verified")
parser.add_argument("--instance", choices=["default", "blue"], default="default")
args = parser.parse_args()
root = Path(args.lab_root).resolve()
assert root.parent == Path("/tmp") and root.name.startswith("bonsai-bn006-live-")
assert Path(args.evidence_name).name == args.evidence_name
out = root / args.evidence_name
out.mkdir(exist_ok=False)
eos = f"clab-{root.name}-eos"
source = f"clab-{root.name}-source"
for node in (eos, source):
    label = subprocess.check_output(["docker", "inspect", "--format", '{{ index .Config.Labels "containerlab" }}', node], text=True).strip()
    assert label == root.name, f"refusing non-owned container {node}"
subprocess.run(["docker", "cp", "tests/lab/bn006_fib_probe.py", f"{source}:/bn006_fib_probe.py"], check=True)
prefix, rd, target, eos_vrf = (("10.77.1.0/24", "0:0", bytes([10, 77, 1, 123]), "default")
    if args.instance == "default" else ("10.77.2.0/24", "65001:100", bytes([10, 77, 2, 123]), "BLUE"))
api = "http://127.0.0.1:19029/internal/routing/v1/routes?" + urllib.parse.urlencode({
    "speaker": args.speaker, "prefix": prefix, "vrf_rd": rd})
other_prefix, other_rd = (("10.77.2.0/24", "65001:100") if args.instance == "default"
                          else ("10.77.1.0/24", "0:0"))
other_api = "http://127.0.0.1:19029/internal/routing/v1/routes?" + urllib.parse.urlencode({
    "speaker": args.speaker, "prefix": other_prefix, "vrf_rd": other_rd})


def request(url, body=None):
    req = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(),
        headers={"x-bonsai-routing-token": "bn006-lab-internal", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as response:
        return json.load(response)


def wait_routes(present):
    for _ in range(60):
        data = request(api)
        routes = data["routes"]
        matched = len(routes) == 2 and {r["view"] for r in routes} == {"LocRib", "AdjRibInPostPolicy"}
        if matched if present else not routes:
            return data
        time.sleep(1)
    raise AssertionError("BMP route transition timed out")


def save(name, value):
    (out / name).write_text(json.dumps(value, indent=2) + "\n")


def probe(expected):
    raw = subprocess.check_output(["docker", "exec", source, "python3", "/bn006_fib_probe.py", "--expect", expected, "--instance", args.instance], text=True, timeout=20)
    result = json.loads(raw)
    sent = bytes.fromhex(result["sent_frame_hex"])
    assert sent[22] == 64 and sent[30:34] == target
    if expected == "forwarded":
        received = bytes.fromhex(result["received_frame_hex"])
        assert result["forwarded"] and received[22] == 63 and received[30:34] == sent[30:34]
        assert received[42:] == sent[42:] and received[:6] == sent[6:12] and received[6:12] == sent[:6]
    else:
        assert not result["forwarded"] and result["received_frame_hex"] is None
    return result


def device_route():
    raw = subprocess.check_output(["docker", "exec", eos, "Cli", "-p", "15", "-c", f"show ip route vrf {eos_vrf} {prefix} | json"], text=True, timeout=10)
    return json.loads(raw)


save("baseline-bmp.json", wait_routes(True))
save("baseline-eos-route.json", device_route())
save("baseline-probe.json", probe("forwarded"))
try:
    save("withdraw-source.json", request(args.source_url, {"session": args.instance, "present": False}))
    save("withdraw-bmp.json", wait_routes(False))
    other = request(other_api)
    assert len(other["routes"]) == 2 and {r["view"] for r in other["routes"]} == {"LocRib", "AdjRibInPostPolicy"}
    save("withdraw-other-vrf.json", other)
    route = device_route()
    assert not route["vrfs"][eos_vrf]["routes"]
    save("withdraw-eos-route.json", route)
    save("withdraw-probe.json", probe("absent"))
finally:
    save("restore-source.json", request(args.source_url, {"session": args.instance, "present": True, "med": 0}))
save("restore-bmp.json", wait_routes(True))
save("restore-eos-route.json", device_route())
save("restore-probe.json", probe("forwarded"))
print("PASS: cEOS packet forwarding, loss on withdrawal, and restoration; raw Ethernet frames retained")

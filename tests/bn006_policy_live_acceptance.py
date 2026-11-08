#!/usr/bin/env python3
"""Compare a stopped-router startup policy candidate with recorded baseline."""
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
parser.add_argument("--min-epoch", type=int, required=True)
args = parser.parse_args()
root = Path(args.lab_root).resolve()
assert root.parent == Path("/tmp") and root.name.startswith("bonsai-bn006-live-")
out = root / "policy-final"
out.mkdir(exist_ok=False)
eos = f"clab-{root.name}-eos"
label = subprocess.check_output(["docker", "inspect", "--format", '{{ index .Config.Labels "containerlab" }}', eos], text=True).strip()
assert label == root.name
api = "http://127.0.0.1:19029/internal/routing/v1/routes?"


def request(url, body=None):
    r = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "x-bonsai-routing-token": "bn006-lab-internal"})
    with urllib.request.urlopen(r, timeout=5) as response:
        return json.load(response)


def save(name, value):
    (out / name).write_text(json.dumps(value, indent=2) + "\n")


def routes(prefix, rd):
    query = urllib.parse.urlencode({"speaker": args.speaker, "prefix": prefix, "vrf_rd": rd})
    return request(api + query)["routes"]


state = request(args.source_url)
assert len(state) == 3 and all(session["established"] for session in state.values())
save("candidate-source.json", request(args.source_url, {"session": "default", "present": True, "med": 150}))
for _ in range(120):
    glob = routes("10.77.1.0/24", "0:0")
    blue = routes("10.77.2.0/24", "65001:100")
    views = {"LocRib", "AdjRibInPostPolicy"}
    if (len(glob) == len(blue) == 2 and {r["view"] for r in glob} == views
        and {r["view"] for r in blue} == views
        and all(r["med"] == 150 and r["local_pref"] == 300 and r["source"]["session_epoch"] >= args.min_epoch for r in glob)
        and all(r["med"] == 0 and r["local_pref"] == 100 and r["source"]["session_epoch"] >= args.min_epoch for r in blue)):
        break
    time.sleep(1)
else:
    raise AssertionError("candidate policy did not produce expected BMP/RIB state")
save("candidate-bmp.json", {"global": glob, "blue": blue})
raw = subprocess.check_output(["docker", "exec", eos, "Cli", "-p", "15", "-c", "show bgp ipv4 unicast 10.77.1.0/24 | json"], text=True, timeout=10)
(out / "candidate-med150.json").write_text(raw)
route = json.loads(raw)["vrfs"]["default"]["bgpRouteEntries"]["10.77.1.0/24"]["bgpRoutePaths"]
assert len(route) == 1 and route[0]["localPreference"] == 300 and route[0]["med"] == 150 and route[0]["routeType"]["active"]
baseline = json.loads((root / "second-run/eos-modified-route.json").read_text())["vrfs"]["default"]["bgpRouteEntries"]["10.77.1.0/24"]["bgpRoutePaths"]
assert len(baseline) == 1 and baseline[0]["localPreference"] == 250 and baseline[0]["med"] == 150
policy = subprocess.check_output(["docker", "exec", eos, "Cli", "-p", "15", "-c", "show running-config section route-map"], text=True, timeout=10)
assert "set local-preference 300" in policy and "set local-preference 250" not in policy
(out / "candidate-policy.txt").write_text(policy)
print("PASS: offline startup policy changed MED 150 local preference from 250 to 300; BLUE RD stayed isolated")

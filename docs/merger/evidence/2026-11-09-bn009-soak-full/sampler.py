#!/usr/bin/env python3
"""BN-009/BN-019 full-length soak sampler.

Appends one CSV row per SAMPLE_SECS to soak-metrics.csv until killed.
Captures: routing health, sidecar /health, /api/sidecars registry, and
docker stats memory for the two packaged containers. Auth: admin JWT via
/api/auth/login using .env creds (refreshed on 401/expiry).
"""
import json, os, subprocess, sys, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent / "soak-metrics.csv"
API = "http://localhost:3000"
SAMPLE_SECS = int(os.environ.get("SOAK_SAMPLE_SECS", "60"))
CONTAINERS = ["bonsai-bonsai-standalone-1", "bonsai-bonsai-routing-1"]

def env_val(k):
    for line in (ROOT / ".env").read_text().splitlines():
        if line.startswith(k + "="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    return ""

_token = [None]
def login():
    body = json.dumps({"username": env_val("BONSAI_ADMIN_USER"),
                       "password": env_val("BONSAI_ADMIN_PASS")}).encode()
    req = urllib.request.Request(API + "/api/auth/login", data=body,
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        d = json.loads(r.read())
    _token[0] = d.get("token") or d.get("access_token") or d.get("jwt")

def get(path, auth=True):
    h = {"Authorization": f"Bearer {_token[0]}"} if auth and _token[0] else {}
    req = urllib.request.Request(API + path, headers=h)
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())

def get_raw(url):
    with urllib.request.urlopen(url, timeout=5) as r:
        return json.loads(r.read())

def docker_mem():
    try:
        out = subprocess.check_output(
            ["docker", "stats", "--no-stream", "--format",
             "{{.Name}} {{.MemUsage}}"] + CONTAINERS, text=True, timeout=15)
        return " ".join(out.split())
    except Exception as e:
        return f"docker_stats_err={e}"

def sidecar_health():
    try:
        return get_raw("http://localhost:9294/health")
    except Exception:
        return {}

def registry():
    try:
        d = get("/api/sidecars")
        items = d.get("sidecars", d if isinstance(d, list) else [])
        return ";".join(f"{s.get('name','?')}={s.get('status','?')}" for s in items)
    except Exception as e:
        return f"registry_err"

def main():
    login()
    hdr = ("ts,routing_peers_up,routing_routes,routing_gaps,routing_seq,"
           "routing_complete,routing_speakers,sc_queue,sc_drops,sc_priority,"
           "sc_jobs,sc_snap_buf,sc_snap_stale,sc_conn_core,sc_conn_local,"
           "sc_uptime,sidecars,mem")
    if not OUT.exists():
        OUT.write_text(hdr + "\n")
    last_login = time.time()
    while True:
        ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        try:
            rh = get("/api/routing/v1/health")
        except Exception:
            if time.time() - last_login > 3000:
                try: login(); last_login = time.time()
                except Exception: pass
            rh = {}
        sc = sidecar_health()
        row = [
            ts,
            rh.get("peers_up", ""), rh.get("total_routes", ""),
            rh.get("transport_gaps", ""), rh.get("durable_sequence", ""),
            rh.get("completeness_state", ""), rh.get("speakers_connected", ""),
            sc.get("queue_depth", ""), sc.get("queue_drops_today", ""),
            sc.get("priority_only_mode", ""), sc.get("job_engine_running", ""),
            sc.get("snapshot_buffer_size", ""), sc.get("snapshot_buffer_stale", ""),
            sc.get("connected_to_core", ""), sc.get("connected_to_local", ""),
            sc.get("uptime_secs", ""),
            registry(), docker_mem(),
        ]
        with OUT.open("a") as f:
            f.write(",".join(str(c) for c in row) + "\n")
        print(f"[soak] {ts} written", flush=True)
        time.sleep(SAMPLE_SECS)

if __name__ == "__main__":
    main()

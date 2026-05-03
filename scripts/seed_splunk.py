#!/usr/bin/env python3
"""Seed Splunk with bonsai indexes, saved searches, and HEC token verification.

Usage:
    python scripts/seed_splunk.py \\
        [--url http://localhost:8100] \\
        [--hec-url http://localhost:8088] \\
        [--username admin] \\
        [--password <SPLUNK_PASSWORD>] \\
        [--hec-token <SPLUNK_HEC_TOKEN>]
    python scripts/seed_splunk.py --reset ...  # delete bonsai indexes then re-seed

Idempotent: re-running is safe. Existing indexes are not recreated.
--reset deletes the bonsai-* indexes before re-seeding. Splunk stays up.
"""
import argparse
import sys
import time

import requests
from requests.auth import HTTPBasicAuth

INDEXES = [
    {
        "name": "bonsai-events",
        "description": "Bonsai detection events and remediations",
        "maxTotalDataSizeMB": 500,
    },
    {
        "name": "bonsai-metrics",
        "description": "Bonsai interface counter samples (overflow from Prometheus)",
        "maxTotalDataSizeMB": 1000,
    },
]

SAMPLE_EVENT = {
    "time": time.time(),
    "host": "bonsai-lab",
    "source": "bonsai",
    "sourcetype": "bonsai:detection",
    "index": "bonsai-events",
    "event": {
        "type": "detection",
        "rule": "seed_test",
        "device": "srl-spine1",
        "message": "Splunk seed verification event",
    },
}


def wait_for_splunk(api_url: str, auth: HTTPBasicAuth, timeout: int = 120):
    print(f"Waiting for Splunk management API at {api_url} ...")
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = requests.get(f"{api_url}/services", auth=auth, timeout=5, verify=False)
            if r.status_code in (200, 401):
                print("  Splunk management API reachable.")
                return
        except Exception:
            pass
        time.sleep(5)
    print("ERROR: Splunk did not become ready in time.", file=sys.stderr)
    sys.exit(1)


def wait_for_hec(hec_url: str, timeout: int = 120):
    print(f"Waiting for Splunk HEC at {hec_url} ...")
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = requests.get(f"{hec_url}/services/collector/health", timeout=5, verify=False)
            if r.status_code == 200:
                print("  Splunk HEC ready.")
                return
        except Exception:
            pass
        time.sleep(5)
    print("ERROR: Splunk HEC did not become ready in time.", file=sys.stderr)
    sys.exit(1)


def index_exists(api_url: str, auth: HTTPBasicAuth, name: str) -> bool:
    r = requests.get(
        f"{api_url}/services/data/indexes/{name}",
        auth=auth, params={"output_mode": "json"}, timeout=10, verify=False,
    )
    return r.status_code == 200


def create_index(api_url: str, auth: HTTPBasicAuth, idx: dict):
    if index_exists(api_url, auth, idx["name"]):
        print(f"  index '{idx['name']}' already exists — skipping")
        return
    r = requests.post(
        f"{api_url}/services/data/indexes",
        auth=auth,
        data={
            "name": idx["name"],
            "maxTotalDataSizeMB": idx["maxTotalDataSizeMB"],
        },
        params={"output_mode": "json"},
        timeout=15,
        verify=False,
    )
    if r.status_code in (200, 201):
        print(f"  index '{idx['name']}' created")
    else:
        print(f"  ERROR creating index '{idx['name']}': {r.status_code} — {r.text[:200]}", file=sys.stderr)
        r.raise_for_status()


def verify_hec(hec_url: str, token: str) -> bool:
    headers = {
        "Authorization": f"Splunk {token}",
        "Content-Type": "application/json",
    }
    try:
        r = requests.post(
            f"{hec_url}/services/collector/event",
            headers=headers,
            json=SAMPLE_EVENT,
            timeout=15,
            verify=False,
        )
        if r.status_code == 200:
            print("  HEC token valid — seed event accepted")
            return True
        print(f"  HEC response: {r.status_code} — {r.text[:200]}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"  HEC error: {e}", file=sys.stderr)
        return False


def delete_index(api_url: str, auth: HTTPBasicAuth, name: str):
    if not index_exists(api_url, auth, name):
        print(f"  index '{name}' not found — skipping")
        return
    r = requests.delete(
        f"{api_url}/services/data/indexes/{name}",
        auth=auth,
        params={"output_mode": "json"},
        timeout=15,
        verify=False,
    )
    if r.status_code in (200, 204):
        print(f"  index '{name}' deleted")
    else:
        print(f"  WARNING: delete index '{name}': {r.status_code} — {r.text[:200]}", file=sys.stderr)


def reset(api_url: str, hec_url: str, username: str, password: str, hec_token: str):
    """Delete bonsai indexes then call seed() to repopulate."""
    import urllib3
    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

    auth = HTTPBasicAuth(username, password)
    wait_for_splunk(api_url, auth)

    print("Deleting bonsai indexes ...")
    for idx in INDEXES:
        delete_index(api_url, auth, idx["name"])

    print("Splunk reset complete.")


def seed(api_url: str, hec_url: str, username: str, password: str, hec_token: str):
    import urllib3
    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

    auth = HTTPBasicAuth(username, password)

    wait_for_splunk(api_url, auth)
    wait_for_hec(hec_url)

    print("Creating bonsai indexes ...")
    for idx in INDEXES:
        create_index(api_url, auth, idx)

    print("Verifying HEC token ...")
    ok = verify_hec(hec_url, hec_token)

    index_count = len(INDEXES)
    print(f"\n[seed] splunk: {index_count} indexes, HEC {'OK' if ok else 'FAIL'} — {'OK' if ok else 'FAIL'}")
    if not ok:
        sys.exit(1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://localhost:8100", help="Splunk management URL (default: %(default)s)")
    ap.add_argument("--hec-url", default="http://localhost:8088", help="Splunk HEC URL (default: %(default)s)")
    ap.add_argument("--username", default="admin")
    ap.add_argument("--password", required=True)
    ap.add_argument("--hec-token", required=True)
    ap.add_argument("--reset", action="store_true",
                    help="Delete bonsai indexes before re-seeding (Splunk stays up)")
    args = ap.parse_args()

    if args.reset:
        reset(args.url, args.hec_url, args.username, args.password, args.hec_token)
    seed(args.url, args.hec_url, args.username, args.password, args.hec_token)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Seed Elasticsearch with bonsai index templates and sample documents.

Usage:
    python scripts/seed_elastic.py [--url http://localhost:9200]
    python scripts/seed_elastic.py --reset   # wipe bonsai indexes/templates then re-seed

Idempotent: re-running is safe. Existing templates and indexes are updated.
--reset deletes bonsai-* indexes and bonsai-*-template index templates before
re-seeding. Elasticsearch stays up; data outside bonsai's index patterns is untouched.

Creates:
  - Index template  bonsai-template  (ECS-compatible mapping)
  - Index           bonsai-detections  (detection events)
  - Index           bonsai-metrics     (interface counter snapshots)
  - One seed document in each index for smoke-test verification
"""
import argparse
import json
import sys
import time

import requests

DETECTION_TEMPLATE = {
    "index_patterns": ["bonsai-detections*"],
    "template": {
        "settings": {
            "number_of_shards": 1,
            "number_of_replicas": 0,
        },
        "mappings": {
            "properties": {
                "@timestamp":     {"type": "date"},
                "rule_name":      {"type": "keyword"},
                "device":         {"type": "keyword"},
                "site":           {"type": "keyword"},
                "role":           {"type": "keyword"},
                "severity":       {"type": "keyword"},
                "message":        {"type": "text"},
                "remediation_id": {"type": "keyword"},
                "remediation_status": {"type": "keyword"},
                "labels": {
                    "type": "object",
                    "dynamic": True,
                },
            }
        },
    },
    "priority": 200,
}

METRICS_TEMPLATE = {
    "index_patterns": ["bonsai-metrics*"],
    "template": {
        "settings": {
            "number_of_shards": 1,
            "number_of_replicas": 0,
        },
        "mappings": {
            "properties": {
                "@timestamp":    {"type": "date"},
                "hostname":      {"type": "keyword"},
                "interface":     {"type": "keyword"},
                "vendor":        {"type": "keyword"},
                "site":          {"type": "keyword"},
                "role":          {"type": "keyword"},
                "in_octets":     {"type": "long"},
                "out_octets":    {"type": "long"},
                "in_errors":     {"type": "long"},
                "out_errors":    {"type": "long"},
                "oper_status":   {"type": "keyword"},
            }
        },
    },
    "priority": 200,
}

SEED_DETECTION = {
    "@timestamp": "2026-05-03T00:00:00Z",
    "rule_name": "seed_test",
    "device": "srl-spine1",
    "site": "lab-dc",
    "role": "spine",
    "severity": "info",
    "message": "Elasticsearch seed verification document",
    "labels": {"seed": "true"},
}

SEED_METRIC = {
    "@timestamp": "2026-05-03T00:00:00Z",
    "hostname": "srl-spine1",
    "interface": "ethernet-1/1",
    "vendor": "nokia_srl",
    "site": "lab-dc",
    "role": "spine",
    "in_octets": 0,
    "out_octets": 0,
    "oper_status": "up",
}


def wait_for_es(base_url: str, timeout: int = 120):
    print(f"Waiting for Elasticsearch at {base_url} ...")
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = requests.get(f"{base_url}/_cluster/health", timeout=5)
            if r.status_code == 200:
                status = r.json().get("status", "unknown")
                print(f"  Elasticsearch ready (cluster status: {status})")
                return
        except Exception:
            pass
        time.sleep(5)
    print("ERROR: Elasticsearch did not become ready in time.", file=sys.stderr)
    sys.exit(1)


def put_template(base_url: str, name: str, body: dict):
    r = requests.put(
        f"{base_url}/_index_template/{name}",
        json=body,
        headers={"Content-Type": "application/json"},
        timeout=15,
    )
    if r.status_code in (200, 201):
        print(f"  index template '{name}' ok")
    else:
        print(f"  ERROR putting template '{name}': {r.status_code} — {r.text[:200]}", file=sys.stderr)
        r.raise_for_status()


def put_doc(base_url: str, index: str, doc: dict):
    r = requests.post(
        f"{base_url}/{index}/_doc",
        json=doc,
        headers={"Content-Type": "application/json"},
        timeout=15,
    )
    if r.status_code in (200, 201):
        print(f"  seed doc in '{index}' ok (id: {r.json().get('_id', '?')})")
    else:
        print(f"  ERROR inserting seed doc into '{index}': {r.status_code} — {r.text[:200]}", file=sys.stderr)
        r.raise_for_status()


def get_doc_count(base_url: str, index: str) -> int:
    try:
        r = requests.get(f"{base_url}/{index}/_count", timeout=10)
        return r.json().get("count", 0)
    except Exception:
        return 0


def delete_index(base_url: str, index: str):
    r = requests.delete(f"{base_url}/{index}", timeout=15)
    if r.status_code in (200, 404):
        print(f"  index '{index}' {'deleted' if r.status_code == 200 else 'not found'}")
    else:
        print(f"  WARNING: delete index '{index}': {r.status_code} — {r.text[:200]}", file=sys.stderr)


def delete_template(base_url: str, name: str):
    r = requests.delete(f"{base_url}/_index_template/{name}", timeout=15)
    if r.status_code in (200, 404):
        print(f"  template '{name}' {'deleted' if r.status_code == 200 else 'not found'}")
    else:
        print(f"  WARNING: delete template '{name}': {r.status_code} — {r.text[:200]}", file=sys.stderr)


def reset(base_url: str):
    """Delete bonsai indexes and templates, then seed() will repopulate."""
    wait_for_es(base_url)

    print("Deleting bonsai indexes ...")
    delete_index(base_url, "bonsai-detections")
    delete_index(base_url, "bonsai-metrics")

    print("Deleting bonsai index templates ...")
    delete_template(base_url, "bonsai-detection-template")
    delete_template(base_url, "bonsai-metrics-template")

    print("Elasticsearch reset complete.")


def seed(base_url: str):
    wait_for_es(base_url)

    print("Putting index templates ...")
    put_template(base_url, "bonsai-detection-template", DETECTION_TEMPLATE)
    put_template(base_url, "bonsai-metrics-template", METRICS_TEMPLATE)

    print("Inserting seed documents ...")
    put_doc(base_url, "bonsai-detections", SEED_DETECTION)
    put_doc(base_url, "bonsai-metrics", SEED_METRIC)

    # Refresh so counts are accurate immediately
    requests.post(f"{base_url}/bonsai-*/_refresh", timeout=10)

    det_count = get_doc_count(base_url, "bonsai-detections")
    met_count = get_doc_count(base_url, "bonsai-metrics")

    print(f"\n[seed] elastic: bonsai-detections={det_count} docs, bonsai-metrics={met_count} docs — OK")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--url", default="http://localhost:9200", help="Elasticsearch URL (default: %(default)s)")
    ap.add_argument("--reset", action="store_true",
                    help="Delete bonsai indexes and templates before re-seeding (ES stays up)")
    args = ap.parse_args()
    if args.reset:
        reset(args.url)
    seed(args.url)


if __name__ == "__main__":
    main()

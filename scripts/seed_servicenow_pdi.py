#!/usr/bin/env python3
"""Seed a fresh ServiceNow PDI with the bonsai lab topology.

Reads lab/seed/topology.yaml (single source of truth) and populates:
  - cmdb_ci_netgear CIs for each lab device
  - cmdb_ci_business_service for representative applications
  - cmdb_rel_ci relationships (Runs::Provided by) linking devices to services
  - Rack / site CIs from site definitions

Credentials: reads PDI URL and admin credentials from environment variables
SNOW_INSTANCE_URL, SNOW_USERNAME, SNOW_PASSWORD.  Vault-backed credential
resolution (via the bonsai gRPC credential API) is future work tracked in the
v10 backlog as T0-2 / Q-6; use --use-vault when that is implemented.

Usage:
    export SNOW_INSTANCE_URL=https://devXXXXXX.service-now.com
    export SNOW_USERNAME=admin
    export SNOW_PASSWORD=your_pdi_password
    python scripts/seed_servicenow_pdi.py [--topology lab/seed/topology.yaml] [--dry-run]
"""

import argparse
import base64
import json
import os
import sys
from pathlib import Path

import requests
import yaml


def load_topology(path: str) -> dict:
    with open(path) as f:
        return yaml.safe_load(f)


class SnowClient:
    def __init__(self, instance_url: str, username: str, password: str, dry_run: bool = False):
        self.base = instance_url.rstrip("/")
        self.auth = (username, password)
        self.dry_run = dry_run
        self.session = requests.Session()
        self.session.auth = self.auth
        self.session.headers.update({"Content-Type": "application/json", "Accept": "application/json"})

    def get(self, table: str, query: str = "", fields: str = "", limit: int = 500) -> list[dict]:
        params: dict = {"sysparm_display_value": "all", "sysparm_limit": str(limit)}
        if query:
            params["sysparm_query"] = query
        if fields:
            params["sysparm_fields"] = fields
        url = f"{self.base}/api/now/table/{table}"
        r = self.session.get(url, params=params)
        r.raise_for_status()
        return r.json().get("result", [])

    def _lookup_one(self, table: str, match_field: str, match_value: str) -> dict | None:
        """Return the first record matching match_field=match_value, or None.

        Uses limit=1 so the query is fast and can't miss records past a 500-row page (Q-8).
        """
        results = self.get(table, f"{match_field}={match_value}", "sys_id,name", limit=1)
        return results[0] if results else None

    def upsert(self, table: str, match_field: str, match_value: str, payload: dict) -> dict:
        """Create or update a record matching match_field=match_value.

        After every write, a verification GET confirms the record is readable (Q-7).
        """
        existing = self._lookup_one(table, match_field, match_value)
        if existing:
            sys_id = existing["sys_id"]
            if self.dry_run:
                print(f"  [dry-run] PATCH {table}/{sys_id} {json.dumps(payload)[:80]}")
                return existing
            url = f"{self.base}/api/now/table/{table}/{sys_id}"
            r = self.session.patch(url, json=payload)
            r.raise_for_status()
            result = r.json().get("result", {})
        else:
            if self.dry_run:
                print(f"  [dry-run] POST  {table} {json.dumps(payload)[:80]}")
                return {"sys_id": f"dry-run-{match_value}", "name": match_value}
            url = f"{self.base}/api/now/table/{table}"
            r = self.session.post(url, json=payload)
            r.raise_for_status()
            result = r.json().get("result", {})

        # Verify the record is readable after write (Q-7)
        verified = self._lookup_one(table, match_field, match_value)
        if not verified:
            print(
                f"  WARNING: {table} record '{match_value}' not found after upsert"
                f" — check PDI configuration or field mapping"
            )

        return result

    def create_rel(self, parent_sys_id: str, child_sys_id: str, rel_type_name: str) -> None:
        """Create a cmdb_rel_ci relationship if it doesn't exist."""
        existing = self.get(
            "cmdb_rel_ci",
            f"parent={parent_sys_id}^child={child_sys_id}^type.name={rel_type_name}",
            "sys_id"
        )
        if existing:
            print(f"  rel already exists: {parent_sys_id} --[{rel_type_name}]--> {child_sys_id}")
            return
        # Resolve the relationship type sys_id
        types = self.get("cmdb_rel_type", f"name={rel_type_name}", "sys_id,name")
        if not types:
            print(f"  WARNING: relationship type '{rel_type_name}' not found — skipping")
            return
        rel_type_sys_id = types[0]["sys_id"]
        payload = {
            "parent": parent_sys_id,
            "child": child_sys_id,
            "type": rel_type_sys_id,
        }
        if self.dry_run:
            print(f"  [dry-run] POST cmdb_rel_ci {parent_sys_id} --[{rel_type_name}]--> {child_sys_id}")
            return
        url = f"{self.base}/api/now/table/cmdb_rel_ci"
        r = self.session.post(url, json=payload)
        r.raise_for_status()


def seed(client: SnowClient, topology: dict) -> None:
    lab = topology.get("lab", {})
    devices = topology.get("devices", [])
    sites = topology.get("sites", [])
    services = topology.get("services", [
        # Fallback sample apps if topology doesn't define them
        {"name": "payment-frontend",  "description": "Customer-facing payment service", "criticality": "1"},
        {"name": "internal-tools",    "description": "Internal engineering tooling",    "criticality": "3"},
        {"name": "monitoring-stack",  "description": "Metrics and alerting pipeline",   "criticality": "2"},
    ])

    print("== Sites ==")
    site_sys_ids: dict[str, str] = {}
    for site in sites:
        print(f"  site: {site['name']}")
        result = client.upsert("cmdb_ci_rack", "name", site["name"], {
            "name": site["name"],
            "short_description": site.get("description", f"bonsai lab site: {site['name']}"),
            "install_status": "1",
        })
        site_sys_ids[site["id"]] = result.get("sys_id", "")

    print("\n== Device CIs (cmdb_ci_netgear) ==")
    device_sys_ids: dict[str, str] = {}
    for dev in devices:
        name = dev["name"]
        print(f"  device: {name}")
        payload = {
            "name": name,
            "ip_address": dev.get("address", ""),
            "short_description": f"{dev.get('vendor', '')} {dev.get('role', '')} — bonsai lab",
            "model_id": dev.get("netbox_model", ""),
            "serial_number": dev.get("netbox_serial", ""),
            "install_status": "1",
            "u_bonsai_role": dev.get("role", ""),
            "u_bonsai_vendor": dev.get("vendor", ""),
            "assignment_group": dev.get("snow_owner_group", "Network-Operations"),
        }
        result = client.upsert("cmdb_ci_netgear", "name", name, payload)
        device_sys_ids[name] = result.get("sys_id", "")

    print("\n== Business Services ==")
    service_sys_ids: dict[str, str] = {}
    for svc in services:
        print(f"  service: {svc['name']}")
        payload = {
            "name": svc["name"],
            "short_description": svc.get("description", ""),
            "operational_status": "1",
        }
        result = client.upsert("cmdb_ci_business_service", "name", svc["name"], payload)
        service_sys_ids[svc["name"]] = result.get("sys_id", "")

    print("\n== Relationships (Runs::Provided by) ==")
    # Map each service to devices by role heuristic: payment-frontend → leaf/border; monitoring → spine
    role_to_services: dict[str, list[str]] = {
        "spine":  ["monitoring-stack"],
        "leaf":   ["payment-frontend", "internal-tools"],
        "border": ["payment-frontend"],
        "pe":     ["internal-tools"],
    }
    for dev in devices:
        dev_sys_id = device_sys_ids.get(dev["name"], "")
        if not dev_sys_id:
            continue
        role = dev.get("role", "")
        for svc_name in role_to_services.get(role, []):
            svc_sys_id = service_sys_ids.get(svc_name, "")
            if svc_sys_id:
                print(f"  {dev['name']} --[Runs::Provided by]--> {svc_name}")
                client.create_rel(dev_sys_id, svc_sys_id, "Runs::Provided by")

    print("\n== Sample incident (for bonsai EM round-trip test) ==")
    client.upsert("incident", "short_description", "bonsai: test connectivity incident", {
        "short_description": "bonsai: test connectivity incident",
        "description": "Pre-seeded sample incident for testing bonsai → ServiceNow Event Management round-trip.",
        "category": "network",
        "priority": "3",
        "state": "1",
        "source": "bonsai",
        "u_bonsai_detection_id": "",
    })

    print("\nSeed complete.")


def main() -> None:
    parser = argparse.ArgumentParser(description="Seed a ServiceNow PDI with bonsai lab topology.")
    parser.add_argument("--topology", default="lab/seed/topology.yaml", help="Path to topology YAML")
    parser.add_argument("--dry-run", action="store_true", help="Print what would be done without making API calls")
    parser.add_argument(
        "--use-vault",
        action="store_true",
        help="(future) Read credentials from the bonsai credential vault instead of env vars."
        " Not yet implemented — will exit with an error.",
    )
    args = parser.parse_args()

    if args.use_vault:
        print("ERROR: --use-vault is not yet implemented.")
        print("  Vault-backed credential resolution requires the bonsai gRPC API.")
        print("  Use env vars SNOW_INSTANCE_URL / SNOW_USERNAME / SNOW_PASSWORD for now.")
        sys.exit(2)

    instance_url = os.environ.get("SNOW_INSTANCE_URL", "").strip()
    username = os.environ.get("SNOW_USERNAME", "").strip()
    password = os.environ.get("SNOW_PASSWORD", "").strip()

    if not instance_url or not username or not password:
        print("ERROR: set SNOW_INSTANCE_URL, SNOW_USERNAME, SNOW_PASSWORD environment variables.")
        print("  Example:")
        print("    export SNOW_INSTANCE_URL=https://devXXXXXX.service-now.com")
        print("    export SNOW_USERNAME=admin")
        print("    export SNOW_PASSWORD=your_pdi_password")
        sys.exit(1)

    topology_path = Path(args.topology)
    if not topology_path.exists():
        print(f"ERROR: topology file not found: {topology_path}")
        sys.exit(1)

    print(f"Loading topology from {topology_path}")
    topology = load_topology(str(topology_path))

    print(f"Connecting to {instance_url} as {username}" + (" [dry-run]" if args.dry_run else ""))
    client = SnowClient(instance_url, username, password, dry_run=args.dry_run)

    # Quick connectivity check
    if not args.dry_run:
        try:
            client.get("sys_properties", "name=glide.sys.domain", "sys_id")
            print("Connection OK\n")
        except Exception as e:
            print(f"ERROR: cannot connect to ServiceNow: {e}")
            sys.exit(1)

    seed(client, topology)


if __name__ == "__main__":
    main()

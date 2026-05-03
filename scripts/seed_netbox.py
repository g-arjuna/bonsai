#!/usr/bin/env python3
"""Seed NetBox with the lab topology from lab/seed/topology.yaml.

Usage:
    python scripts/seed_netbox.py [--url http://localhost:8000] [--token bonsai-dev-token]
    python scripts/seed_netbox.py --reset   # wipe bonsai-managed objects then re-seed

Idempotent: re-running is safe — existing objects are updated, not duplicated.
--reset deletes bonsai-managed devices, sites, platforms, device-types, and roles
before re-seeding. NetBox stays up; no data outside bonsai's topology is affected.
"""
import argparse
import sys
import time
from pathlib import Path

import requests
import yaml

REPO_ROOT = Path(__file__).parent.parent
TOPOLOGY_FILE = REPO_ROOT / "lab" / "seed" / "topology.yaml"

MANUFACTURER_NAME = "Lab-Vendor"


def load_topology() -> dict:
    with open(TOPOLOGY_FILE) as f:
        return yaml.safe_load(f)


def delete_if_exists(session: requests.Session, base_url: str, endpoint: str,
                     lookup_field: str, lookup_value: str) -> bool:
    """Delete the first object matching lookup, return True if deleted."""
    url = f"{base_url}/api/{endpoint}/?{lookup_field}={lookup_value}"
    resp = session.get(url)
    if resp.status_code != 200:
        return False
    results = resp.json().get("results", [])
    if not results:
        return False
    obj_id = results[0]["id"]
    del_resp = session.delete(f"{base_url}/api/{endpoint}/{obj_id}/")
    if del_resp.status_code == 204:
        print(f"  deleted {endpoint} '{lookup_value}'")
        return True
    print(f"  WARNING: delete {endpoint} '{lookup_value}' returned {del_resp.status_code}", file=sys.stderr)
    return False


def reset(base_url: str, token: str):
    """Delete all bonsai-managed NetBox objects derived from topology.yaml."""
    topo = load_topology()

    session = requests.Session()
    session.headers.update({
        "Authorization": f"Token {token}",
        "Content-Type": "application/json",
        "Accept": "application/json",
    })

    wait_for_netbox(base_url)

    # Devices (and their interfaces/IPs cascade in NetBox)
    print("Resetting devices ...")
    for device in topo["devices"]:
        delete_if_exists(session, base_url, "dcim/devices", "name", device["name"])

    # Sites
    print("Resetting sites ...")
    for site in topo["sites"]:
        delete_if_exists(session, base_url, "dcim/sites", "name", site["name"])

    # Platforms
    print("Resetting platforms ...")
    seen_platforms: set[str] = set()
    for device in topo["devices"]:
        name = device.get("netbox_platform", device["vendor"])
        if name not in seen_platforms:
            seen_platforms.add(name)
            delete_if_exists(session, base_url, "dcim/platforms", "name", name)

    # Device types
    print("Resetting device types ...")
    seen_models: set[str] = set()
    for device in topo["devices"]:
        model = device.get("netbox_model", device["vendor"])
        if model not in seen_models:
            seen_models.add(model)
            delete_if_exists(session, base_url, "dcim/device-types", "model", model)

    # Device roles
    print("Resetting device roles ...")
    seen_roles: set[str] = set()
    for device in topo["devices"]:
        role = device["role"]
        if role not in seen_roles:
            seen_roles.add(role)
            delete_if_exists(session, base_url, "dcim/device-roles", "name", role)

    # Manufacturer
    print("Resetting manufacturer ...")
    delete_if_exists(session, base_url, "dcim/manufacturers", "name", MANUFACTURER_NAME)

    print("NetBox reset complete.")


def api(session: requests.Session, base_url: str, method: str, path: str, **kwargs):
    url = f"{base_url}/api/{path}"
    resp = getattr(session, method)(url, **kwargs)
    if resp.status_code not in (200, 201):
        print(f"  ERROR {method.upper()} {url}: {resp.status_code} — {resp.text[:200]}", file=sys.stderr)
        resp.raise_for_status()
    return resp.json()


def get_or_create(session, base_url, endpoint, lookup_field, lookup_value, payload):
    """Upsert: look up by lookup_field, create if missing, update if present."""
    resp = api(session, base_url, "get", f"{endpoint}/?{lookup_field}={lookup_value}")
    results = resp.get("results", [])
    if results:
        obj_id = results[0]["id"]
        api(session, base_url, "patch", f"{endpoint}/{obj_id}/", json=payload)
        return results[0]
    return api(session, base_url, "post", f"{endpoint}/", json=payload)


def wait_for_netbox(base_url: str, timeout: int = 120):
    print(f"Waiting for NetBox at {base_url}/api/ ...")
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            r = requests.get(f"{base_url}/api/", timeout=5)
            if r.status_code == 200:
                print("  NetBox ready.")
                return
        except Exception:
            pass
        time.sleep(5)
    print("ERROR: NetBox did not become ready in time.", file=sys.stderr)
    sys.exit(1)


def seed(base_url: str, token: str):
    topo = load_topology()

    session = requests.Session()
    session.headers.update({
        "Authorization": f"Token {token}",
        "Content-Type": "application/json",
        "Accept": "application/json",
    })

    wait_for_netbox(base_url)

    # 1. Manufacturer
    print("Seeding manufacturer ...")
    mfr = get_or_create(session, base_url, "dcim/manufacturers", "name", MANUFACTURER_NAME,
                         {"name": MANUFACTURER_NAME, "slug": "lab-vendor"})

    # 2. Sites
    print("Seeding sites ...")
    site_ids: dict[str, int] = {}
    for site in topo["sites"]:
        obj = get_or_create(session, base_url, "dcim/sites", "name", site["name"], {
            "name": site["name"],
            "slug": site["name"].lower().replace(" ", "-"),
            "status": "active",
            "description": site.get("description", ""),
        })
        site_ids[site["id"]] = obj["id"]
        print(f"  site {site['name']} → id={obj['id']}")

    # 3. Platforms
    print("Seeding platforms ...")
    platform_ids: dict[str, int] = {}
    for device in topo["devices"]:
        platform_name = device.get("netbox_platform", device["vendor"])
        if platform_name in platform_ids:
            continue
        obj = get_or_create(session, base_url, "dcim/platforms", "name", platform_name, {
            "name": platform_name,
            "slug": platform_name.lower().replace(" ", "-"),
            "manufacturer": mfr["id"],
        })
        platform_ids[platform_name] = obj["id"]

    # 4. Device types (one per model)
    print("Seeding device types ...")
    dtype_ids: dict[str, int] = {}
    for device in topo["devices"]:
        model = device.get("netbox_model", device["vendor"])
        if model in dtype_ids:
            continue
        obj = get_or_create(session, base_url, "dcim/device-types", "model", model, {
            "manufacturer": mfr["id"],
            "model": model,
            "slug": model.lower().replace(" ", "-").replace("(", "").replace(")", ""),
        })
        dtype_ids[model] = obj["id"]

    # 5. Device roles
    print("Seeding device roles ...")
    role_ids: dict[str, int] = {}
    for device in topo["devices"]:
        role = device["role"]
        if role in role_ids:
            continue
        obj = get_or_create(session, base_url, "dcim/device-roles", "name", role, {
            "name": role,
            "slug": role,
            "color": "0080ff",
        })
        role_ids[role] = obj["id"]

    # 6. Devices
    print("Seeding devices ...")
    device_ids: dict[str, int] = {}
    for device in topo["devices"]:
        model = device.get("netbox_model", device["vendor"])
        platform_name = device.get("netbox_platform", device["vendor"])
        site_obj = next(s for s in topo["sites"] if s["id"] == f"site-{device['site']}"
                        or s["name"] == device["site"])
        obj = get_or_create(session, base_url, "dcim/devices", "name", device["name"], {
            "name": device["name"],
            "device_type": dtype_ids[model],
            "role": role_ids[device["role"]],
            "platform": platform_ids[platform_name],
            "site": site_ids[site_obj["id"]],
            "status": "active",
            "serial": device.get("netbox_serial", ""),
            "primary_ip4": None,
            "custom_fields": {
                "gnmi_address": device["address"],
                "gnmi_port": str(device["gnmi_port"]),
                "bonsai_vendor": device["vendor"],
            },
        })
        device_ids[device["name"]] = obj["id"]
        print(f"  device {device['name']} → id={obj['id']}")

    # 7. Interfaces + IP addresses
    print("Seeding interfaces and IPs ...")
    for device in topo["devices"]:
        device_id = device_ids[device["name"]]
        for iface in device.get("interfaces", []):
            iface_obj = get_or_create(
                session, base_url, "dcim/interfaces", "name",
                f"{iface['name']}&device_id={device_id}",
                {
                    "device": device_id,
                    "name": iface["name"],
                    "type": "1000base-t",
                    "description": iface.get("description", ""),
                },
            )
            if iface.get("ip"):
                get_or_create(session, base_url, "ipam/ip-addresses", "address", iface["ip"], {
                    "address": iface["ip"],
                    "assigned_object_type": "dcim.interface",
                    "assigned_object_id": iface_obj["id"],
                    "status": "active",
                })

    print("NetBox seed complete.")


def main():
    parser = argparse.ArgumentParser(description="Seed NetBox with bonsai lab topology.")
    parser.add_argument("--url", default="http://localhost:8000")
    parser.add_argument("--token", default="bonsai-dev-token")
    parser.add_argument("--reset", action="store_true",
                        help="Delete bonsai-managed objects before re-seeding (NetBox stays up)")
    args = parser.parse_args()
    if args.reset:
        reset(args.url, args.token)
    seed(args.url, args.token)


if __name__ == "__main__":
    main()

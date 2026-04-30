#!/usr/bin/env bash
# seed_lab.sh — Seed all lab targets from lab/seed/topology.yaml.
#
# Usage:
#   ./scripts/seed_lab.sh --all
#   ./scripts/seed_lab.sh --netbox [--url http://localhost:8000] [--token TOKEN]
#   ./scripts/seed_lab.sh --servicenow [--url http://localhost:8080]
#   ./scripts/seed_lab.sh --clab      # print ContainerLab topology YAML to stdout
#
# Options:
#   --all          Run all seeding steps (NetBox + ServiceNow mock)
#   --netbox       Seed NetBox only
#   --servicenow   Verify ServiceNow mock is up (mock uses its own embedded seed)
#   --clab         Generate ContainerLab topology YAML from seed (to stdout)
#   --url          Base URL override (applies to the step being run)
#   --token        API token override for NetBox

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOPOLOGY="${REPO_ROOT}/lab/seed/topology.yaml"
SEED_NETBOX="${REPO_ROOT}/scripts/seed_netbox.py"

NETBOX_URL="http://localhost:8000"
NETBOX_TOKEN="bonsai-dev-token"
SNOW_URL="http://localhost:8080"
TARGET_ALL=false
TARGET_NETBOX=false
TARGET_SNOW=false
TARGET_CLAB=false

# ── Arg parsing ───────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case "$1" in
    --all)           TARGET_ALL=true ;;
    --netbox)        TARGET_NETBOX=true ;;
    --servicenow)    TARGET_SNOW=true ;;
    --clab)          TARGET_CLAB=true ;;
    --url)           shift; NETBOX_URL="$1"; SNOW_URL="$1" ;;
    --token)         shift; NETBOX_TOKEN="$1" ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
  shift
done

if ! $TARGET_ALL && ! $TARGET_NETBOX && ! $TARGET_SNOW && ! $TARGET_CLAB; then
  echo "Usage: $0 [--all|--netbox|--servicenow|--clab] [options]"
  exit 1
fi

# ── ContainerLab topology generation ─────────────────────────────────────────

generate_clab() {
  python3 - <<'PYEOF'
import sys, yaml
from pathlib import Path

topo = yaml.safe_load(open(Path(__file__).parent.parent / "lab/seed/topology.yaml" if False else
    Path(sys.argv[0]).parent.parent / "lab/seed/topology.yaml"))

lab = topo["lab"]
out = {
    "name": lab["name"],
    "mgmt": {
        "network": lab["mgmt_network"],
        "ipv4-subnet": lab["mgmt_subnet"],
    },
    "topology": {
        "nodes": {},
        "links": [],
    }
}

for dev in topo["devices"]:
    node = {
        "kind": dev["clab_kind"],
        "image": dev["clab_image"],
        "mgmt-ipv4": dev["address"],
    }
    out["topology"]["nodes"][dev["name"]] = node

# Infer links from interface descriptions (format: "to <node> <port>")
import re
for dev in topo["devices"]:
    for iface in dev.get("interfaces", []):
        desc = iface.get("description", "")
        m = re.match(r"to (\S+) (\S+)", desc)
        if m:
            peer_node, peer_port = m.group(1), m.group(2)
            link = {
                "endpoints": [
                    f"{dev['name']}:{iface['name']}",
                    f"{peer_node}:{peer_port}",
                ]
            }
            # Only add once (skip the reverse direction)
            reverse = {"endpoints": [link["endpoints"][1], link["endpoints"][0]]}
            if link not in out["topology"]["links"] and reverse not in out["topology"]["links"]:
                out["topology"]["links"].append(link)

print(yaml.dump(out, sort_keys=False, default_flow_style=False))
PYEOF
}

# ── NetBox seeding ────────────────────────────────────────────────────────────

seed_netbox() {
  echo "→ Seeding NetBox at ${NETBOX_URL} ..."
  if command -v python3 &>/dev/null; then
    python3 "${SEED_NETBOX}" --url "${NETBOX_URL}" --token "${NETBOX_TOKEN}"
  else
    echo "ERROR: python3 not found. Install it or run from WSL." >&2
    exit 1
  fi
}

# ── ServiceNow mock verification ──────────────────────────────────────────────

verify_snow() {
  echo "→ Checking ServiceNow mock at ${SNOW_URL}/health ..."
  if curl -sf "${SNOW_URL}/health" | python3 -m json.tool; then
    echo "  ServiceNow mock is up."
  else
    echo "  ServiceNow mock not reachable. Start it with:"
    echo "  docker compose --profile servicenow-mock up -d servicenow-mock"
    exit 1
  fi
}

# ── Dispatch ──────────────────────────────────────────────────────────────────

if $TARGET_CLAB; then
  generate_clab
fi

if $TARGET_ALL || $TARGET_NETBOX; then
  seed_netbox
fi

if $TARGET_ALL || $TARGET_SNOW; then
  verify_snow
fi

echo "seed_lab.sh done."

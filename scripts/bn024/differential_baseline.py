#!/usr/bin/env python3
"""BN-024-01 — genuine differential oracle vs pinned SuzieQ parsing assets.

Executes the *actual* pinned upstream reference — SuzieQ's own TextFSM
templates (``suzieq/config/textfsm_templates/*.tfsm`` at revision
75198b5585ebef91f0659b6b1ec3c964cc2cd1ad) driven by the pinned TextFSM
library source (google/textfsm @ 7e4ed596442867fc961c0788e7cac5bc8d091da9)
— and the *actual* Bonsai parser path: the packaged PyATS sidecar
(``docker/sidecars/pyats``) which the Rust ``ParserChain`` dispatches to,
exercised over HTTP for both the ``genie`` and ``textfsm`` (ntc-templates)
backends.

Scope is deliberately bounded: SuzieQ ``iosxe`` fixtures under
``tests/integration/nossim/iosxe/16.10.1/C9300``. For each scope the report
compares typed fields record-by-record on an explicit join key and an
explicit per-field mapping table. Fields that cannot be compared are
reported as ``not_comparable`` — never silently counted as matches.

This is *not* a parity-percentage claim: every count carries its
denominator, and SuzieQ service-layer post-processing (e.g. the
``interfaces.py`` normalisation pass) is explicitly out of scope — the
reference is the pinned template execution plus the avsc schema field
list.

Usage:
    PYTHONPATH=<textfsm-src> python3 scripts/bn024/differential_baseline.py \
        --suzieq-src /path/to/suzieq-checkout \
        --pyats-url http://127.0.0.1:19101 \
        --out report.json
"""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import sys
import time
import urllib.request
from pathlib import Path

SUZIEQ_PIN = "75198b5585ebef91f0659b6b1ec3c964cc2cd1ad"
TEXTFSM_PIN = "7e4ed596442867fc961c0788e7cac5bc8d091da9"

# Bounded scope: (name, fixture, suzieq tfsm template, suzieq avsc table,
#                  bonsai vendor, bonsai command, join spec, field map)
#
# join: how to key records on each side.
#   suzieq  — template Value name (possibly composite "a+b")
#   textfsm — ntc-templates output field (composite joined with '/')
#   genie   — "dict_key" (top-level key) or extractor name below
#
# field_map: suzieq template Value name -> (ntc field, genie field).
# Genie field names refer to the flattened per-record dict produced by the
# scope's extractor; None means no honest mapping exists.
SCOPES = [
    {
        "name": "iosxe_interfaces",
        "fixture": "tests/integration/nossim/iosxe/16.10.1/C9300/show_interfaces.txt",
        "tfsm": "suzieq/config/textfsm_templates/iosxe_show_int.tfsm",
        "avsc": "interfaces",
        "vendor": "cisco_iosxe",
        "command": "show interfaces",
        "genie_extract": "interfaces_dict",
        "join": {"suzieq": "ifname", "textfsm": "interface",
                 "genie": "dict_key"},
        "field_map": {
            "ifname": ("interface", "__key__"),
            "mtu": ("mtu", "mtu"),
            "speed": ("speed", "speed"),
            "description": ("description", "description"),
            "macaddr": ("mac_address", "mac_address"),
            "type": (None, "type"),
            "adminState": (None, "enabled"),
            "state": (None, "oper_status"),
        },
    },
    {
        "name": "iosxe_ip_arp",
        "fixture": "tests/integration/nossim/iosxe/16.10.1/C9300/show_ip_arp.txt",
        "tfsm": "suzieq/config/textfsm_templates/iosxe_show_arp.tfsm",
        "avsc": "arpnd",
        "vendor": "cisco_iosxe",
        "command": "show ip arp",
        "genie_extract": "arp_neighbors",
        "join": {"suzieq": "ipAddress", "textfsm": "ip_address",
                 "genie": "ip_address"},
        "field_map": {
            "ipAddress": ("ip_address", "ip_address"),
            "macaddr": ("mac_address", "mac_address"),
            "type": ("type", "type"),
            "oif": ("interface", "interface"),
            "protocol": ("protocol", "protocol"),
        },
    },
    {
        "name": "iosxe_routes",
        "fixture": "tests/integration/nossim/iosxe/16.10.1/C9300/show_ip_route_vrf_all.txt",
        "tfsm": "suzieq/config/textfsm_templates/iosxe_show_route.tfsm",
        "avsc": "routes",
        "vendor": "cisco_iosxe",
        "command": "show ip route vrf all",
        "genie_extract": "bgp_routes",
        # SuzieQ's route template emits `prefix` without a mask for /32
        # host routes and `netmask` separately; the service layer rejoins
        # them. "route_prefix" normalises to canonical a.b.c.d/p form.
        "join": {"suzieq": "route_prefix", "textfsm": "network+prefix_length",
                 "genie": "route"},
        "field_map": {
            "prefix": ("network+prefix_length", "route"),
            "protocol": ("protocol", "source_protocol_codes"),
            "preference": ("distance", "route_preference"),
            "metric": ("metric", "metric"),
            "nexthopIps": ("nexthop_ip", None),
            "statusChangeTimestamp": ("uptime", None),
        },
    },
]


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def norm(value) -> str | None:
    if value is None:
        return None
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    s = str(value).strip()
    return s if s else None


def run_suzieq_template(textfsm_mod, tfsm_path: Path, raw: str):
    """Execute the pinned SuzieQ .tfsm template with pinned TextFSM."""
    with open(tfsm_path, "r", encoding="utf-8") as fh:
        tfsm = textfsm_mod.TextFSM(fh)
    rows = tfsm.ParseText(raw)
    return [dict(zip(tfsm.header, row)) for row in rows]


def sidecar_parse(pyats_url: str, wire_parser: str, vendor: str,
                  command: str, raw: str) -> dict:
    body = {
        "parser": wire_parser,
        "vendor": vendor,
        "command": command,
        "command_pattern": command,
        "raw_output": raw,
    }
    req = urllib.request.Request(
        f"{pyats_url.rstrip('/')}/parse",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    t0 = time.perf_counter()
    with urllib.request.urlopen(req, timeout=120) as resp:
        payload = json.loads(resp.read())
    payload["_elapsed_ms"] = round((time.perf_counter() - t0) * 1000.0, 2)
    return payload


def _join_key(record: dict, spec: str) -> str | None:
    """Composite join spec: 'field' or 'a+b' joined with '/'."""
    parts = []
    for f in spec.split("+"):
        v = norm(record.get(f))
        if v is None:
            return None
        parts.append(v)
    return "/".join(parts)


def extract_bonsai_records(scope: dict, backend: str, parsed):
    """Return {join_key: record_dict} for a structured sidecar payload."""
    if parsed is None:
        return {}
    join_spec = scope["join"][backend]
    out: dict[str, dict] = {}

    if backend == "textfsm":
        if not isinstance(parsed, list):
            return {}
        for row in parsed:
            if not isinstance(row, dict):
                continue
            k = _join_key(row, join_spec)
            if k:
                out.setdefault(k, row)
        return out

    # genie — nested structure; scope-specific flattening
    kind = scope.get("genie_extract", "interfaces_dict")
    if kind == "interfaces_dict":
        if not isinstance(parsed, dict):
            return {}
        for name, attrs in parsed.items():
            if isinstance(attrs, dict):
                rec = dict(attrs)
                rec["__key__"] = str(name)
                out[str(name)] = rec
        return out
    if kind == "arp_neighbors":
        ifaces = parsed.get("interfaces", {}) if isinstance(parsed, dict) else {}
        for ifname, blk in ifaces.items():
            neigh = (((blk or {}).get("ipv4") or {}).get("neighbors") or {})
            for ip, n in neigh.items():
                if isinstance(n, dict):
                    rec = dict(n)
                    rec["ip_address"] = norm(n.get("ip")) or str(ip)
                    rec["mac_address"] = n.get("link_layer_address")
                    rec["interface"] = ifname
                    k = _join_key(rec, join_spec) or str(ip)
                    out.setdefault(k, rec)
        return out
    if kind == "bgp_routes":
        vrfs = parsed.get("vrf", {}) if isinstance(parsed, dict) else {}
        for vrf_name, vrf_blk in vrfs.items():
            afs = ((vrf_blk or {}).get("address_family") or {})
            for af_name, af_blk in afs.items():
                routes = ((af_blk or {}).get("routes") or {})
                for prefix, r in routes.items():
                    if isinstance(r, dict):
                        rec = dict(r)
                        rec["route"] = norm(r.get("route")) or str(prefix)
                        rec["vrf"] = vrf_name
                        k = _join_key(rec, join_spec) or str(prefix)
                        out.setdefault(k, rec)
        return out
    return {}


def _route_prefix(rec: dict) -> str | None:
    """Canonical a.b.c.d/p for a SuzieQ route template record."""
    prefix = norm(rec.get("prefix"))
    if prefix is None:
        return None
    if "/" in prefix:
        return prefix
    mask = norm(rec.get("netmask"))
    return f"{prefix}/{mask}" if mask else prefix


def _sq_join_key(scope: dict, rec: dict) -> str | None:
    spec = scope["join"]["suzieq"]
    if spec == "route_prefix":
        return _route_prefix(rec)
    if "+" in spec:
        return _join_key(rec, spec)
    return norm(rec.get(spec))


def _sq_field_value(scope: dict, rec: dict, field: str):
    if field == "prefix" and scope["join"]["suzieq"] == "route_prefix":
        return _route_prefix(rec)
    v = rec.get(field)
    if isinstance(v, list):
        v = ",".join(norm(x) or "" for x in v)
    return norm(v)


def compare_scope(scope: dict, suzieq_records: list[dict],
                  bonsai_results: dict[str, dict], avsc_fields: list[str]):
    """Record-level field comparison on the scope join key."""
    field_map = scope["field_map"]
    join_sq = scope["join"]["suzieq"]

    sq_by_key = {}
    for rec in suzieq_records:
        k = _sq_join_key(scope, rec)
        if k:
            sq_by_key.setdefault(k, rec)

    report = {"suzieq_records": len(suzieq_records),
              "suzieq_join": join_sq,
              "suzieq_avsc_field_count": len(avsc_fields),
              "backends": {}}

    for backend, payload in bonsai_results.items():
        parsed = payload.get("parsed_json")
        outcome = payload.get("outcome")
        backend_used = payload.get("backend_used")
        entry = {
            "backend_used": backend_used,
            "outcome": outcome,
            "elapsed_ms": payload.get("_elapsed_ms"),
        }
        if outcome != "structured_success" or parsed is None:
            entry.update({
                "status": "not_comparable",
                "reason": f"bonsai outcome '{outcome}' is not structured_success",
            })
            report["backends"][backend] = entry
            continue

        bonsai_by_key = extract_bonsai_records(scope, backend, parsed)
        joined = sorted(set(sq_by_key) & set(bonsai_by_key))
        stats = {"records_joined": len(joined),
                 "records_suzieq_only": len(set(sq_by_key) - set(bonsai_by_key)),
                 "records_bonsai_only": len(set(bonsai_by_key) - set(sq_by_key)),
                 "fields": {}, "sample_mismatches": []}

        for sq_field, (ntc_field, genie_field) in field_map.items():
            bf = ntc_field if backend == "textfsm" else genie_field
            if bf is None:
                stats["fields"][sq_field] = {"status": "not_comparable",
                                             "reason": "no bonsai-side mapping"}
                continue
            compared = matched = mismatched = missing_bonsai = missing_sq = 0
            for key in joined:
                sq_val = _sq_field_value(scope, sq_by_key[key], sq_field)
                if backend == "genie" and bf == "__key__":
                    b_val = key
                else:
                    brec = bonsai_by_key[key]
                    if "+" in bf:
                        b_raw = _join_key(brec, bf)
                    else:
                        b_raw = brec.get(bf)
                        if isinstance(b_raw, list):
                            b_raw = ",".join(norm(x) or "" for x in b_raw)
                    b_val = norm(b_raw)
                if sq_val is None and b_val is None:
                    continue
                if sq_val is None:
                    missing_sq += 1
                    continue
                if b_val is None:
                    missing_bonsai += 1
                    continue
                compared += 1
                if sq_val == b_val:
                    matched += 1
                else:
                    mismatched += 1
                    if len(stats["sample_mismatches"]) < 5:
                        stats["sample_mismatches"].append(
                            {"key": key, "field": sq_field,
                             "suzieq": sq_val, "bonsai": b_val})
            stats["fields"][sq_field] = {
                "status": "compared" if compared else "no_values",
                "compared": compared, "matched": matched,
                "mismatched": mismatched,
                "missing_in_bonsai": missing_bonsai,
                "missing_in_suzieq": missing_sq,
            }
        entry.update(stats)
        report["backends"][backend] = entry

    return report


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--suzieq-src", required=True, type=Path,
                    help="checkout of netenglabs/suzieq @ pinned revision")
    ap.add_argument("--textfsm-src", type=Path, default=None,
                    help="checkout of google/textfsm @ pinned revision "
                         "(or set PYTHONPATH to its parent)")
    ap.add_argument("--pyats-url", type=str, default=None,
                    help="running bonsai pyats sidecar base URL; if omitted "
                         "the Bonsai side is reported as not_executed")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()

    if args.textfsm_src:
        sys.path.insert(0, str(args.textfsm_src))
    try:
        textfsm_mod = importlib.import_module("textfsm")
    except ImportError:
        print("error: textfsm not importable; pass --textfsm-src or set "
              "PYTHONPATH to the pinned google/textfsm checkout",
              file=sys.stderr)
        return 2

    suzieq = args.suzieq_src
    schema_dir = suzieq / "suzieq" / "config" / "schema"
    results = []
    missing = []

    for scope in SCOPES:
        fixture = suzieq / scope["fixture"]
        tfsm = suzieq / scope["tfsm"]
        avsc = schema_dir / f"{scope['avsc']}.avsc"
        if not fixture.exists():
            missing.append(scope["fixture"])
            continue
        if not tfsm.exists():
            missing.append(scope["tfsm"])
            continue
        if not avsc.exists():
            missing.append(f"{scope['avsc']}.avsc")
            continue

        raw_bytes = fixture.read_bytes()
        raw = raw_bytes.decode("utf-8", errors="replace")
        t0 = time.perf_counter()
        suzieq_records = run_suzieq_template(textfsm_mod, tfsm, raw)
        suzieq_ms = (time.perf_counter() - t0) * 1000.0

        avsc_fields = [f["name"] for f in
                       json.loads(avsc.read_text()).get("fields", [])]

        bonsai_results = {}
        bonsai_status = "executed"
        if args.pyats_url:
            for wire in ("textfsm", "genie"):
                try:
                    bonsai_results[wire] = sidecar_parse(
                        args.pyats_url, wire, scope["vendor"],
                        scope["command"], raw)
                except Exception as exc:  # noqa: BLE001 - report honestly
                    bonsai_results[wire] = {
                        "outcome": "sidecar_error",
                        "backend_used": None,
                        "parsed_json": None,
                        "error": f"{type(exc).__name__}: {exc}",
                    }
        else:
            bonsai_status = "not_executed (no --pyats-url)"

        entry = {
            "scope": scope["name"],
            "fixture": scope["fixture"],
            "fixture_sha256": sha256_bytes(raw_bytes),
            "fixture_bytes": len(raw_bytes),
            "suzieq_template": scope["tfsm"],
            "suzieq_template_sha256": sha256_file(tfsm),
            "suzieq_pinned_revision": SUZIEQ_PIN,
            "textfsm_pinned_revision": TEXTFSM_PIN,
            "suzieq_records_parsed": len(suzieq_records),
            "suzieq_parse_ms": round(suzieq_ms, 2),
            "suzieq_avsc": scope["avsc"],
            "bonsai_vendor": scope["vendor"],
            "bonsai_command": scope["command"],
            "bonsai_status": bonsai_status,
            "comparison": compare_scope(scope, suzieq_records,
                                        bonsai_results, avsc_fields),
        }
        results.append(entry)

    report = {
        "baseline": "bn024-01-differential-v2",
        "method": ("pinned SuzieQ .tfsm template executed by pinned TextFSM "
                   "source vs live Bonsai pyats sidecar (genie + ntc textfsm "
                   "backends); per-record typed-field comparison on explicit "
                   "join keys and field maps; SuzieQ service-layer "
                   "post-processing out of scope"),
        "suzieq_pinned_revision": SUZIEQ_PIN,
        "textfsm_pinned_revision": TEXTFSM_PIN,
        "scopes": results,
        "missing": missing,
    }

    text = json.dumps(report, indent=2)
    if args.out:
        args.out.write_text(text + "\n")
    else:
        sys.stdout.write(text + "\n")
    if missing:
        print(f"warning: {len(missing)} asset(s) missing: {missing}",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())

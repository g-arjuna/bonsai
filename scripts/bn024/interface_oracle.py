#!/usr/bin/env python3
"""Differential oracle: pinned SuzieQ textfsm + cumulus/sonic cleaner vs
the native Rust interface normalizer.

Usage:
  # produce upstream normalized records from the fixture pair
  python3 scripts/bn024/interface_oracle.py upstream \
      --suzieq-dir /tmp/suzieq-pinned \
      --fixture-dir tests/fixtures/bn024/interface-cumulus \
      --out /tmp/oracle-upstream.json

  # compare against Rust-emitted normalization (BONSAI_ORACLE_OUT artifact)
  python3 scripts/bn024/interface_oracle.py compare \
      --upstream /tmp/oracle-upstream.json \
      --rust target/bn024-interface-oracle-rust.json

Upstream path: cl_interfaces.tfsm @ suzieq 75198b5 + verbatim
merge_results/_entry_cumulus_sonic_cleaner semantics (interfaces.py is
exec'd with stubbed imports — the cleaner code itself is unmodified
pinned source). Requires the `textfsm` package (pinned google/textfsm
@ 7e4ed59).
"""
import argparse
import importlib.util
import json
import sys
import types
from pathlib import Path


def _stub_module(name, **attrs):
    m = types.ModuleType(name)
    for k, v in attrs.items():
        setattr(m, k, v)
    sys.modules[name] = m
    return m


def load_interface_service(suzieq_dir: Path):
    """Import pinned interfaces.py with stubbed heavy deps."""
    _stub_module("numpy")
    _stub_module("suzieq")
    _stub_module("suzieq.shared")
    utils = _stub_module(
        "suzieq.shared.utils",
        MISSING_SPEED=-1,
        MISSING_SPEED_IF_TYPES=["ethernet", "bond", "bond_slave"],
        NO_SPEED=0,
        convert_macaddr_format_to_colon=lambda m: m,
        expand_ios_ifname=lambda s: s,
        expand_nxos_ifname=lambda s: s,
        get_timestamp_from_junos_time=lambda s: s,
        normalize_junos_field=lambda s: s,
        parse_relative_timestamp=lambda s: s,
    )
    service_mod = _stub_module(
        "suzieq.poller.worker.services.service", Service=object
    )
    for pkg in [
        "suzieq.poller",
        "suzieq.poller.worker",
        "suzieq.poller.worker.services",
    ]:
        _stub_module(pkg)
    sys.modules["suzieq.poller.worker.services"].Service = service_mod.Service

    path = suzieq_dir / "suzieq/poller/worker/services/interfaces.py"
    spec = importlib.util.spec_from_file_location(
        "pinned_interfaces", path
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.InterfaceService


def textfsm_parse(template_path: Path, text: str):
    import textfsm

    with open(template_path) as f:
        tmpl = textfsm.TextFSM(f)
    return [dict(zip(tmpl.header, row)) for row in tmpl.ParseText(text)]


def merge_results(entries):
    """Verbatim port of service.py::merge_results for keyflds=['ifname']."""
    int_res = {}
    for entry in entries:
        keyvals = []
        for kfld in ["ifname"]:
            if not entry.get(kfld, None):
                keyvals.append("_default")
            else:
                val = entry.get(kfld, "")
                if not isinstance(val, str):
                    val = str(val)
                keyvals.append(val)
        if entry.get("_entryType", ""):
            keyvals.append(entry["_entryType"])
        key = "-".join(keyvals)
        if key not in int_res:
            int_res[key] = dict(entry)
        else:
            existing = int_res[key]
            for fld in entry:
                if entry[fld] and not existing.get(fld, None):
                    existing[fld] = entry[fld]
                elif entry[fld]:
                    existing[fld + "-_2nd"] = entry[fld]
    return list(int_res.values())


def upstream_records(suzieq_dir: Path, fixture_dir: Path):
    svc_cls = load_interface_service(suzieq_dir)
    svc = object.__new__(svc_cls)  # cleaner methods need no __init__ state

    template = suzieq_dir / "suzieq/config/textfsm_templates/cl_interfaces.tfsm"
    vtysh = (fixture_dir / "vtysh-show-int-vrf-all.txt").read_text()
    ipaddr = (fixture_dir / "ip-d-address-show.txt").read_text()
    # Upstream runs both as one shell command: concatenated output, one
    # textfsm pass, then merge_results over the entry list.
    entries = textfsm_parse(template, vtysh + "\n" + ipaddr)
    merged = merge_results(entries)
    for e in merged:
        svc._entry_cumulus_sonic_cleaner(e)
    return merged


def cmd_upstream(args):
    recs = upstream_records(Path(args.suzieq_dir), Path(args.fixture_dir))
    slim = []
    for e in recs:
        slim.append(
            {
                k: e.get(k)
                for k in [
                    "ifname",
                    "adminState",
                    "state",
                    "type",
                    "mtu",
                    "speed",
                    "macaddr",
                    "master",
                    "ipAddressList",
                    "ip6AddressList",
                    "vrf",
                ]
            }
        )
    Path(args.out).write_text(json.dumps(slim, indent=2, sort_keys=True))
    print(f"wrote {len(slim)} upstream records -> {args.out}")


def _norm_state(v):
    # upstream emits '' for fields the textfsm record never captured;
    # that is absence, identical to rust `unknown`/None.
    if v is None or v == "":
        return None
    return str(v).lower()


def compare(upstream_path: Path, rust_path: Path):
    up = {r["ifname"]: r for r in json.loads(upstream_path.read_text())}
    rust = json.loads(rust_path.read_text())
    rints = {i["ifname"]: i for i in rust["interfaces"]}
    facts = {f["ifname"]: f for f in rust["facts"] if f.get("ifname")}

    mismatches = []
    only_up = sorted(set(up) - set(rints))
    only_rust = sorted(set(rints) - set(up))
    if only_up:
        mismatches.append(f"ifnames only upstream: {only_up}")
    if only_rust:
        mismatches.append(f"ifnames only rust: {only_rust}")

    KNOWN_DEVIATIONS = []
    for name in sorted(set(up) & set(rints)):
        u, d = up[name], rints[name]
        f = facts.get(name, {})
        payload = f.get("payload") or {}
        # adminState / state: compare typed payload values (upstream
        # admin-down forcing is part of the cleaner contract we mirror).
        for uf, pf in [("adminState", "administrative_state"), ("state", "operational_state")]:
            uv = _norm_state(u.get(uf))
            fld = payload.get(pf) or {}
            dv = fld.get("value") if fld.get("state") == "known" else None
            dv = _norm_state(dv)
            # upstream state vocabulary is up|down only; map rust
            # lower_layer_down/testing conservatively to 'down'-class is
            # wrong — flag any non-up/down rust value as mismatch unless
            # upstream also lacks the field.
            if uv != dv:
                mismatches.append(f"{name}.{uf}: upstream={uv!r} rust={dv!r} (state={fld.get('state')!r})")
        # type (post-cleaner raw string vs post-_2nd raw string).
        # Upstream cleaner rewrites 'ether'->'ethernet' cosmetically;
        # apply the same fixup to the rust raw value before comparing.
        ut = (u.get("type") or "").lower() or None
        dt = (d.get("type_raw") or "").lower() or None
        if dt == "ether":
            dt = "ethernet"
        if ut != dt:
            mismatches.append(f"{name}.type: upstream={ut!r} rust={dt!r}")
        # mtu
        um, dm = u.get("mtu"), d.get("mtu_raw")
        um = int(um) if um not in (None, "") else None
        dm = int(dm) if dm not in (None, "") else None
        if um != dm:
            mismatches.append(f"{name}.mtu: upstream={um!r} rust={dm!r}")
        # macaddr
        um, dm = (u.get("macaddr") or "").lower() or None, d.get("mac_raw")
        if um != dm:
            mismatches.append(f"{name}.macaddr: upstream={um!r} rust={dm!r}")
        # master (upstream post-clean: parens stripped, lowercased)
        um = u.get("master") or ""
        um = um.replace("(", "").replace(")", "").lower() or None
        dm = payload.get("member_of")
        if um != dm:
            mismatches.append(f"{name}.master/member_of: upstream={um!r} rust={dm!r}")
        # addresses
        for uf, df in [
            ("ipAddressList", "ip_addresses"),
            ("ip6AddressList", "ipv6_addresses"),
        ]:
            uv = u.get(uf) or []
            dv = d.get(df) or []
            if list(uv) != list(dv):
                mismatches.append(f"{name}.{uf}: upstream={uv!r} rust={dv!r}")
        # speed: upstream post-clean int/-1/0 vs rust typed field
        us = u.get("speed")
        try:
            us = int(us)
        except (TypeError, ValueError):
            us = None
        sp = (payload.get("speed") or {})
        if us is None:
            mismatches.append(f"{name}.speed: upstream=missing rust_state={sp.get('state')!r}")
        elif us == -1 or us == 0 or us == 4294967295:
            # upstream missing/no-speed/sentinel -> rust must be Unknown
            if sp.get("state") != "unknown":
                mismatches.append(
                    f"{name}.speed: upstream={us} rust_state={sp.get('state')!r}"
                )
        else:
            sv = (sp.get("value") or {})
            if sp.get("state") != "known" or sv.get("value") != us:
                mismatches.append(
                    f"{name}.speed: upstream={us} rust={sp!r}"
                )

    print(f"upstream records: {len(up)}, rust interfaces: {len(rints)}")
    if mismatches:
        print(f"MISMATCHES ({len(mismatches)}):")
        for m in mismatches[:80]:
            print("  " + m)
        return 1
    print("ORACLE: all compared fields match")
    return 0


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    u = sub.add_parser("upstream")
    u.add_argument("--suzieq-dir", required=True)
    u.add_argument("--fixture-dir", required=True)
    u.add_argument("--out", required=True)
    c = sub.add_parser("compare")
    c.add_argument("--upstream", required=True)
    c.add_argument("--rust", required=True)
    args = ap.parse_args()
    if args.cmd == "upstream":
        cmd_upstream(args)
        return 0
    return compare(Path(args.upstream), Path(args.rust))


if __name__ == "__main__":
    sys.exit(main())

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


def _typed_field(payload: dict, name: str) -> dict:
    return payload.get(name) or {}


def _typed_value(payload: dict, name: str):
    """Typed payload value iff the field state is `known`, else None."""
    fld = _typed_field(payload, name)
    return fld.get("value") if fld.get("state") == "known" else None


# Post-cleaner upstream `type` strings -> the typed `interface_type`
# classification the native normalizer is contracted to produce
# (src/interface_facts.rs InterfaceType). This table is an independent
# restatement of that contract for oracle purposes: an upstream type
# absent here is an oracle coverage gap and fails closed rather than
# being silently exempted. Upstream '' means "type not captured" and
# requires the rust field to be non-`known`.
UPSTREAM_TYPE_TO_TYPED = {
    "": None,  # not captured -> rust must report unknown
    "ethernet": "physical",
    "bond": "port_channel",
    "bond_slave": "physical",
    "loopback": "loopback",
    "vlan": "virtual",
    "macvlan": "virtual",
    "bridge": "virtual",
    "vrf": "virtual",
    "vxlan": "tunnel",
}


def compare(upstream_path: Path, rust_path: Path):
    up = {r["ifname"]: r for r in json.loads(upstream_path.read_text())}
    rust = json.loads(rust_path.read_text())
    rints = rust["interfaces"]
    facts_list = rust["facts"]

    mismatches = []
    deviations = []
    uncompared = []

    # ── Artifact integrity: exactly one typed fact per interface ──────
    # The dict comprehensions below would silently collapse duplicates;
    # count first so a duplicated or missing fact is a hard mismatch.
    fact_names = [f.get("ifname") for f in facts_list]
    iface_names = [i["ifname"] for i in rints]
    dup_facts = sorted({n for n in fact_names if n is not None and fact_names.count(n) > 1})
    dup_ifaces = sorted({n for n in iface_names if iface_names.count(n) > 1})
    if dup_facts:
        mismatches.append(f"duplicate typed facts for ifnames: {dup_facts}")
    if dup_ifaces:
        mismatches.append(f"duplicate interface diagnostics: {dup_ifaces}")
    fact_iface_set = {n for n in fact_names if n is not None}
    iface_set = set(iface_names)
    if fact_iface_set != iface_set:
        mismatches.append(
            f"fact/interface population differs: "
            f"facts-only={sorted(fact_iface_set - iface_set)} "
            f"interfaces-only={sorted(iface_set - fact_iface_set)}"
        )
    status_facts = [f for f in facts_list if f.get("ifname") is None]
    for f in facts_list:
        if f.get("ifname") is not None and not f.get("payload"):
            mismatches.append(f"{f.get('ifname')}: fact missing typed payload")
    rints_by_name = {i["ifname"]: i for i in rints}
    facts = {n: f for n, f in zip(fact_names, facts_list) if n is not None}

    # ── Section 1: interface population parity ────────────────────────
    only_up = sorted(set(up) - iface_set)
    only_rust = sorted(iface_set - set(up))
    if only_up:
        mismatches.append(f"ifnames only upstream: {only_up}")
    if only_rust:
        mismatches.append(f"ifnames only rust: {only_rust}")

    # ── Section 2: typed semantic payload parity ──────────────────────
    for name in sorted(set(up) & iface_set):
        u = up[name]
        f = facts.get(name, {})
        payload = f.get("payload") or {}
        subj = f.get("subject") or {}

        # Subject identity axes: entity must be the interface name and
        # namespace/device identity must be present and consistent.
        if subj.get("entity") != name:
            mismatches.append(
                f"{name}.subject.entity: expected {name!r} got {subj.get('entity')!r}"
            )
        if not subj.get("device_id") or not subj.get("namespace"):
            mismatches.append(f"{name}.subject: missing device_id/namespace")
        pin = payload.get("interface_name")
        if pin != name:
            mismatches.append(
                f"{name}.payload.interface_name: expected {name!r} got {pin!r}"
            )

        # adminState / state: typed payload values (upstream
        # admin-down forcing is part of the cleaner contract we mirror).
        for uf, pf in [("adminState", "administrative_state"), ("state", "operational_state")]:
            uv = _norm_state(u.get(uf))
            fld = _typed_field(payload, pf)
            dv = _norm_state(_typed_value(payload, pf))
            if uv is None and fld.get("state") == "known" and pf == "operational_state":
                # KNOWN DEVIATION (documented): the upstream textfsm
                # `state` Value is captured ONLY from the vtysh
                # "line protocol is <state>" rule — the iproute2 header
                # `state <WORD>` is consumed but never captured. The
                # native normalizer additionally uses the iproute2
                # operational state as evidence (A5 admin/oper
                # separation), so ip-only interfaces legitimately carry
                # a rust oper state where upstream has none. Upstream
                # state is never overridden by this path: when the
                # vtysh line-protocol value exists it must still match.
                deviations.append(
                    f"{name}.state: upstream absent, rust={dv!r} "
                    "(iproute2 operstate evidence — documented deviation)"
                )
                continue
            if uv != dv:
                mismatches.append(f"{name}.{uf}: upstream={uv!r} rust={dv!r} (state={fld.get('state')!r})")

        # interface_type: typed classification vs upstream post-cleaner
        # type string via the explicit mapping table (fail closed on
        # unmapped upstream vocabulary).
        ut = (u.get("type") or "").lower()
        expected = UPSTREAM_TYPE_TO_TYPED.get(ut, "__UNMAPPED__")
        itf = _typed_field(payload, "interface_type")
        if expected == "__UNMAPPED__":
            mismatches.append(f"{name}.type: unmapped upstream type {ut!r}")
        elif expected is None:
            if itf.get("state") == "known":
                mismatches.append(
                    f"{name}.type: upstream type absent but rust typed "
                    f"{itf.get('value')!r}"
                )
        else:
            tv = itf.get("value") if itf.get("state") == "known" else None
            if tv != expected:
                mismatches.append(
                    f"{name}.type: upstream={ut!r} expected_typed={expected!r} rust={tv!r}"
                )

        # mtu: typed ObservedField<u32> value vs upstream mtu.
        um = u.get("mtu")
        um = int(um) if um not in (None, "") else None
        mtf = _typed_field(payload, "mtu")
        mv = mtf.get("value") if mtf.get("state") == "known" else None
        if um is None:
            if mtf.get("state") == "known":
                mismatches.append(
                    f"{name}.mtu: upstream absent but rust typed {mv!r}"
                )
        elif mv != um:
            mismatches.append(
                f"{name}.mtu: upstream={um!r} rust={mv!r} (state={mtf.get('state')!r})"
            )

        # macaddr: typed normalized lowercase colon form.
        um = (u.get("macaddr") or "").lower() or None
        macf = _typed_field(payload, "mac_address")
        mc = macf.get("value") if macf.get("state") == "known" else None
        mc = mc.lower() if isinstance(mc, str) else mc
        if um != mc:
            mismatches.append(f"{name}.macaddr: upstream={um!r} rust={mc!r}")

        # master: typed relationship fields vs upstream post-clean
        # master (parens stripped, lowercased). The typed payload
        # distinguishes LAG membership (`member_of`), physical parent
        # (`parent_interface`) and the raw classified relationship
        # (`master_relationship.target`); the effective observed master
        # target is whichever is populated.
        um = u.get("master") or ""
        um = um.replace("(", "").replace(")", "").lower() or None
        dm = (
            payload.get("member_of")
            or payload.get("parent_interface")
            or (payload.get("master_relationship") or {}).get("target")
        )
        if isinstance(dm, str):
            dm = dm.lower() or None
        if um != dm:
            mismatches.append(f"{name}.master/member_of: upstream={um!r} rust={dm!r}")

        # addresses: typed ObservedField<Vec<String>> values.
        for uf, df in [
            ("ipAddressList", "ip_addresses"),
            ("ip6AddressList", "ipv6_addresses"),
        ]:
            uv = u.get(uf) or []
            af = _typed_field(payload, df)
            dv = af.get("value") if af.get("state") == "known" else None
            if uf == "ip6AddressList" and dv is not None and list(dv) == list(uv) + ["::1/128"]:
                # KNOWN DEVIATION (documented): the upstream cleaner
                # deletes "::1/128" ("Remove loopbacks"); the typed
                # payload preserves observed addresses verbatim —
                # scope/loopback classification is a consumer concern.
                deviations.append(
                    f"{name}.ip6AddressList: rust retains ::1/128 "
                    "(upstream removes loopbacks — documented deviation)"
                )
                continue
            if af.get("state") == "known":
                if list(uv) != list(dv or []):
                    mismatches.append(f"{name}.{uf}: upstream={uv!r} rust={dv!r}")
            elif uv:
                mismatches.append(
                    f"{name}.{uf}: upstream has {len(uv)} addr(s) but rust state={af.get('state')!r}"
                )

        # speed: upstream post-clean int/-1/0 vs typed
        # SpeedObservation{value,unit}. Value parity is checked; the
        # declared unit is oracle-uncompared (profiles own unit claims).
        us = u.get("speed")
        try:
            us = int(us)
        except (TypeError, ValueError):
            us = None
        sp = _typed_field(payload, "speed")
        if us is None:
            mismatches.append(f"{name}.speed: upstream=missing rust_state={sp.get('state')!r}")
        elif us == -1 or us == 0 or us == 4294967295:
            # upstream missing/no-speed/sentinel -> rust must be Unknown
            if sp.get("state") != "unknown":
                mismatches.append(
                    f"{name}.speed: upstream={us} rust_state={sp.get('state')!r}"
                )
        else:
            sv = sp.get("value") or {}
            if sp.get("state") != "known" or sv.get("value") != us:
                mismatches.append(
                    f"{name}.speed: upstream={us} rust={sp!r}"
                )
        if sp.get("state") == "known" and not (sp.get("value") or {}).get("unit"):
            uncompared.append(f"{name}.speed.unit (typed unit claim, no upstream axis)")

    # ── Section 3: raw extraction parity (diagnostics vs upstream) ────
    # The `interfaces` diagnostics preserve the raw captured strings;
    # upstream post-cleaner values are compared with the same cosmetic
    # fixups the typed path documents. Divergence here indicates the
    # normalizer lost or altered raw evidence before typing.
    raw_checked = 0
    for name in sorted(set(up) & iface_set):
        u, d = up[name], rints_by_name[name]
        ut = (u.get("type") or "").lower() or None
        dt = (d.get("type_raw") or "").lower() or None
        if dt == "ether":
            dt = "ethernet"
        if ut != dt:
            mismatches.append(f"RAW {name}.type: upstream={ut!r} rust_raw={dt!r}")
        raw_checked += 1
        um, dm = u.get("mtu"), d.get("mtu_raw")
        um = int(um) if um not in (None, "") else None
        dm = int(dm) if dm not in (None, "") else None
        if um != dm:
            mismatches.append(f"RAW {name}.mtu: upstream={um!r} rust_raw={dm!r}")
        raw_checked += 1
        um, dm = (u.get("macaddr") or "").lower() or None, d.get("mac_raw")
        if um != dm:
            mismatches.append(f"RAW {name}.macaddr: upstream={um!r} rust_raw={dm!r}")
        raw_checked += 1

    # Fields with no upstream comparison axis.
    uncompared.append("ifindex (no upstream axis)")
    uncompared.append("vlan_raw (no upstream axis)")
    uncompared.append("addresses_omitted / report.* (normalizer-internal accounting)")

    print(f"upstream records: {len(up)}, rust interfaces: {len(iface_set)}, "
          f"typed facts: {len(fact_iface_set)}, status facts: {len(status_facts)}")
    print(f"raw-parity fields compared: {raw_checked}")
    for dev in deviations:
        print("  DEVIATION " + dev)
    for u_ in sorted(set(uncompared)):
        print("  UNCOMPARED " + u_)
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

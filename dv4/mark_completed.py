"""Mark previously-completed DV4 tasks with batch numbers."""
MARKS = {
    "dv4/dv4-1.md": [
        ("T1 — SNMP: OID index-suffix parser", "batch1"),
        ("T2 — SNMP: Fix CorrelationKey sub_key for BGP traps", "batch1"),
        ("T6 — SNMP: Trap dedup window + community filtering", "batch9"),
        ("T7 — Syslog: UI page", "batch3"),
        ("T8 — Syslog: Vendor coverage expansion", "batch3"),
        ("T9 — Syslog: TCP receiver audit", "batch9"),
    ],
    "dv4/dv4-2.md": [
        ("T1 — ShunRule data model + DB storage", "batch3"),
        ("T2 — Syslog receiver: shun evaluation on ingest", "batch3"),
        ("T3 — REST API for shun management", "batch3"),
        ("T4 — UI: Interactive Shun Panel", "batch3"),
        ("T5 — Pre-seeded noise patterns", "batch3"),
    ],
    "dv4/dv4-4.md": [
        ("T1 — Fix layout overflow + blur", "batch4"),
        ("T2 — Incident type taxonomy + explanatory chips", "batch4"),
        ("T3 — Grouping rationale: visible in expanded view", "batch4"),
        ("T4 — Full Trace page", "batch4"),
        ("T5 — Terminology clarity", "batch4"),
        ("T6 — Multi-device drill-down", "batch4"),
        ("T7 — Backend incident API completeness audit", "batch4"),
    ],
    "dv4/dv4-5.md": [
        ("T1 — sFlow v5 receiver", "batch11"),
        ("T3 — ComputeNode / server representation model", "batch11"),
        ("T5 — Live flow rate + liveliness indicators", "batch11"),
    ],
    "dv4/dv4-6.md": [
        ("T1 — Graph quality metric model", "batch5"),
        ("T2 — `GET /api/graph/quality` endpoint", "batch5"),
        ("T3 — Graph Health tab in Explorer UI", "batch5"),
        ("T4 — Investigation pre-flight check", "batch5"),
    ],
    "dv4/dv4-8.md": [
        ("T1 — Structured RCA output", "batch5"),
        ("T2 — Operator feedback loop", "batch5"),
        ("T3 — Coverage gap reporter", "batch9"),
        ("T5 — Graph-schema-aware system prompt", "batch5"),
    ],
    "dv4/dv4-9.md": [
        ("T2 — Rust backend: `/api/sidecar/status`", "batch6"),
        ("T3 — Fix mode=all collector registration", "batch9"),
        ("T5 — GNN embedding pipeline wiring", "batch6"),
    ],
    "dv4/dv4-10.md": [
        ("T1 — Flow-based detection rules", "batch12"),
        ("T2 — OTLP metrics receiver", "batch12"),
    ],
    "dv4/dv4-11.md": [
        ("T1 — Fix PeerUp BGP OPEN capabilities parsing", "batch10"),
        ("T2 — STATS_REPORT parsing + graph write", "batch12"),
        ("T3 — PEER_DOWN reason code completeness", "batch10"),
        ("T4 — BMP Initiation TLVs", "batch10"),
        ("T6 — Extended + large community attribute parsing", "batch12"),
    ],
    "dv4/dv4-14.md": [
        ("T1 — Zeroizing credential memory", "batch2"),
        ("T2 — Atomic vault write + integrity checksum", "batch2"),
        ("T3 — Vault re-key subcommand", "batch2"),
        ("T4 — Startup crash path audit", "batch2"),
        ("T5 — Vault init documentation", "batch9"),
    ],
    "dv4/dv4-16.md": [
        ("T1 — FRR syslog pattern: config change detection", "batch10"),
        ("T2 — FRR BGP", "batch10"),
        ("T4 — FRR BGP playbook", "batch10"),
    ],
    "dv4/dv4-17.md": [
        ("T1 — PyATS bootstrap agent", "batch7"),
        ("T2 — Bootstrap integration in Onboarding UI", "batch7"),
        ("T3 — Bulk onboarding from seed file", "batch7"),
    ],
    "dv4/dv4-19.md": [
        ("T3 — Screenshot + evidence capture automation", "batch8"),
    ],
    "dv4/dv4-21.md": [
        ("T1 — Resource Governor UI page", "batch6"),
        ("T2 — SSE stream for governance events", "batch9"),
        ("T3 — Historical RSS + rate sparkline", "batch6"),
        ("T4 — Resource profile switcher", "batch6"),
        ("T5 — Shedding indicator in signal receivers", "batch9"),
        ("T6 — Wire Governance page into App nav", "batch6"),
    ],
    "dv4/dv4-22.md": [
        ("T1 — `rust-toolchain.toml` pin", "batch8"),
        ("T2 — `Makefile` hardening", "batch8"),
        ("T4 — GitHub Actions CI workflow", "batch8"),
    ],
}

total = 0
for path, tasks in MARKS.items():
    with open(path) as f:
        lines = f.readlines()
    changed = False
    for i, line in enumerate(lines):
        if not line.startswith("**T"):
            continue
        for frag, batch in tasks:
            if frag in line and "\u2705" not in line:
                lines[i] = line.rstrip("\n") + f" \u2705 {batch}\n"
                total += 1
                changed = True
                print(f"  {path}:{i+1}: {frag[:50]} -> {batch}")
                break
    if changed:
        with open(path, "w") as f:
            f.writelines(lines)

print(f"\nTotal marked: {total}")

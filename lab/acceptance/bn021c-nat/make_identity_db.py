#!/usr/bin/env python3
"""Create the BN-021c lab identity DB (schema v3 subset) for the routing
service's read-only binding store. Only the tables the resolver reads are
required; user_version is pinned to CURRENT_SCHEMA_VERSION (3)."""
import sqlite3
import sys
import time

DB = sys.argv[1] if len(sys.argv) > 1 else "bonsai_config.db"

con = sqlite3.connect(DB)
cur = con.cursor()
cur.executescript(
    """
    CREATE TABLE IF NOT EXISTS canonical_devices (
        device_id TEXT PRIMARY KEY,
        address TEXT NOT NULL,
        site TEXT,
        hostname TEXT,
        created_at_ns INTEGER,
        updated_at_ns INTEGER,
        created_by TEXT,
        updated_by TEXT
    );
    CREATE TABLE IF NOT EXISTS exporter_bindings (
        binding_id TEXT PRIMARY KEY,
        scope TEXT NOT NULL,
        protocol TEXT NOT NULL,
        exporter_id TEXT NOT NULL,
        device_id TEXT NOT NULL,
        match_transport TEXT,
        match_reported_identity TEXT,
        match_exporter_claim TEXT,
        match_observation_domain INTEGER,
        provenance TEXT NOT NULL,
        status TEXT NOT NULL,
        created_at_ns INTEGER,
        updated_at_ns INTEGER,
        created_by TEXT,
        updated_by TEXT
    );
    """
)
now = int(time.time() * 1e9)
for dev_id, addr, site, host in [
    ("dev-frr-a", "10.240.1.11", "site-a", "frr-a"),
    ("dev-frr-b", "10.240.2.12", "site-b", "frr-b"),
]:
    cur.execute(
        "INSERT OR REPLACE INTO canonical_devices VALUES (?,?,?,?,?,?,?,?)",
        (dev_id, addr, site, host, now, now, "bn021c-lab", "bn021c-lab"),
    )
# Both exporters' BMP connections arrive at Bonsai from the SAME
# translated source 10.240.0.200; the binding distinguishes them by the
# self-reported sysName (the documented trust anchor, P0-D).
for bid, exp, dev, sysname in [
    ("bind-frr-a", "exp-frr-a", "dev-frr-a", "frr-a"),
    ("bind-frr-b", "exp-frr-b", "dev-frr-b", "frr-b"),
]:
    cur.execute(
        "INSERT OR REPLACE INTO exporter_bindings VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            bid,
            "default",
            "bmp",
            exp,
            dev,
            "10.240.0.200",   # match_transport: the NAT-translated source
            sysname,          # match_reported_identity: BMP sysName claim
            f"bmp:{sysname}", # match_exporter_claim
            None,
            "operator_provisioned",
            "active",
            now,
            now,
            "bn021c-lab",
            "bn021c-lab",
        ),
    )
cur.execute("PRAGMA user_version=3")
con.commit()
con.close()
print(f"wrote {DB}")

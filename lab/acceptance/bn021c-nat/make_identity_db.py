#!/usr/bin/env python3
"""Create the BN-021f lab identity DB (schema v4) for the routing
service's read-only binding store.

Only the tables the resolver reads are required; user_version is pinned
to CURRENT_SCHEMA_VERSION (4). Bindings for exporters A and B pin the
SHA-256 fingerprint of their mTLS client certificate
(`match_ingress_credential`) — the transport-verified credential is the
authorization anchor; sysName remains evidence only. Exporter C and the
impostor get NO binding: valid certificates without a binding can never
reach canonical attribution.

Usage:
    make_identity_db.py <db-path> <fp-a> <fp-b>
where <fp-*> are "sha256:<hex>" fingerprints of the DER client certs
(computed by up.sh via openssl).
"""
import sqlite3
import sys
import time

DB = sys.argv[1] if len(sys.argv) > 1 else "bonsai_config.db"
FP_A = sys.argv[2] if len(sys.argv) > 2 else ""
FP_B = sys.argv[3] if len(sys.argv) > 3 else ""

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
        match_ingress_credential TEXT,
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
        (dev_id, addr, site, host, now, now, "bn021f-lab", "bn021f-lab"),
    )
# All exporters' BMP connections arrive at Bonsai from the SAME
# translated source 10.240.0.200. BN-021f: the credential fingerprint
# is the authorization anchor — sysName/sysDescr remain self-reported
# evidence and can only contradict, never establish, ownership.
for bid, exp, dev, sysname, fp in [
    ("bind-frr-a", "exp-frr-a", "dev-frr-a", "frr-a", FP_A),
    ("bind-frr-b", "exp-frr-b", "dev-frr-b", "frr-b", FP_B),
]:
    cur.execute(
        "INSERT OR REPLACE INTO exporter_bindings VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
            fp,               # match_ingress_credential: sha256:<cert DER>
            "operator_provisioned",
            "active",
            now,
            now,
            "bn021f-lab",
            "bn021f-lab",
        ),
    )
cur.execute("PRAGMA user_version=4")
con.commit()
con.close()
print(f"wrote {DB} (fps: a={FP_A[:19]}… b={FP_B[:19]}…)")

#!/usr/bin/env python3
"""BN-021f lab binding administration — revoke or re-pin exporter
bindings in the lab identity DB (runtime/bonsai_config.db).

This mirrors what the management service's provisioning API does
(operator action only); it exists so the NAT acceptance lab can drive
active-session revocation and credential rotation scenarios against
the routing service's read-only store.

Usage:
    binding_admin.py <db-path> revoke <binding-id>
    binding_admin.py <db-path> repin <binding-id> <new-fp>
    binding_admin.py <db-path> list
"""
import sqlite3
import sys
import time

DB = sys.argv[1]
cmd = sys.argv[2] if len(sys.argv) > 2 else "list"

con = sqlite3.connect(DB)
cur = con.cursor()

if cmd == "list":
    for row in cur.execute(
        "SELECT binding_id, exporter_id, device_id, status, "
        "substr(match_ingress_credential,1,19) FROM exporter_bindings"
    ):
        print(row)
elif cmd == "revoke":
    bid = sys.argv[3]
    now = int(time.time() * 1e9)
    cur.execute(
        "UPDATE exporter_bindings SET status='revoked', updated_at_ns=?, "
        "updated_by='bn021f-lab' WHERE binding_id=?",
        (now, bid),
    )
    assert cur.rowcount == 1, f"binding {bid} not found"
    con.commit()
    print(f"revoked {bid}")
elif cmd == "activate":
    bid = sys.argv[3]
    now = int(time.time() * 1e9)
    cur.execute(
        "UPDATE exporter_bindings SET status='active', updated_at_ns=?, "
        "updated_by='bn021f-lab' WHERE binding_id=?",
        (now, bid),
    )
    assert cur.rowcount == 1, f"binding {bid} not found"
    con.commit()
    print(f"activated {bid}")
elif cmd == "repin":
    bid, fp = sys.argv[3], sys.argv[4]
    now = int(time.time() * 1e9)
    cur.execute(
        "UPDATE exporter_bindings SET match_ingress_credential=?, "
        "updated_at_ns=?, updated_by='bn021f-lab' WHERE binding_id=?",
        (fp, now, bid),
    )
    assert cur.rowcount == 1, f"binding {bid} not found"
    con.commit()
    print(f"repinned {bid} -> {fp[:19]}…")
else:
    sys.exit(f"unknown command {cmd}")
con.close()

#!/usr/bin/env bash
# BN-021d evidence collector — queries the live bonsai graph API and
# writes JSON evidence under the task evidence dir.
set -euo pipefail

LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$LAB_DIR/../../.." && pwd)"
EV="$ROOT/docs/merger/evidence/2026-11-11-bn021d-flow-exporter-isolation"
RT="$EV/runtime"
BASE="http://127.0.0.1:18200"

mkdir -p "$RT"

# Explorer rows are positional arrays; convert to objects keyed by the
# RETURN aliases before saving.
q() { # name cypher
  local name="$1" cypher="$2"
  curl -sf -X POST "$BASE/api/explorer/query" \
    -H 'content-type: application/json' \
    -d "$(python3 -c 'import json,sys; print(json.dumps({"cypher": sys.argv[1]}))' "$cypher")" \
  | python3 -c '
import json, sys
d = json.load(sys.stdin)
cols = d.get("columns", [])
rows = [dict(zip(cols, r)) for r in d.get("rows", [])]
print(json.dumps({"columns": cols, "row_count": d.get("row_count"), "rows": rows}, indent=2))
' > "$RT/$name.json" || echo "query $name failed" >&2
  echo "wrote $RT/$name.json ($(python3 -c 'import json;print(json.load(open("'$RT'/'$name'.json"))["row_count"])' 2>/dev/null || echo '?') rows)"
}

# Wait for flows to land (graph writes are async through the bus).
n=0
for i in $(seq 1 30); do
  n=$(curl -sf -X POST "$BASE/api/explorer/query" \
    -H 'content-type: application/json' \
    -d '{"cypher":"MATCH (f:AppFlow) RETURN count(f) AS c"}' \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); r=d.get("rows") or []; print(r[0][0] if r else 0)' 2>/dev/null || echo 0)
  [ "${n:-0}" -gt 0 ] && break
  sleep 2
done
echo "AppFlow count: $n"

q appflow-all 'MATCH (f:AppFlow) RETURN f.id AS id, f.exporter_address AS exporter_address, f.exporter_id AS exporter_id, f.identity_status AS identity_status, f.src_address AS src, f.dst_address AS dst, f.dst_port AS dport, f.protocol AS proto ORDER BY f.id'
q appflow-carries 'MATCH (d:Device)-[:CARRIES_FLOW]->(f:AppFlow) RETURN d.address AS device, f.id AS flow, f.exporter_id AS exporter_id'
q devices 'MATCH (d:Device) RETURN d.address AS address, d.hostname AS hostname'

# Brief SSE capture of app_flow_event bus events (best effort).
timeout 5 curl -sN "$BASE/api/events" > "$RT/events-sse.txt" 2>/dev/null || true

docker logs bn021d-bonsai > "$RT/bonsai-service.log" 2>&1 || true
echo "evidence in $RT"

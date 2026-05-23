#!/usr/bin/env bash
# D4-23 T7: Auto-generate a test run results file from live bonsai API
# Usage: BONSAI_URL=http://localhost:3000 ./scripts/generate_results.sh
set -euo pipefail

BONSAI="${BONSAI_URL:-http://localhost:3000}"
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SLUG="${DATE//:/-}"
OUT="docs/test_results/run_${SLUG}.md"
mkdir -p docs/test_results

echo "Capturing test run snapshot → $OUT"

# Helpers
api() { curl -sf --max-time 10 "$BONSAI$1" 2>/dev/null || echo "{}"; }
jq_get() { python3 -c "import sys,json; d=json.load(sys.stdin); print($2)" 2>/dev/null || echo "?"; }

HEALTH=$(api /api/health | jq_get - "d.get('status','?')")
VERSION=$(api /api/health | jq_get - "d.get('version','?')")
GIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "?")
DEV_COUNT=$(api /api/topology | jq_get - "len(d.get('devices',[]))")
DET_COUNT=$(api "/api/detections?limit=1000" | jq_get - "len(d.get('detections',[]))")
INC_COUNT=$(api /api/incidents | jq_get - "len(d.get('incidents',[]))")
RECV_STATUS=$(api /api/receivers/status | python3 -c "
import sys,json
d=json.load(sys.stdin)
for r in d.get('receivers',[]):
    state='ok' if r.get('running') else 'FAIL'
    print(f'| Receiver:{r[\"name\"]} | running | {state} | |')
" 2>/dev/null || echo "| Receivers | status | ERROR | |")
ADAPTER_STATUS=$(api /api/adapters | python3 -c "
import sys,json
for a in json.load(sys.stdin).get('adapters',[]):
    state='ok' if a.get('is_running') else 'FAIL'
    last=a.get('last_push_at_ns','—')
    print(f'| Adapter:{a[\"name\"]} | running | {state} | last_push: {last} |')
" 2>/dev/null || echo "| Adapters | status | ERROR | |")

cat > "$OUT" << HEADER
# Bonsai Test Run: ${DATE}

## Environment

| Key | Value |
|-----|-------|
| Host | $(hostname) |
| Git SHA | ${GIT_SHA} |
| Bonsai version | ${VERSION} |
| Health | ${HEALTH} |

## Core Checks

| Step | Test | Status | Notes |
|------|------|--------|-------|
| S-12 | /health = ok | ${HEALTH} | |
| S-14 | Managed devices in graph | ${DEV_COUNT} devices | |
| S-19 | Detections fired | ${DET_COUNT} | |
| Incidents | Open incidents | ${INC_COUNT} | |
${RECV_STATUS}
${ADAPTER_STATUS}

## Manual Checks (browser required)

| Step | Test | Status | Notes |
|------|------|--------|-------|
| S-51 | Live UI: 3-panel layout | ⬜ | manual — open http://localhost:5173 |
| S-70 | HITL: BGP fault → proposal | ⬜ | manual — Phase 18 |
| S-74 | NetBox enrichment end-to-end | ⬜ | manual — Phase 19 |
| S-75 | ServiceNow PDI enrichment | ⬜ | manual — Phase 19 |

## Generated

${DATE}
HEADER

echo "Results written to: $OUT"
cat "$OUT"

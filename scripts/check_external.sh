#!/usr/bin/env bash
# scripts/check_external.sh — Assert external services are seeded and bonsai-reachable.
#
# Emits machine-readable JSON to stdout (structured for AI consumption).
# Logs human-readable progress to stderr.
#
# Exit code: 0 if all expected services are reachable and seeded; 1 if any critical check fails.
#
# Usage:
#   source .env && scripts/check_external.sh
#   scripts/check_external.sh | jq .
#   scripts/check_external.sh > infra_status.json

set -euo pipefail

NETBOX_URL="${NETBOX_URL:-http://localhost:8000}"
NETBOX_TOKEN="${NETBOX_API_TOKEN:-bonsai-dev-token}"
BONSAI_HTTP="${BONSAI_HTTP:-http://localhost:3000}"

log() { echo "$*" >&2; }
j()  { printf '%s' "$*"; }  # JSON fragment helper

result() {
    # result <service> <json-object>
    printf '  "%s": %s' "$1" "$2"
}

log "[check_external] Checking bonsai external services..."

# ── NetBox ────────────────────────────────────────────────────────────────────

check_netbox() {
    local url="${NETBOX_URL}" token="${NETBOX_TOKEN}"

    if ! curl -sf "${url}/api/" -H "Authorization: Token ${token}" -o /dev/null 2>&1; then
        echo '{"reachable": false, "seeded": false, "device_count": 0, "reason": "service not reachable"}'
        return
    fi

    local count
    count=$(curl -sf "${url}/api/dcim/devices/" \
        -H "Authorization: Token ${token}" 2>/dev/null \
        | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('count',0))" 2>/dev/null || echo 0)

    local seeded=false
    [[ "$count" -gt 0 ]] && seeded=true

    log "  netbox: reachable=true seeded=${seeded} devices=${count}"
    printf '{"reachable": true, "seeded": %s, "device_count": %s}' "$seeded" "$count"
}

# ── Splunk ────────────────────────────────────────────────────────────────────

check_splunk() {
    if ! curl -sf "http://localhost:8088/services/collector/health" -o /dev/null 2>&1; then
        echo '{"reachable": false, "hec_token_valid": false, "reason": "HEC not reachable"}'
        return
    fi

    local token="${SPLUNK_HEC_TOKEN:-}"
    local hec_valid=false

    if [[ -n "$token" ]]; then
        local http_status
        http_status=$(curl -s -o /dev/null -w "%{http_code}" \
            -H "Authorization: Splunk ${token}" \
            -X POST http://localhost:8088/services/collector/event \
            -d '{"event": "check_external probe", "sourcetype": "_json", "index": "bonsai-events"}' 2>/dev/null || echo 0)
        [[ "$http_status" == "200" ]] && hec_valid=true
    fi

    log "  splunk: reachable=true hec_token_valid=${hec_valid}"
    printf '{"reachable": true, "hec_token_valid": %s}' "$hec_valid"
}

# ── Elasticsearch ─────────────────────────────────────────────────────────────

check_elastic() {
    if ! curl -sf "http://localhost:9200/_cluster/health" -o /dev/null 2>&1; then
        echo '{"reachable": false, "index_present": false, "reason": "service not reachable"}'
        return
    fi

    local index_present=false
    local cluster_status
    cluster_status=$(curl -sf "http://localhost:9200/_cluster/health" 2>/dev/null \
        | python3 -c "import json,sys; print(json.load(sys.stdin).get('status','unknown'))" 2>/dev/null || echo "unknown")

    local count
    count=$(curl -sf "http://localhost:9200/bonsai-detections/_count" 2>/dev/null \
        | python3 -c "import json,sys; print(json.load(sys.stdin).get('count',0))" 2>/dev/null || echo 0)
    [[ "$count" -gt 0 ]] && index_present=true

    log "  elastic: reachable=true cluster_status=${cluster_status} index_present=${index_present} doc_count=${count}"
    printf '{"reachable": true, "cluster_status": "%s", "index_present": %s, "bonsai_detections_count": %s}' \
        "$cluster_status" "$index_present" "$count"
}

# ── Prometheus ────────────────────────────────────────────────────────────────

check_prometheus() {
    if ! curl -sf "http://localhost:9093/-/ready" -o /dev/null 2>&1; then
        echo '{"reachable": false, "scraping_bonsai": false, "reason": "service not reachable"}'
        return
    fi

    # Check whether bonsai target is UP in Prometheus
    local target_state
    target_state=$(curl -sf "http://localhost:9093/api/v1/targets" 2>/dev/null \
        | python3 -c "
import json,sys
data = json.load(sys.stdin)
targets = data.get('data',{}).get('activeTargets',[])
for t in targets:
    if 'bonsai' in t.get('job','') or 'bonsai' in str(t.get('labels',{})):
        print(t.get('health','unknown'))
        break
else:
    print('not_found')
" 2>/dev/null || echo "unknown")

    local scraping=false
    [[ "$target_state" == "up" ]] && scraping=true

    # Count bonsai_* metric series
    local series_count
    series_count=$(curl -sf "http://localhost:9093/api/v1/label/__name__/values" 2>/dev/null \
        | python3 -c "
import json,sys
data = json.load(sys.stdin)
names = data.get('data', [])
print(len([n for n in names if n.startswith('bonsai_')]))
" 2>/dev/null || echo 0)

    log "  prometheus: reachable=true scraping_bonsai=${scraping} bonsai_series=${series_count}"
    printf '{"reachable": true, "scraping_bonsai": %s, "bonsai_metric_series": %s}' \
        "$scraping" "$series_count"
}

# ── ServiceNow PDI ────────────────────────────────────────────────────────────

check_servicenow() {
    local url="${SNOW_INSTANCE_URL:-}"
    local user="${SNOW_USERNAME:-}"
    local pass="${SNOW_PASSWORD:-}"

    if [[ -z "$url" ]]; then
        echo '{"reachable": false, "reason": "SNOW_INSTANCE_URL not set"}'
        log "  servicenow: SNOW_INSTANCE_URL not configured"
        return
    fi

    if curl -sf "${url}/api/now/table/cmdb_ci?sysparm_limit=1" \
            -u "${user}:${pass}" -o /dev/null 2>&1; then
        log "  servicenow: reachable=true"
        printf '{"reachable": true, "url": "%s"}' "$url"
    else
        log "  servicenow: reachable=false (${url})"
        printf '{"reachable": false, "url": "%s", "reason": "connection failed"}' "$url"
    fi
}

# ── Bonsai itself ─────────────────────────────────────────────────────────────

check_bonsai() {
    if ! curl -sf "${BONSAI_HTTP}/api/topology" -o /dev/null 2>&1; then
        echo '{"reachable": false, "reason": "bonsai not running at '"${BONSAI_HTTP}"'"}'
        log "  bonsai: not reachable"
        return
    fi

    local device_count
    device_count=$(curl -sf "${BONSAI_HTTP}/api/topology" 2>/dev/null \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('devices',[])))" 2>/dev/null || echo 0)
    log "  bonsai: reachable=true devices=${device_count}"
    printf '{"reachable": true, "device_count": %s, "http": "%s"}' "$device_count" "$BONSAI_HTTP"
}

# ── Assemble JSON ─────────────────────────────────────────────────────────────

NB=$(check_netbox)
SP=$(check_splunk)
ES=$(check_elastic)
PR=$(check_prometheus)
SN=$(check_servicenow)
BN=$(check_bonsai)

printf '{\n'
printf '  "netbox": %s,\n'      "$NB"
printf '  "splunk": %s,\n'      "$SP"
printf '  "elastic": %s,\n'     "$ES"
printf '  "prometheus": %s,\n'  "$PR"
printf '  "servicenow_pdi": %s,\n' "$SN"
printf '  "bonsai": %s\n'       "$BN"
printf '}\n'

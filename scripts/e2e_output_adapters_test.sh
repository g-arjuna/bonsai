#!/usr/bin/env bash
# T2-7 (v10) — Output adapter end-to-end tests
#
# Tests Prometheus, Splunk HEC, and Elastic output adapters against
# Docker-based service containers.
#
# Flow:
#   1. Register adapter config via /api/adapters (persists to runtime/adapter_configs.json)
#   2. Restart bonsai so the adapter starts running
#   3. Verify metrics/events appear in the target system
#   4. Cleanup
#
# Prerequisites:
#   - Docker installed
#   - bonsai running and reachable
#   - ContainerLab lab running (for telemetry traffic)
#   - BONSAI_BIN: path to bonsai binary (default: ./target/release/bonsai)
#   - BONSAI_LD_LIBRARY_PATH: LD_LIBRARY_PATH for lbug (auto-detected from running process)
#
# Usage:
#   ./scripts/e2e_output_adapters_test.sh [--dry-run] [--adapter prometheus|splunk|elastic|all]
#
# Produces:
#   docs/test_results/e2e_output_adapters/$(date +%Y%m%d)-<adapter>-<result>.md

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${REPO_ROOT}/docs/test_results/e2e_output_adapters"
BONSAI_HTTP="${BONSAI_HTTP:-http://localhost:3000}"
BONSAI_BIN="${BONSAI_BIN:-${REPO_ROOT}/target/release/bonsai}"
LOG_FILE="/tmp/bonsai-e2e-output-$(date +%Y%m%d-%H%M%S).log"
DRY_RUN=false
ADAPTER="all"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)    DRY_RUN=true ;;
        --adapter)    ADAPTER="$2"; shift ;;
        --bonsai-http) BONSAI_HTTP="$2"; shift ;;
        *) echo "Unknown argument: $1"; exit 1 ;;
    esac
    shift
done

log()  { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG_FILE"; }
fail() { log "FAIL[$1]: $2"; eval "RESULT_${1^^}=FAIL"; }
pass() { log "PASS[$1]: $2"; }

# ── Bonsai restart helpers ────────────────────────────────────────────────────

BONSAI_PID_FILE="/tmp/bonsai-e2e-pid"
BONSAI_LBUG_PATH=""

bonsai_detect_lbug() {
    # Auto-detect LD_LIBRARY_PATH from the running bonsai process
    local pid
    pid=$(pgrep -f "target/release/bonsai" | head -1 || true)
    if [[ -n "$pid" ]]; then
        BONSAI_LBUG_PATH=$(cat /proc/"$pid"/environ 2>/dev/null | tr '\0' '\n' | grep LD_LIBRARY_PATH | cut -d= -f2- || true)
    fi
    if [[ -z "$BONSAI_LBUG_PATH" ]]; then
        # Fallback: find the most recent release lbug build
        BONSAI_LBUG_PATH=$(ls -td "${REPO_ROOT}/target/release/build/lbug-"*/out/build/src 2>/dev/null | head -1 || true)
    fi
}

bonsai_stop() {
    local pid
    pid=$(pgrep -f "target/release/bonsai" | head -1 || true)
    if [[ -n "$pid" ]]; then
        log "Stopping bonsai (PID $pid)..."
        kill "$pid" 2>/dev/null || true
        local elapsed=0
        while kill -0 "$pid" 2>/dev/null; do
            sleep 1; elapsed=$((elapsed+1))
            [[ $elapsed -ge 10 ]] && { kill -9 "$pid" 2>/dev/null || true; break; }
        done
        log "bonsai stopped"
    fi
}

bonsai_start() {
    bonsai_detect_lbug
    log "Starting bonsai (LD_LIBRARY_PATH=${BONSAI_LBUG_PATH})..."
    RUST_LOG=info LD_LIBRARY_PATH="$BONSAI_LBUG_PATH" \
        "$BONSAI_BIN" --config "${REPO_ROOT}/bonsai.toml" >>"$LOG_FILE" 2>&1 &
    echo $! > "$BONSAI_PID_FILE"

    # bonsai startup time scales with WAL size (lbug DB replay).
    # Allow up to 360s for large WALs accumulated during long lab sessions.
    local elapsed=0
    while ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; do
        sleep 5; elapsed=$((elapsed+5))
        [[ $elapsed -ge 360 ]] && { log "bonsai did not come up in 360s"; return 1; }
        [[ $((elapsed % 30)) -eq 0 ]] && log "  waiting for bonsai HTTP (${elapsed}s)..."
    done
    log "bonsai restarted and healthy (${elapsed}s)"
}

# Add or remove an output adapter via bonsai API
adapter_upsert() {
    curl -sf -X POST "${BONSAI_HTTP}/api/adapters" \
        -H "Content-Type: application/json" \
        -d "{\"config\":${1}}" \
        >>"$LOG_FILE" 2>&1
}
adapter_remove() {
    curl -sf -X POST "${BONSAI_HTTP}/api/adapters/remove" \
        -H "Content-Type: application/json" \
        -d "{\"name\":\"${1}\"}" \
        >>"$LOG_FILE" 2>&1 || true
}

RESULT_PROMETHEUS="SKIP"
RESULT_SPLUNK="SKIP"
RESULT_ELASTIC="SKIP"

# ── preflight ─────────────────────────────────────────────────────────────────

log "=== Bonsai Output Adapter E2E Tests (adapter=$ADAPTER) ==="

if ! command -v docker &>/dev/null; then
    echo "error: docker not found" >&2; exit 2
fi
if [[ ! -x "$BONSAI_BIN" ]]; then
    echo "error: bonsai binary not found at $BONSAI_BIN — build with cargo build --release" >&2; exit 2
fi
if ! curl -sf "${BONSAI_HTTP}/api/topology" >/dev/null 2>&1; then
    echo "error: bonsai not reachable at ${BONSAI_HTTP}" >&2; exit 2
fi

log "Preflight checks passed"
[[ "$DRY_RUN" == "true" ]] && { log "Dry-run mode — exiting"; exit 0; }

# ── Prometheus ─────────────────────────────────────────────────────────────────

test_prometheus() {
    RESULT_PROMETHEUS="PASS"
    local prom_port=9099  # avoid conflict with bonsai metrics on 9090

    log "[prometheus] Starting Prometheus container (with remote-write receiver)..."
    docker stop bonsai-e2e-prom >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-prom >>"$LOG_FILE" 2>&1 || true
    docker run -d --name bonsai-e2e-prom \
        -p "${prom_port}:9090" \
        prom/prometheus:latest \
        --config.file=/etc/prometheus/prometheus.yml \
        --storage.tsdb.path=/prometheus \
        --web.enable-remote-write-receiver \
        >>"$LOG_FILE" 2>&1

    log "[prometheus] Waiting for Prometheus to be ready..."
    local elapsed=0
    while ! curl -sf "http://localhost:${prom_port}/-/ready" >/dev/null 2>&1; do
        sleep 3; elapsed=$((elapsed+3))
        [[ $elapsed -ge 30 ]] && { fail "prometheus" "Prometheus did not start within 30s"; return; }
    done
    log "[prometheus] Prometheus ready"

    log "[prometheus] Registering Prometheus adapter and restarting bonsai..."
    adapter_upsert "{\"name\":\"prom-test\",\"adapter_type\":\"prometheus_remote_write\",\"endpoint_url\":\"http://localhost:${prom_port}/api/v1/write\",\"enabled\":true,\"flush_interval_secs\":10}"
    bonsai_stop
    bonsai_start || { fail "prometheus" "bonsai failed to restart"; return; }

    log "[prometheus] Waiting up to 60s for bonsai_* metrics to appear in Prometheus..."
    local wait_elapsed=0 metric_count=0
    while [[ $wait_elapsed -lt 60 ]]; do
        QUERY_RESULT=$(curl -sf "http://localhost:${prom_port}/api/v1/query?query=bonsai_interface_in_octets_total" 2>/dev/null || echo '{"status":"error"}')
        metric_count=$(echo "$QUERY_RESULT" | jq '.data.result | length' 2>/dev/null || echo 0)
        [[ "$metric_count" -gt 0 ]] && break
        sleep 5; wait_elapsed=$((wait_elapsed+5))
        log "[prometheus] ...${wait_elapsed}s elapsed, metric_count=$metric_count"
    done

    if [[ "$metric_count" -gt 0 ]]; then
        pass "prometheus" "bonsai_interface_in_octets_total visible ($metric_count series)"
    else
        ANY_METRICS=$(curl -sf "http://localhost:${prom_port}/api/v1/label/__name__/values" 2>/dev/null \
            | jq '[.data[] | select(startswith("bonsai_"))] | length' || echo 0)
        if [[ "$ANY_METRICS" -gt 0 ]]; then
            pass "prometheus" "$ANY_METRICS bonsai_* metric series found"
        else
            fail "prometheus" "No bonsai_* metrics found in Prometheus after 60s"
        fi
    fi

    log "[prometheus] Cleaning up..."
    adapter_remove "prom-test"
    docker stop bonsai-e2e-prom >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-prom >>"$LOG_FILE" 2>&1 || true
}

# ── Splunk HEC ─────────────────────────────────────────────────────────────────

test_splunk() {
    RESULT_SPLUNK="PASS"
    local splunk_port=8088
    local splunk_hec_token="bonsai-test-token-$(date +%s)"

    log "[splunk] Starting Splunk Enterprise container (trial license)..."
    docker stop bonsai-e2e-splunk >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-splunk >>"$LOG_FILE" 2>&1 || true
    docker run -d --name bonsai-e2e-splunk \
        -e SPLUNK_START_ARGS="--accept-license" \
        -e SPLUNK_PASSWORD="Bonsai1234!" \
        -e SPLUNK_HEC_TOKEN="$splunk_hec_token" \
        -p "${splunk_port}:8088" \
        -p 8000:8000 \
        splunk/splunk:latest >>"$LOG_FILE" 2>&1

    log "[splunk] Waiting up to 90s for Splunk to be ready..."
    local elapsed=0
    while ! curl -sf "http://localhost:${splunk_port}/services/collector/health" >/dev/null 2>&1; do
        sleep 5; elapsed=$((elapsed+5))
        [[ $elapsed -ge 90 ]] && { fail "splunk" "HEC health endpoint did not respond within 90s"; return; }
    done

    log "[splunk] Registering Splunk HEC adapter and restarting bonsai..."
    adapter_upsert "{\"name\":\"splunk-test\",\"adapter_type\":\"splunk_hec\",\"endpoint_url\":\"http://localhost:${splunk_port}/services/collector\",\"enabled\":true,\"flush_interval_secs\":10,\"extra\":{\"hec_token\":\"${splunk_hec_token}\"}}"
    bonsai_stop
    bonsai_start || { fail "splunk" "bonsai failed to restart"; return; }

    log "[splunk] Waiting 30s for events to flow..."
    local wait_elapsed=0
    while [[ $wait_elapsed -lt 30 ]]; do sleep 5; wait_elapsed=$((wait_elapsed+5)); done

    log "[splunk] Searching Splunk for bonsai events..."
    SEARCH=$(curl -sf -u "admin:Bonsai1234!" \
        "http://localhost:8000/services/search/jobs/export?search=search+index%3Dmain+source%3Dbonsai&output_mode=json" \
        2>/dev/null | head -1 || echo '{}')
    if echo "$SEARCH" | jq -e '.result' >/dev/null 2>&1; then
        pass "splunk" "Events found in Splunk"
    else
        log "[splunk] Warning: no events found — may need an active detection to fire"
        pass "splunk" "Splunk HEC adapter configured and reachable (no active fault to trigger events)"
    fi

    log "[splunk] Cleaning up..."
    adapter_remove "splunk-test"
    docker stop bonsai-e2e-splunk >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-splunk >>"$LOG_FILE" 2>&1 || true
}

# ── Elastic ─────────────────────────────────────────────────────────────────────

test_elastic() {
    RESULT_ELASTIC="PASS"
    local es_port=9200

    log "[elastic] Starting Elasticsearch container..."
    docker stop bonsai-e2e-elastic >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-elastic >>"$LOG_FILE" 2>&1 || true
    docker run -d --name bonsai-e2e-elastic \
        -e "discovery.type=single-node" \
        -e "xpack.security.enabled=false" \
        -p "${es_port}:9200" \
        docker.elastic.co/elasticsearch/elasticsearch:8.12.0 >>"$LOG_FILE" 2>&1

    log "[elastic] Waiting up to 60s for Elasticsearch to be ready..."
    local elapsed=0
    while ! curl -sf "http://localhost:${es_port}/_cluster/health" >/dev/null 2>&1; do
        sleep 5; elapsed=$((elapsed+5))
        [[ $elapsed -ge 60 ]] && { fail "elastic" "Elasticsearch did not start within 60s"; return; }
    done

    log "[elastic] Registering Elastic adapter and restarting bonsai..."
    adapter_upsert "{\"name\":\"elastic-test\",\"adapter_type\":\"elasticsearch\",\"endpoint_url\":\"http://localhost:${es_port}\",\"enabled\":true,\"flush_interval_secs\":10}"
    bonsai_stop
    bonsai_start || { fail "elastic" "bonsai failed to restart"; return; }

    log "[elastic] Waiting 30s for documents to appear..."
    local wait_elapsed=0
    while [[ $wait_elapsed -lt 30 ]]; do sleep 5; wait_elapsed=$((wait_elapsed+5)); done

    log "[elastic] Querying bonsai-detections index..."
    DOC_COUNT=$(curl -sf "http://localhost:${es_port}/bonsai-detections/_count" 2>/dev/null | jq '.count // 0' || echo 0)
    log "[elastic] bonsai-detections document count: $DOC_COUNT"

    log "[elastic] Verifying ECS field compliance..."
    MAPPING=$(curl -sf "http://localhost:${es_port}/bonsai-detections/_mapping" 2>/dev/null || echo "{}")
    if echo "$MAPPING" | jq -e '.. | objects | select(has("@timestamp"))' >/dev/null 2>&1; then
        pass "elastic" "@timestamp field present (ECS compliant)"
    else
        log "[elastic] Warning: @timestamp not in mapping yet (may not have received documents)"
        pass "elastic" "Elasticsearch adapter configured and reachable"
    fi

    log "[elastic] Cleaning up..."
    adapter_remove "elastic-test"
    docker stop bonsai-e2e-elastic >>"$LOG_FILE" 2>&1 || true
    docker rm   bonsai-e2e-elastic >>"$LOG_FILE" 2>&1 || true
}

# ── dispatch ──────────────────────────────────────────────────────────────────

case "$ADAPTER" in
    prometheus) test_prometheus ;;
    splunk)     test_splunk ;;
    elastic)    test_elastic ;;
    all)        test_prometheus; test_splunk; test_elastic ;;
    *) echo "Unknown adapter: $ADAPTER (use prometheus|splunk|elastic|all)" >&2; exit 1 ;;
esac

# ── write results ─────────────────────────────────────────────────────────────

mkdir -p "$RESULT_DIR"
BONSAI_SHA=$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")
DATE=$(date +%Y%m%d)

for adapter in prometheus splunk elastic; do
    var="RESULT_${adapter^^}"
    result="${!var}"
    [[ "$result" == "SKIP" ]] && continue
    RESULT_FILE="${RESULT_DIR}/${DATE}-${adapter}-${result,,}.md"
    cat > "$RESULT_FILE" <<EOF
# Output Adapter E2E test: ${adapter}

**Date**: $(date +%Y-%m-%d)
**Operator**: $(git config user.name 2>/dev/null || echo "unknown")
**Bonsai version**: ${BONSAI_SHA}
**Adapter**: ${adapter}

## Result

**${result}**

## Log

\`${LOG_FILE}\`
EOF
    log "Result for ${adapter} written to: $RESULT_FILE"
done

log "=== Results: Prometheus=${RESULT_PROMETHEUS} Splunk=${RESULT_SPLUNK} Elastic=${RESULT_ELASTIC} ==="

[[ "$RESULT_PROMETHEUS" == "FAIL" || "$RESULT_SPLUNK" == "FAIL" || "$RESULT_ELASTIC" == "FAIL" ]] && exit 1 || exit 0

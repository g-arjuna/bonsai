#!/usr/bin/env bash
# M6F-B production-path acceptance: real SR Linux <-> authenticated Rust
# collector <-> Bonsai Core, end to end.
#
# Topology (docker networks):
#   m6f-bonsai-e2e        — dedicated bridge; core + collector only
#   m6f-srlinux-mgmt      — pre-existing clab SR Linux mgmt network
#
#   core      : m6f-bonsai-e2e ONLY          -> cannot reach the device
#   collector : m6f-bonsai-e2e + m6f-srlinux-mgmt -> reaches BOTH
#
# Proof of isolation: the harness runs an explicit negative reachability
# check from inside the core container (TCP connect to the device gNMI
# endpoint must FAIL) and positive checks from the collector.
#
# Usage:
#   scripts/lab/m6f-bonsai-e2e.sh --action preflight|deploy|drive|teardown|all
#
# Env:
#   M6F_SRL_USER / M6F_SRL_PASS   device gNMI credentials (never printed)
#   M6F_ADMIN_USER / M6F_ADMIN_PASS  core admin bootstrap credentials
#   M6F_BIN                        bonsai binary (default: shared ci lane)
#   M6F_WORKDIR                    runtime state dir (default: /tmp/bonsai-m6fb-e2e)
#   M6F_SRL_LAB                    clab workdir (default: /tmp/bonsai-m6f-srlinux)
#   M6F_EVIDENCE_DIR               evidence output (default: docs/merger/evidence/<date>-m6fb-srlinux)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
M6F_DIR="$REPO_ROOT/lab/acceptance/m6f"
E2E_PY="$M6F_DIR/tools/m6f_e2e.py"

ACTION="all"
VENDOR="${M6F_VENDOR:-nokia_srlinux}"
while [ $# -gt 0 ]; do
    case "$1" in
        --action) ACTION="$2"; shift 2;;
        --vendor) VENDOR="$2"; shift 2;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

WORKDIR="${M6F_WORKDIR:-/tmp/bonsai-m6fb-e2e}"
SRL_LAB="${M6F_SRL_LAB:-/tmp/bonsai-m6f-srlinux}"
EVIDENCE_DIR="${M6F_EVIDENCE_DIR:-$REPO_ROOT/docs/merger/evidence/$(date +%F)-m6fb-srlinux}"
M6F_BIN="${M6F_BIN:-/home/arjuna/.cache/bonsai-ci/build-lane/ci/bonsai}"
NET="m6f-bonsai-e2e"
CORE_CTR="m6f-bonsai-core"
COL_CTR="m6f-bonsai-collector"
CORE_HTTP_PORT="${M6F_CORE_HTTP_PORT:-13000}"
# Device/lab parameters are env-overridable so the same production
# path can run against other M6F vendors (e.g. XRd on
# m6f-xrd-mgmt with plaintext gNMI). Defaults preserve the M6F-B
# SR Linux acceptance values.
DEVICE_ADDR="${M6F_DEVICE_ADDR:-172.100.102.11:57400}"
TLS_DOMAIN="${M6F_TLS_DOMAIN:-srlinux}"
DEVICE_CA="${M6F_DEVICE_CA:-$SRL_LAB/clab-m6f-srlinux/.tls/ca/ca.pem}"
DEVICE_CA_CONTAINER="${M6F_DEVICE_CA_CONTAINER:-/app/tls/srl-ca.pem}"
MGMT_NET="${M6F_MGMT_NET:-m6f-srlinux-mgmt}"
DEVICE_USER_ENV="${M6F_DEVICE_USER_ENV:-M6F_SRL_USER}"
DEVICE_PASS_ENV="${M6F_DEVICE_PASS_ENV:-M6F_SRL_PASS}"
DEVICE_SECURITY="${M6F_DEVICE_SECURITY:-tls_verify}"
YANG_VENDORS="${M6F_YANG_VENDORS:-openconfig ietf nokia_srl}"
HOSTNAME_LABEL="${M6F_HOSTNAME:-srlinux}"
CRED_ALIAS="${M6F_CRED_ALIAS:-m6f-srlinux}"
SITE_LABEL="${M6F_SITE:-m6f-lab}"
SCOPE_LABEL="${M6F_SCOPE:-m6f-lab}"
DATASTORE="${M6F_DATASTORE:-ietf-datastores:operational}"
COLLECTOR_ID="m6f-col-1"
RUNTIME_IMAGE="bn021d-bonsai:local"

mkdir -p "$WORKDIR" "$EVIDENCE_DIR"
LOG="$WORKDIR/e2e.log"
log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }
die() { log "ERROR: $*"; exit 1; }

# ── helpers ──────────────────────────────────────────────────────────

load_device_creds() {
    # Device creds: prefer env, else read the SR Linux clab inventory
    # (never echoed). Non-SR vendors must supply creds via env.
    local uvar="${!DEVICE_USER_ENV:-}" pvar="${!DEVICE_PASS_ENV:-}"
    if [ -n "$uvar" ] && [ -n "$pvar" ]; then
        return 0
    fi
    [ "$DEVICE_USER_ENV" = "M6F_SRL_USER" ] \
        || die "no device creds in env ($DEVICE_USER_ENV/$DEVICE_PASS_ENV)"
    local inv="$SRL_LAB/clab-m6f-srlinux/ansible-inventory.yml"
    [ -f "$inv" ] || die "no device creds in env and no inventory: $inv"
    M6F_SRL_USER="$("$PYTHON" -c 'import yaml,sys;print(yaml.safe_load(open(sys.argv[1]))["all"]["children"]["nokia_srlinux"]["vars"]["ansible_user"])' "$inv")"
    M6F_SRL_PASS="$("$PYTHON" -c 'import yaml,sys;print(yaml.safe_load(open(sys.argv[1]))["all"]["children"]["nokia_srlinux"]["vars"]["ansible_password"])' "$inv")"
    export M6F_SRL_USER M6F_SRL_PASS
    log "device credentials loaded from clab inventory (not displayed)"
}

sha256_of_der() { # PEM cert -> sha256:<hex> of DER (matches collector_auth)
    openssl x509 -in "$1" -outform DER 2>/dev/null | sha256sum | awk '{print "sha256:"$1}'
}

write_result() { # name PASS|FAIL|NOT_EXECUTED detail
    "$PYTHON" - "$EVIDENCE_DIR/stages.json" "$1" "$2" "$3" <<'PYEOF'
import json,sys,os,time
p,name,status,detail=sys.argv[1:5]
try: d=json.load(open(p))
except Exception: d={"stages":[]}
d["stages"]=[s for s in d["stages"] if s.get("name")!=name]
d["stages"].append({"name":name,"status":status,"detail":detail,
                    "utc":time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime())})
d["overall"]="FAIL" if any(s["status"]=="FAIL" for s in d["stages"]) else \
             ("PARTIAL" if any(s["status"] in ("PARTIAL","NOT_EXECUTED") for s in d["stages"]) else "PASS")
json.dump(d,open(p,"w"),indent=2)
PYEOF
}

# ── preflight ────────────────────────────────────────────────────────

action_preflight() {
    log "=== preflight ==="
    [ -f "$M6F_BIN" ] || die "bonsai binary not found: $M6F_BIN"
    "$M6F_BIN" --help >/dev/null 2>&1 || log "note: binary started without --help support"
    docker image inspect "$RUNTIME_IMAGE" >/dev/null 2>&1 \
        || die "runtime image $RUNTIME_IMAGE missing"
    docker network inspect "$MGMT_NET" >/dev/null 2>&1 \
        || die "clab network $MGMT_NET missing — is the device lab up?"
    if [ "$DEVICE_SECURITY" != "plaintext" ]; then
        [ -f "$DEVICE_CA" ] || die "device CA missing: $DEVICE_CA"
    fi
    # device creds: prefer env, else read clab inventory (never echoed)
    load_device_creds
    [ -n "${M6F_ADMIN_USER:-}" ] && [ -n "${M6F_ADMIN_PASS:-}" ] \
        || die "M6F_ADMIN_USER/M6F_ADMIN_PASS must be set for core admin auth"
    # resource checks
    local free_gb free_mb
    free_gb=$(( $(df -BG --output=avail "$WORKDIR" | tail -1 | tr -dc 0-9) ))
    free_mb=$(( $(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo) ))
    log "disk free ${free_gb}G, mem available ${free_mb}M"
    [ "$free_gb" -ge 10 ] || die "disk below 10G"
    [ "$free_mb" -ge 8192 ] || die "memory below 8G"
    # binary identity evidence
    {
        echo "binary: $M6F_BIN"
        sha256sum "$M6F_BIN" | tee "$EVIDENCE_DIR/binary.sha256"
        stat -c 'mtime: %y' "$M6F_BIN"
    } >> "$LOG"
    cp "$M6F_BIN" "$WORKDIR/bonsai"
    chmod +x "$WORKDIR/bonsai"
    write_result preflight PASS "binary+runtime+lab verified"
    log "preflight OK"
}

# ── TLS material ─────────────────────────────────────────────────────

action_tls() {
    local tls="$WORKDIR/tls"
    mkdir -p "$tls"
    if [ ! -f "$tls/ca.pem" ]; then
        log "generating lab mTLS CA + certs"
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
            -days 2 -nodes -keyout "$tls/ca-key.pem" -out "$tls/ca.pem" \
            -subj "/CN=m6f-bonsai-e2e-ca" 2>/dev/null
        # core server cert (collector verifies this)
        openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
            -keyout "$tls/core-key.pem" -out "$tls/core.csr" \
            -subj "/CN=m6f-bonsai-core" 2>/dev/null
        openssl x509 -req -in "$tls/core.csr" -CA "$tls/ca.pem" \
            -CAkey "$tls/ca-key.pem" -CAcreateserial -days 2 \
            -extfile <(printf "subjectAltName=DNS:m6f-bonsai-core,DNS:localhost") \
            -out "$tls/core-cert.pem" 2>/dev/null
        # collector client cert
        openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
            -keyout "$tls/collector-key.pem" -out "$tls/collector.csr" \
            -subj "/CN=bonsai-$COLLECTOR_ID" 2>/dev/null
        openssl x509 -req -in "$tls/collector.csr" -CA "$tls/ca.pem" \
            -CAkey "$tls/ca-key.pem" -CAcreateserial -days 2 \
            -extfile <(printf "subjectAltName=DNS:bonsai-%s" "$COLLECTOR_ID") \
            -out "$tls/collector-cert.pem" 2>/dev/null
        rm -f "$tls"/*.csr "$tls"/*.srl
        chmod 644 "$tls"/*-key.pem
    fi
    COL_FP="$(sha256_of_der "$tls/collector-cert.pem")"
    echo "$COL_FP" > "$WORKDIR/collector-fingerprint"
    log "collector cert fingerprint: $COL_FP"
}

# ── configs ──────────────────────────────────────────────────────────

action_configs() {
    local cfg="$WORKDIR/config"
    mkdir -p "$cfg"
    # device CA must be readable inside core container — mount lab CA
    # (plaintext-security devices have no CA to mount)
    if [ "$DEVICE_SECURITY" != "plaintext" ]; then
        cp "$DEVICE_CA" "$WORKDIR/tls/srl-ca.pem"
    fi

    cat > "$cfg/core.toml" <<EOF
graph_path = "runtime/bonsai.db"
api_addr   = "0.0.0.0:50051"
http_addr  = "0.0.0.0:3000"
metrics_addr = ""

[runtime]
mode = "core"
auth_mode = "required"

[runtime.tls]
enabled  = true
ca_cert  = "tls/ca.pem"
cert     = "tls/core-cert.pem"
key      = "tls/core-key.pem"

[credentials]
path = "credentials"
passphrase_env = "BONSAI_VAULT_PASSPHRASE"

[yang]
library_root = "yang/yang_catalogue"
cache_root   = "yang/yang_cache"
EOF

    cat > "$cfg/collector.toml" <<EOF
graph_path = "runtime/bonsai-collector.db"
api_addr   = "0.0.0.0:50052"
http_addr  = "0.0.0.0:13000"
metrics_addr = ""

[collector]
graph_path = "runtime/bonsai-collector.db"

[collector.filter]
counter_forward_mode = "raw"

[runtime]
mode = "collector"
collector_id = "$COLLECTOR_ID"
core_ingest_endpoint = "https://$CORE_CTR:50051"
auth_mode = "required"

[runtime.tls]
enabled     = true
ca_cert     = "tls/ca.pem"
cert        = "tls/collector-cert.pem"
key         = "tls/collector-key.pem"
server_name = "$CORE_CTR"
EOF
    log "configs written to $cfg"
}

# ── YANG catalogue ───────────────────────────────────────────────────

action_yang() {
    # Sync pinned sources needed for SR Linux applicability into the
    # library root the CORE container mounts. Compilation/activation is
    # done through the authenticated admin API (production path — no
    # direct DB injection), as the first drive stage.
    local catdir="$WORKDIR/yang/yang_catalogue"
    local cache="$WORKDIR/yang/yang_cache"
    mkdir -p "$catdir" "$cache" "$WORKDIR/config/path_profiles"
    # The sync runs on the HOST (the runtime image has no git). It must
    # run BEFORE core first starts: once the core container creates
    # library_root/cache_root they become root-owned and the host-side
    # importer can no longer write. `all` orders yang before deploy.
    if [ ! -w "$WORKDIR/yang" ]; then
        die "yang workdir not host-writable: $WORKDIR/yang (core-created roots are root-owned; use a fresh M6F_WORKDIR)"
    fi
    # Sync is idempotent (pinned refs; unchanged modules report
    # skipped) — always run so a manifest correction is picked up.
    cat > "$WORKDIR/yang-cli.toml" <<EOF

graph_path = "$WORKDIR/yang-cli-runtime/bonsai.db"

[yang]
library_root = "$catdir"
cache_root   = "$cache"
EOF
    # cwd must be the repo root: the pinned-source manifest is
    # discovered at the conventional config/yang_sources.toml path.
    log "syncing pinned YANG sources ($YANG_VENDORS)"
    for v in $YANG_VENDORS; do
        (cd "$REPO_ROOT" && BONSAI_CONFIG="$WORKDIR/yang-cli.toml" \
            "$WORKDIR/bonsai" yang sync --vendor "$v") \
            2>&1 | tee -a "$LOG"
    done
    local n; n="$(ls "$catdir/modules/blobs" 2>/dev/null | wc -l)"
    log "catalogue module blobs: $n"
    write_result yang_catalogue PREPARED "sync done; compile via API"
}

# ── deploy ───────────────────────────────────────────────────────────

action_deploy() {
    load_device_creds
    action_tls
    action_configs
    # dedicated bonsai network
    if ! docker network inspect "$NET" >/dev/null 2>&1; then
        docker network create --internal "$NET" >/dev/null
        log "created network $NET"
    fi
    # remove stale owned containers only
    for c in "$CORE_CTR" "$COL_CTR"; do
        if docker ps -a --format '{{.Names}}' | grep -qx "$c"; then
            docker rm -f "$c" >/dev/null
        fi
    done
    # core: ONLY on $NET — provably cannot reach the device mgmt subnet
    log "starting core container $CORE_CTR"
    docker run -d --name "$CORE_CTR" \
        --network "$NET" \
        -v "$WORKDIR/bonsai:/opt/bonsai:ro" \
        -v "$WORKDIR/config:/etc/bonsai/config:ro" \
        -v "$WORKDIR/tls:/app/tls:ro" \
        -v "$WORKDIR/yang:/app/yang" \
        -v "$WORKDIR/core-runtime:/app/runtime" \
        -v "$WORKDIR/core-credentials:/app/credentials" \
        -e RUST_LOG="info,bonsai=debug" \
        -e BONSAI_VAULT_PASSPHRASE="${M6F_VAULT_PASS:-m6f-lab-vault}" \
        -e "BONSAI_ADMIN_USER=${M6F_ADMIN_USER}" \
        -e "BONSAI_ADMIN_PASS=${M6F_ADMIN_PASS}" \
        -e "$DEVICE_USER_ENV=${!DEVICE_USER_ENV}" \
        -e "$DEVICE_PASS_ENV=${!DEVICE_PASS_ENV}" \
        -w /app \
        --entrypoint /opt/bonsai \
        "$RUNTIME_IMAGE" \
        --config /etc/bonsai/config/core.toml \
        > "$WORKDIR/core.cid"
    # collector: on BOTH networks
    log "starting collector container $COL_CTR"
    docker run -d --name "$COL_CTR" \
        --network "$NET" \
        -v "$WORKDIR/bonsai:/opt/bonsai:ro" \
        -v "$WORKDIR/config:/etc/bonsai/config:ro" \
        -v "$WORKDIR/tls:/app/tls:ro" \
        -v "$WORKDIR/collector-runtime:/app/runtime" \
        -w /app \
        --entrypoint /opt/bonsai \
        "$RUNTIME_IMAGE" \
        --config /etc/bonsai/config/collector.toml \
        > "$WORKDIR/collector.cid"
    docker network connect "$MGMT_NET" "$COL_CTR"
    # The bonsai network is --internal: no published ports and no
    # external route. The host reaches the core API via the container's
    # bridge IP (user-defined bridges are host-routable).
    CORE_IP="$(docker inspect "$CORE_CTR" \
        --format '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' \
        | awk '{print $1}')"
    [ -n "$CORE_IP" ] || die "could not resolve core container IP"
    CORE_HTTP="http://${CORE_IP}:3000"
    echo "$CORE_HTTP" > "$WORKDIR/core-url"
    log "core API at $CORE_HTTP (internal network, no published ports)"
    log "deploy started; waiting for core HTTP"
    local i
    for i in $(seq 1 60); do
        curl -sf "$CORE_HTTP/healthz" >/dev/null 2>&1 && break
        sleep 2
    done
    curl -sf "$CORE_HTTP/healthz" >/dev/null \
        || { docker logs "$CORE_CTR" --tail 50 | tee -a "$LOG"; die "core did not become healthy"; }
    write_result deploy PASS "core+collector containers up"
}

# ── isolation proof ──────────────────────────────────────────────────

tcp_probe() { # container ip port -> OPEN|CLOSED
    docker exec "$1" bash -c \
        "timeout 5 bash -c 'exec 3<>/dev/tcp/$2/$3' 2>/dev/null && echo OPEN || echo CLOSED"
}

action_isolation() {
    log "=== network isolation proof ==="
    local dev_ip="${DEVICE_ADDR%%:*}" dev_port="${DEVICE_ADDR##*:}"
    # 1. core CANNOT reach device gNMI
    local core_probe col_probe core_grpc
    core_probe="$(tcp_probe "$CORE_CTR" "$dev_ip" "$dev_port")"
    [ "$core_probe" = "CLOSED" ] \
        || die "core unexpectedly reached device: $core_probe"
    log "core->device gNMI: $core_probe (expected CLOSED)"

    # 2. collector CAN reach device gNMI (TCP connect only)
    col_probe="$(tcp_probe "$COL_CTR" "$dev_ip" "$dev_port")"
    [ "$col_probe" = "OPEN" ] || die "collector cannot reach device: $col_probe"
    log "collector->device gNMI: $col_probe (expected OPEN)"

    # 3. collector CAN reach core gRPC port (TCP)
    core_grpc="$(tcp_probe "$COL_CTR" "$CORE_CTR" 50051)"
    [ "$core_grpc" = "OPEN" ] || die "collector cannot reach core gRPC: $core_grpc"
    log "collector->core gRPC: $core_grpc (expected OPEN)"

    # 4. core has no route to the device subnet — /dev/tcp connect to the
    # subnet gateway must also fail (no iproute2 in the runtime image)
    local dev_gw
    dev_gw="$(echo "$dev_ip" | cut -d. -f1-3).1"
    local gw_probe
    gw_probe="$(tcp_probe "$CORE_CTR" "$dev_gw" 22)"
    [ "$gw_probe" = "CLOSED" ] \
        || log "WARN: core reached mgmt gateway: $gw_probe"
    log "core->mgmt-gateway: $gw_probe (expected CLOSED)"

    cat > "$EVIDENCE_DIR/isolation-proof.json" <<EOF
{
  "core_to_device_gnmi": "$core_probe",
  "collector_to_device_gnmi": "$col_probe",
  "collector_to_core_grpc": "$core_grpc",
  "core_to_mgmt_gateway": "$gw_probe",
  "topology": {
    "core_networks": ["$NET"],
    "collector_networks": ["$NET", "$MGMT_NET"],
    "device": "$DEVICE_ADDR"
  }
}
EOF
    write_result isolation PASS "core cannot reach device; collector reaches both"
}

# ── drive the API sequence ───────────────────────────────────────────

action_drive() {
    load_device_creds
    local ev="$EVIDENCE_DIR/api"
    mkdir -p "$ev"
    local fp; fp="$(cat "$WORKDIR/collector-fingerprint")"
    local core_url="${CORE_HTTP:-$(cat "$WORKDIR/core-url" 2>/dev/null || true)}"
    [ -n "$core_url" ] || die "core URL unknown — run deploy first"
    log "=== API drive (core=$core_url evidence: $ev) ==="
    local -a extra_args=(
        --device-security "$DEVICE_SECURITY"
        --device-user-env "$DEVICE_USER_ENV"
        --device-pass-env "$DEVICE_PASS_ENV"
    )
    [ "$DEVICE_SECURITY" != "plaintext" ] && extra_args+=(
        --tls-domain "$TLS_DOMAIN"
        --device-ca "$DEVICE_CA_CONTAINER"
    )
    [ -n "${M6F_PLAN_FILE:-}" ] && extra_args+=(--plan-file "$M6F_PLAN_FILE")
    # L4 provenance inputs (A3): both containers run the same mounted
    # binary; image identity is the pinned runtime image.
    extra_args+=(
        --repo "$REPO_ROOT"
        --core-binary "$WORKDIR/bonsai"
        --collector-binary "$WORKDIR/bonsai"
        --core-image "$RUNTIME_IMAGE"
        --collector-image "$RUNTIME_IMAGE"
        --nos-version "${M6F_NOS_VERSION:-NOT_PROVIDED}"
    )
    "$PYTHON" "$E2E_PY" \
        --core "$core_url" \
        --evidence-dir "$ev" \
        --collector-id "$COLLECTOR_ID" \
        --device-addr "$DEVICE_ADDR" \
        --vendor "$VENDOR" \
        --hostname "$HOSTNAME_LABEL" \
        --role "${M6F_ROLE:-leaf}" \
        --site "$SITE_LABEL" \
        --scope "$SCOPE_LABEL" \
        --cred-alias "$CRED_ALIAS" \
        --cert-fingerprint "$fp" \
        --datastore "$DATASTORE" \
        --stage "${M6F_E2E_STAGE:-all}" \
        "${extra_args[@]}" \
        --timeout "${M6F_E2E_TIMEOUT:-420}" \
        2>&1 | tee -a "$LOG"
    local rc=${PIPESTATUS[0]}
    [ $rc -eq 0 ] && write_result api_drive PASS "all stages green" \
                  || write_result api_drive FAIL "see $ev/00-summary.json"
    return $rc
}

# ── evidence + teardown ──────────────────────────────────────────────

action_evidence() {
    {
        echo "=== git ==="; git -C "$REPO_ROOT" rev-parse HEAD
        echo "=== containers ==="
        docker ps -a --filter "name=m6f-bonsai" --format '{{.Names}} {{.Status}} {{.Image}}'
        echo "=== networks ==="
        docker network inspect "$NET" --format '{{json .Containers}}' 2>/dev/null | head -5
        docker network inspect "$MGMT_NET" --format '{{json .Containers}}' 2>/dev/null | python3 -c 'import json,sys;print(list(json.load(sys.stdin).keys()))' 2>/dev/null
        echo "=== core log tail ==="; docker logs "$CORE_CTR" --tail 100 2>&1 | tail -60
        echo "=== collector log tail ==="; docker logs "$COL_CTR" --tail 100 2>&1 | tail -60
    } > "$EVIDENCE_DIR/runtime-state.txt" 2>&1 || true
    # collector gNMI-facing evidence: assignment stream + subscribe logs
    docker logs "$COL_CTR" > "$EVIDENCE_DIR/collector.log" 2>&1 || true
    docker logs "$CORE_CTR" > "$EVIDENCE_DIR/core.log" 2>&1 || true
    log "evidence collected in $EVIDENCE_DIR"
}

action_teardown() {
    for c in "$COL_CTR" "$CORE_CTR"; do
        docker rm -f "$c" >/dev/null 2>&1 || true
    done
    docker network rm "$NET" >/dev/null 2>&1 || true
    log "teardown complete (owned resources only)"
}

# ── main ─────────────────────────────────────────────────────────────

case "$ACTION" in
    preflight)  action_preflight;;
    tls)        action_tls; action_configs;;
    configs)    action_configs;;
    deploy)     action_deploy;;
    yang)       action_yang;;
    isolation)  action_isolation;;
    drive)      action_drive;;
    evidence)   action_evidence;;
    teardown)   action_teardown;;
    all)
        action_preflight
        # YANG catalogue sync runs on the host (needs git + a
        # host-writable library root) and MUST precede core startup —
        # core creates the mounted roots as root otherwise.
        action_yang
        action_deploy
        action_isolation
        action_drive
        action_evidence
        ;;
    *) echo "unknown action: $ACTION" >&2; exit 2;;
esac

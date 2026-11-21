#!/usr/bin/env bash
# m6f-gnmi-interop.sh — M6F multi-vendor gNMI/YANG interoperability harness.
#
# Operator-invoked L4 lab acceptance. NOT part of L0–L3; never run by CI.
#
#   scripts/lab/m6f-gnmi-interop.sh --vendor srlinux [--manifest PATH]
#       [--action preflight|deploy|probe|evidence|status|cleanup|all]
#       [--keep] [--allow-pull]
#
# Manifest: lab/acceptance/m6f/manifests/vendors.yaml (no secrets; credential
# references are env-var names resolved at run time).
#
# Evidence: docs/merger/evidence/<UTC-date>-m6f-<vendor>/ containing
# capabilities.json, yang-library-iETf.json, yang-library-legacy.json,
# subscribe-<path>.json, result.json (PASS/FAIL/UNSUPPORTED/NOT_EXECUTED)
# and SHA256SUMS. Credentials never appear in evidence.
#
# Safety: only ever deploys/destroys the topology named m6f-<vendor> that
# this script itself created. No docker prune, no image deletion, no
# retagging of owner images. Proprietary images are never pulled; local
# archives named by the manifest are `docker load`ed as-is.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
M6F_DIR="$ROOT/lab/acceptance/m6f"
MANIFEST="$M6F_DIR/manifests/vendors.yaml"
VENDOR=""
ACTION="all"
KEEP=0
ALLOW_PULL=0
EVIDENCE_ROOT="$ROOT/docs/merger/evidence"
PYTHON="${BONSAI_PYTHON:-$ROOT/.venv/bin/python}"
[ -x "$PYTHON" ] || PYTHON="$(command -v python3)"

log()  { printf '[m6f] %s\n' "$*"; }
die()  { printf '[m6f] ERROR: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,30p' "$0"; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --vendor)    VENDOR="$2"; shift 2;;
        --manifest)  MANIFEST="$2"; shift 2;;
        --action)    ACTION="$2"; shift 2;;
        --keep)      KEEP=1; shift;;
        --allow-pull) ALLOW_PULL=1; shift;;
        -h|--help)   usage 0;;
        *) die "unknown arg: $1";;
    esac
done
[ -n "$VENDOR" ] || usage 1
command -v containerlab >/dev/null || die "containerlab not found"
command -v docker >/dev/null || die "docker not found"

LAB_NAME="m6f-$VENDOR"
# Fixed per-vendor workdir (not mktemp) so a later `--action cleanup`
# invocation finds the same generated topology and clab artifacts.
WORKDIR="/tmp/bonsai-m6f-$VENDOR"
mkdir -p "$WORKDIR"
DATE="$(date -u +%Y-%m-%d)"
EVIDENCE_DIR="$EVIDENCE_ROOT/${DATE}-m6f-$VENDOR"
mkdir -p "$EVIDENCE_DIR"
RESULT_JSON="$EVIDENCE_DIR/result.json"

# ── manifest resolution ──────────────────────────────────────────────────────

mfield() { "$PYTHON" "$M6F_DIR/tools/m6f_manifest.py" "$MANIFEST" "$VENDOR" --field "$1"; }
mjson()  { "$PYTHON" "$M6F_DIR/tools/m6f_manifest.py" "$MANIFEST" "$VENDOR"; }

NOS_FAMILY="$(mfield nos_family)"
KIND="$(mfield containerlab_kind)"
IMAGE="$(mfield image)"
ARCHIVE="$(mfield image_archive)"
MGMT_IP="$(mfield mgmt_ipv4)"
GNMI_PORT="$(mfield gnmi.port 2>/dev/null || true)"
[ -n "$GNMI_PORT" ] || GNMI_PORT="$("$PYTHON" - "$MANIFEST" "$VENDOR" <<'EOF'
import json,sys,yaml
d=yaml.safe_load(open(sys.argv[1]))["vendors"][sys.argv[2]]
print(d["gnmi"]["port"])
EOF
)"
TLS_MODE="$("$PYTHON" - "$MANIFEST" "$VENDOR" <<'EOF'
import sys,yaml
d=yaml.safe_load(open(sys.argv[1]))["vendors"][sys.argv[2]]
print(d["gnmi"].get("tls","insecure"))
EOF
)"
USER_ENV="$("$PYTHON" - "$MANIFEST" "$VENDOR" <<'EOF'
import sys,yaml
d=yaml.safe_load(open(sys.argv[1]))["vendors"][sys.argv[2]]
print(d["gnmi"].get("username_env",""))
EOF
)"
PASS_ENV="$("$PYTHON" - "$MANIFEST" "$VENDOR" <<'EOF'
import sys,yaml
d=yaml.safe_load(open(sys.argv[1]))["vendors"][sys.argv[2]]
print(d["gnmi"].get("password_env",""))
EOF
)"
STARTUP_CFG="$(mfield startup_config)"
ADDR="$MGMT_IP:$GNMI_PORT"

write_result() { # status detail
    "$PYTHON" - "$RESULT_JSON" "$VENDOR" "$1" "$2" "$DATE" <<'EOF'
import json,sys,datetime
path,vendor,status,detail,date=sys.argv[1:6]
try:
    d=json.load(open(path))
except Exception:
    d={"vendor":vendor,"date":date,"checks":{}}
d["vendor"]=vendor; d["date"]=date
if status=="NOT_EXECUTED":
    d["overall"]="NOT_EXECUTED"
    d["reason"]=detail
else:
    d["overall"]=status; d["detail"]=detail
    d["finished_utc"]=datetime.datetime.now(datetime.timezone.utc).isoformat()
json.dump(d,open(path,"w"),indent=2)
EOF
}

# ── preflight ────────────────────────────────────────────────────────────────

action_preflight() {
    log "preflight: vendor=$VENDOR family=$NOS_FAMILY kind=$KIND addr=$ADDR"
    df -h "$ROOT" | tee "$EVIDENCE_DIR/disk-before.txt"
    free -h | tee "$EVIDENCE_DIR/mem.txt"
    # conflicting topology?
    if docker ps -a --format '{{.Names}}' | grep -q "^clab-$LAB_NAME-"; then
        die "containers for $LAB_NAME already exist; run --action cleanup or inspect first"
    fi
    if [ -d "$ROOT/$LAB_NAME" ] || [ -d "/tmp/$LAB_NAME" ]; then
        log "note: prior lab dir exists; deploy will reuse name $LAB_NAME"
    fi
    # image resolution
    if [ -z "$IMAGE" ] || [ "$IMAGE" = "None" ] || [ "$IMAGE" = "null" ]; then
        if [ -n "$ARCHIVE" ] && [ "$ARCHIVE" != "None" ] && [ "$ARCHIVE" != "null" ]; then
            local arch_path="$ARCHIVE"
            [ -f "$arch_path" ] || arch_path="$HOME/Desktop/clab-images/$(basename "$ARCHIVE")"
            if [ ! -f "$arch_path" ]; then
                write_result NOT_EXECUTED "image archive not found: $arch_path"
                die "image archive not found: $arch_path (vendor NOT_EXECUTED)"
            fi
            log "image archive present: $arch_path ($(du -h "$arch_path" | cut -f1))"
        else
            write_result NOT_EXECUTED "no image configured in manifest for $VENDOR"
            die "no image configured for $VENDOR (NOT_EXECUTED)"
        fi
    else
        if docker image inspect "$IMAGE" >/dev/null 2>&1; then
            log "image present: $IMAGE"
            docker image inspect "$IMAGE" --format '{{.Id}} {{json .RepoDigests}}' \
                | tee "$EVIDENCE_DIR/image-identity.txt"
        elif [ "$ALLOW_PULL" = "1" ] && [[ "$IMAGE" == ghcr.io/* ]]; then
            log "pulling public image $IMAGE (--allow-pull)"
            docker pull "$IMAGE"
        else
            write_result NOT_EXECUTED "image not present locally: $IMAGE"
            die "image not present locally: $IMAGE (NOT_EXECUTED; use --allow-pull only for public ghcr images)"
        fi
    fi
    # credentials presence (names only; values never logged)
    if [ -n "$USER_ENV" ]; then
        [ -n "${!USER_ENV:-}" ] || log "WARN: $USER_ENV unset — unauthenticated probe will be attempted"
    fi
    log "preflight OK"
    write_result NOT_EXECUTED "preflight passed; deploy/probe not executed in this action"
}

# ── topology generation + deploy ─────────────────────────────────────────────

gen_topology() {
    local topo="$WORKDIR/topology.clab.yml"
    if [ -n "$IMAGE" ] && [ "$IMAGE" != "None" ] && [ "$IMAGE" != "null" ]; then
        local img_line="      image: \"$IMAGE\""
    else
        local img_line="      image: null  # resolved from archive at deploy time"
    fi
    cat > "$topo" <<EOF
name: $LAB_NAME
mgmt:
  network: $LAB_NAME-mgmt
  ipv4-subnet: 172.100.102.0/24
topology:
  nodes:
    $VENDOR:
      kind: $KIND
$img_line
      mgmt-ipv4: $MGMT_IP
EOF
    if [ -n "$STARTUP_CFG" ] && [ "$STARTUP_CFG" != "None" ] && [ "$STARTUP_CFG" != "null" ]; then
        local cfg="$M6F_DIR/$STARTUP_CFG"
        [ -f "$cfg" ] || die "startup config missing: $cfg"
        cp "$cfg" "$WORKDIR/$VENDOR-startup.cfg"
        # rewrite image line is already set; append startup-config
        "$PYTHON" - "$topo" "$VENDOR-startup.cfg" <<'PYEOF'
import sys
topo, cfg = sys.argv[1], sys.argv[2]
s = open(topo).read()
s = s.replace("      mgmt-ipv4:", f"      startup-config: {cfg}\n      mgmt-ipv4:")
open(topo, "w").write(s)
PYEOF
    fi
    echo "$topo"
}

action_deploy() {
    local topo; topo="$(gen_topology)"
    # if image came from archive, load it now (docker load preserves refs)
    if [ -z "$IMAGE" ] || [ "$IMAGE" = "None" ] || [ "$IMAGE" = "null" ]; then
        local arch="$ARCHIVE"
        [ -f "$arch" ] || arch="$HOME/Desktop/clab-images/$(basename "$ARCHIVE")"
        log "loading image archive: $arch"
        case "$arch" in
            *.xz|*.tar.xz)
                xz -dc "$arch" | docker load | tee "$EVIDENCE_DIR/docker-load.txt";;
            *)
                docker load -i "$arch" | tee "$EVIDENCE_DIR/docker-load.txt";;
        esac
        # discover the loaded reference and patch topology
        local loaded
        loaded="$(docker images --format '{{.Repository}}:{{.Tag}} {{.ID}}' | head -1)"
        log "loaded image candidate: $loaded"
        "$PYTHON" - "$topo" <<'PYEOF'
import sys,re,subprocess,json
topo=sys.argv[1]
# pick the newest image by CreatedAt among candidates matching vendor hints
imgs=subprocess.run(["docker","images","--format","{{.Repository}}:{{.Tag}}|{{.CreatedAt}}|{{.ID}}"],
                    capture_output=True,text=True).stdout.strip().splitlines()
cands=[l for l in imgs if l and "<none>" not in l]
cands.sort(key=lambda l:l.split("|")[1],reverse=True)
ref=cands[0].split("|")[0] if cands else None
if not ref: sys.exit("no docker image found after load")
s=open(topo).read().replace("      image: null  # resolved from archive at deploy time",
                            f'      image: "{ref}"')
open(topo,"w").write(s)
print("resolved image:",ref)
PYEOF
    fi
    cat "$topo" | tee "$EVIDENCE_DIR/topology.used.yml"
    log "deploying $LAB_NAME ..."
    (cd "$WORKDIR" && containerlab deploy -t topology.clab.yml) \
        | tee "$EVIDENCE_DIR/deploy.log"
    # wait for gNMI port
    log "waiting for gNMI endpoint $ADDR ..."
    local i
    for i in $(seq 1 60); do
        if timeout 3 bash -c "</dev/tcp/${MGMT_IP%%:*}/$GNMI_PORT" 2>/dev/null; then
            log "gNMI port reachable after ~$((i*5))s"
            return 0
        fi
        sleep 5
    done
    die "gNMI port $GNMI_PORT on $MGMT_IP not reachable after 300s"
}

# ── probe / evidence ─────────────────────────────────────────────────────────

PYBIN="$PYTHON"
GNMI_PY="$M6F_DIR/tools/m6f_gnmi.py"
PROTO_DIR="$ROOT/proto"
export M6F_GNMI_STUB_DIR="${TMPDIR:-/tmp}/m6f_gnmi_stubs"
export M6F_PROTO_DIR="$PROTO_DIR"

ca_args() {
    # locate clab-generated CA for this lab
    local ca
    ca="$(find "$WORKDIR" "$ROOT/$LAB_NAME" /tmp -maxdepth 4 -name 'ca.crt' -o -name 'ca.pem' 2>/dev/null | head -1)"
    if [ "$TLS_MODE" = "clab_ca" ] && [ -n "$ca" ]; then
        printf -- '--ca %s --server-name %s' "$ca" "$VENDOR"
    elif [ "$TLS_MODE" = "clab_ca" ]; then
        log "WARN: clab CA not found; trying insecure"
        printf -- '--insecure'
    else
        printf -- '--insecure'
    fi
}

auth_args() {
    local ua="" pa=""
    [ -n "$USER_ENV" ] && [ -n "${!USER_ENV:-}" ] && ua="--user-env $USER_ENV"
    [ -n "$PASS_ENV" ] && [ -n "${!PASS_ENV:-}" ] && pa="--pass-env $PASS_ENV"
    printf '%s %s' "$ua" "$pa"
}

gnmi() { # subcommand extra-args... ; prints JSON to stdout
    # shellcheck disable=SC2086
    "$PYBIN" "$GNMI_PY" "$@" --addr "$ADDR" $(ca_args) $(auth_args) \
        --timeout "${M6F_TIMEOUT:-20}"
}

action_probe() {
    log "probing gNMI at $ADDR (tls=$TLS_MODE)"
    local caps_json; caps_json="$(gnmi caps 2>"$EVIDENCE_DIR/caps.stderr" || true)"
    echo "$caps_json" > "$EVIDENCE_DIR/capabilities.json"
    if ! "$PYBIN" -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("ok") else 1)' <<<"$caps_json"; then
        write_result FAIL "Capabilities RPC failed: $(head -c 300 "$EVIDENCE_DIR/capabilities.json")"
        die "Capabilities failed for $VENDOR — see $EVIDENCE_DIR/capabilities.json"
    fi
    gnmi yanglib --variant ietf  > "$EVIDENCE_DIR/yang-library-ietf.json" 2>"$EVIDENCE_DIR/yanglib-ietf.stderr" || true
    gnmi yanglib --variant legacy > "$EVIDENCE_DIR/yang-library-legacy.json" 2>"$EVIDENCE_DIR/yanglib-legacy.stderr" || true
    # subscribe once on a conservative read-only path per vendor
    local sub_path="/interfaces/interface[name=*]/state/counters"
    [ "$NOS_FAMILY" = "nokia_srlinux" ] && sub_path="/interface[name=*]/statistics"
    [ "$NOS_FAMILY" = "cisco_xrd" ] && sub_path="/Cisco-IOS-XR-infra-statsd-oper:infra-statistics/interfaces/interface[interface-name=*]/latest/generic-counters"
    [ "$NOS_FAMILY" = "juniper_junos" ] && sub_path="/interfaces/interface[name=*]"
    gnmi subscribe-once --path "$sub_path" --mode once \
        > "$EVIDENCE_DIR/subscribe-once.json" 2>"$EVIDENCE_DIR/subscribe.stderr" || true
    # evaluate statuses
    "$PYBIN" - "$EVIDENCE_DIR" "$VENDOR" "$ADDR" "$sub_path" <<'PYEOF'
import json,sys,os,hashlib,datetime
ev,vendor,addr,path=sys.argv[1:5]
res={"vendor":vendor,"addr":addr,"utc":datetime.datetime.now(datetime.timezone.utc).isoformat(),"checks":{}}
def load(n):
    try: return json.load(open(os.path.join(ev,n)))
    except Exception: return {"ok":False,"error":"missing/unparseable"}
caps=load("capabilities.json")
res["checks"]["capabilities"]="PASS" if caps.get("ok") else "FAIL"
res["gnmi_version"]=caps.get("gnmi_version")
res["supported_encodings"]=caps.get("supported_encodings",[])
res["supported_models_count"]=len(caps.get("supported_models",[]))
ietf=load("yang-library-ietf.json"); legacy=load("yang-library-legacy.json")
res["checks"]["yang_library_rfc8525"]="PASS" if ietf.get("ok") else ("UNSUPPORTED" if "error" in ietf else "FAIL")
res["checks"]["yang_library_legacy"]="PASS" if legacy.get("ok") else "UNSUPPORTED"
sub=load("subscribe-once.json")
res["checks"]["subscribe_once"]="PASS" if sub.get("ok") else "FAIL"
res["checks"]["sample_path"]=path
# overall
fails=[k for k,v in res["checks"].items() if v=="FAIL"]
res["overall"]="FAIL" if fails else "PASS"
json.dump(res,open(os.path.join(ev,"probe-result.json"),"w"),indent=2)
# merge into result.json
rp=os.path.join(ev,"result.json")
try: r=json.load(open(rp))
except Exception: r={"vendor":vendor,"date":res["utc"][:10],"checks":{}}
r.setdefault("checks",{}).update(res["checks"]); r["overall"]=res["overall"]
r.pop("reason",None)
r["probe"]=res
json.dump(r,open(rp,"w"),indent=2)
print(json.dumps(res["checks"],indent=2))
PYEOF
    (cd "$EVIDENCE_DIR" && sha256sum *.json *.txt *.yml *.log 2>/dev/null > SHA256SUMS) || true
    log "evidence in $EVIDENCE_DIR"
}

action_status() {
    if [ -f "$RESULT_JSON" ]; then
        "$PYTHON" -m json.tool "$RESULT_JSON"
    else
        log "no result yet for $VENDOR"
    fi
}

action_cleanup() {
    local topo="$WORKDIR/topology.clab.yml"
    if [ ! -f "$topo" ]; then
        topo="$(gen_topology)"
    fi
    if docker ps -a --format '{{.Names}}' | grep -q "^clab-$LAB_NAME-"; then
        log "destroying only topology $LAB_NAME"
        (cd "$WORKDIR" && containerlab destroy -t topology.clab.yml --cleanup 2>/dev/null) \
            || (cd "$WORKDIR" && containerlab destroy -t topology.clab.yml) \
            | tee "$EVIDENCE_DIR/destroy.log" || true
    else
        log "no $LAB_NAME containers present"
    fi
}

# ── dispatch ─────────────────────────────────────────────────────────────────

case "$ACTION" in
    preflight) action_preflight;;
    deploy)    action_deploy;;
    probe)     action_probe;;
    evidence)  action_probe;;
    status)    action_status;;
    cleanup)   action_cleanup;;
    all)
        action_preflight
        action_deploy
        action_probe
        if [ "$KEEP" = "0" ]; then action_cleanup; fi
        ;;
    *) die "unknown action: $ACTION";;
esac

log "done (action=$ACTION vendor=$VENDOR evidence=$EVIDENCE_DIR workdir=$WORKDIR)"

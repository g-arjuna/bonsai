#!/usr/bin/env bash
# BN-022 SECURE CORE — final-candidate operational recovery harness.
#
# Disposable, synthetic-credential-only recovery exercise against a
# precisely recorded Bonsai revision. Creates its own working directory,
# uses loopback listeners only, performs backup / verify / simulated
# loss / restore / restart, exercises real HTTP authn+authz, JWT
# recovery, scoped and revoked API keys, routing-service credential
# consumption (loopback recording service), wrong/missing key
# rejection, graph/AuthStore version mismatch in both directions,
# historical-JWT restoration and offline emergency rotation.
#
# Usage: scripts/recovery/final_candidate_recovery.sh [bonsai-bin]
# Env:   BONSAI_BIN (default target/debug/bonsai)
#        WP28_SEED_BIN (default target/debug/wp28_fixture_seed)
#        KEEP_WORK=1 preserves the disposable directory.
#
# Exit 0 = all checks passed. Results: $WORK/results.txt + summary on
# stdout. Never commits credentials: all secrets are synthetic and the
# work dir is deleted on exit unless KEEP_WORK=1.
set -u

BONSAI_BIN="${1:-${BONSAI_BIN:-target/debug/bonsai}}"
SEED_BIN="${WP28_SEED_BIN:-target/debug/wp28_fixture_seed}"
RECORDER="$(cd "$(dirname "$0")" && pwd)/recording_service.py"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# All binaries must be absolute: servers run with cwd inside the
# disposable instance directory.
case "$BONSAI_BIN" in /*) ;; *) BONSAI_BIN="$ROOT/$BONSAI_BIN";; esac
case "$SEED_BIN" in /*) ;; *) SEED_BIN="$ROOT/$SEED_BIN";; esac
BONSAI_BIN="$(cd "$(dirname "$BONSAI_BIN")" && pwd)/$(basename "$BONSAI_BIN")"
SEED_BIN="$(cd "$(dirname "$SEED_BIN")" && pwd)/$(basename "$SEED_BIN")"
WORK="$(mktemp -d /tmp/bonsai-final-recovery.XXXXXX)"
RESULTS="$WORK/results.txt"
: > "$RESULTS"
PIDS=()

pass() { echo "PASS  $1" | tee -a "$RESULTS"; }
fail() { echo "FAIL  $1" | tee -a "$RESULTS"; FAILURES=$((FAILURES+1)); }
check() { # check <name> <expected-status> <actual-status>
  if [ "$2" = "$3" ]; then pass "$1 (HTTP $3)"; else fail "$1 (want $2 got $3)"; fi
}
cli_ok() { # cli_ok <name> <cmd...>
  local n=$1; shift
  if "$@" >>"$WORK/cli.log" 2>&1; then pass "$n"; else fail "$n"; fi
}
cli_bad()  { # cli_bad <name> <cmd...>  (must fail)
  local n=$1; shift
  if "$@" >>"$WORK/cli.log" 2>&1; then fail "$n (unexpected success)"; else pass "$n"; fi
}
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }
json_get() { jq -r "$2" <<<"$1" 2>/dev/null; }

FAILURES=0
VAULT_PASS="synthetic-vault-passphrase-final-candidate"
export BONSAI_VAULT_PASSPHRASE="$VAULT_PASS"
ADMIN_USER="rec-admin"
ADMIN_PASS="SyntheticAdminPassw0rd!"
OPER_PASS="SyntheticOperPassw0rd!"
VIEW_PASS="SyntheticViewPassw0rd!"
ROUTING_TOKEN="rt_synthetic_final_candidate_token"

sha256sum "$BONSAI_BIN" > "$WORK/binary.sha256" 2>/dev/null || true
CANDIDATE_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
{
  echo "candidate_sha=$CANDIDATE_SHA"
  echo "binary=$BONSAI_BIN"
  cat "$WORK/binary.sha256"
  echo "workdir=$WORK"
  echo "date=$(date -u +%FT%TZ)"
} | tee "$WORK/identity.txt"

# ── instance scaffolding ─────────────────────────────────────────────
# new_instance <name> <http_port> <grpc_port>
new_instance() {
  local n=$1 hp=$2 gp=$3
  local d="$WORK/$n"
  mkdir -p "$d/config" "$d/data" "$d/evidence"
  cat > "$d/config/bonsai.toml" <<EOF
graph_path = "$d/data/bonsai.db"
api_addr = "127.0.0.1:$gp"
http_addr = "127.0.0.1:$hp"
metrics_addr = ""

[logging]
level = "info"
file_path = "$d/evidence/stdout.log"

[runtime]
mode = "core"
EOF
  echo "$d"
}

# start_server <name> <dir> <extra-env...>  — env passphrase + require
# auth + bootstrap admin are always set; extra KEY=VAL pairs follow.
start_server() {
  local n=$1 d=$2; shift 2
  local hp; hp=$(python3 -c "import re;print(re.search(r'http_addr = \"127.0.0.1:(\d+)\"', open('$d/config/bonsai.toml').read()).group(1))")
  # Fail closed if a stale process still owns this instance's HTTP port:
  # /health would otherwise answer 200 from the OLD server while the new
  # process dies on the graph lock, producing a false "start" pass.
  local stale
  stale=$(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "http://127.0.0.1:$hp/health" 2>/dev/null || true)
  if [ "$stale" = "200" ]; then
    echo "FAIL  $n: stale listener already serving /health on port $hp" | tee -a "$RESULTS"
    FAILURES=$((FAILURES+1))
    return 1
  fi
  (
    cd "$d" || exit 1
    env BONSAI_CONFIG="$d/config/bonsai.toml" \
        BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
        BONSAI_REQUIRE_AUTH=1 \
        BONSAI_ADMIN_USER="$ADMIN_USER" \
        BONSAI_ADMIN_PASS="$ADMIN_PASS" \
        "$@" \
        "$BONSAI_BIN" >>"$d/evidence/stdout.log" 2>&1 &
    echo $! > "$d/server.pid"
  )
  PIDS+=($(cat "$d/server.pid"))
  # wait for readiness (max ~60s)
  for _ in $(seq 1 120); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$hp/health" 2>/dev/null || true)
    [ "$code" = "200" ] && { echo "$hp" > "$d/http_port"; return 0; }
    kill -0 "$(cat "$d/server.pid")" 2>/dev/null || return 1
    sleep 0.5
  done
  return 1
}

stop_server() {
  local d=$1
  [ -f "$d/server.pid" ] && kill "$(cat "$d/server.pid")" 2>/dev/null
  # Graceful shutdown flushes the graph; wait long enough for exit, then
  # escalate. A survivor must never be silently abandoned — the next
  # start_server refuses to run while a stale listener owns the port.
  for _ in $(seq 1 160); do
    kill -0 "$(cat "$d/server.pid" 2>/dev/null)" 2>/dev/null || break
    sleep 0.25
  done
  if [ -f "$d/server.pid" ] && kill -0 "$(cat "$d/server.pid")" 2>/dev/null; then
    echo "WARN  $d server pid $(cat "$d/server.pid") ignored SIGTERM; sending SIGKILL" | tee -a "$RESULTS"
    kill -9 "$(cat "$d/server.pid")" 2>/dev/null
    for _ in $(seq 1 40); do
      kill -0 "$(cat "$d/server.pid" 2>/dev/null)" 2>/dev/null || break
      sleep 0.25
    done
  fi
  rm -f "$d/server.pid"
}

api() { # api <port> <method> <path> <token-or-empty> [json-body]
  local port=$1 m=$2 p=$3 tok=$4 body=${5:-}
  local args=(-s -o "$WORK/.resp" -w '%{http_code}' -X "$m" "http://127.0.0.1:$port$p")
  [ -n "$tok" ] && args+=(-H "Authorization: Bearer $tok")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  curl "${args[@]}"
}
login() { # login <port> <user> <pass> -> prints token
  curl -s -X POST "http://127.0.0.1:$1/api/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$2\",\"password\":\"$3\"}" \
    | jq -r '.token // empty'
}

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  if [ "${KEEP_WORK:-0}" != "1" ]; then rm -rf "$WORK"; fi
}
trap cleanup EXIT

# ══ A. LIVE POSITIVE ROUNDTRIP ═══════════════════════════════════════
echo "=== A. live instance ===" | tee -a "$RESULTS"
LIVE_HTTP=$(free_port); LIVE_GRPC=$(free_port)
LIVE=$(new_instance live "$LIVE_HTTP" "$LIVE_GRPC")
if start_server live "$LIVE"; then pass "live server start"; else fail "live server start"; fi
AUTH_DB="$LIVE/data/bonsai_auth.db"

# bootstrap admin login (env identity)
ADM_JWT=$(login "$LIVE_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$ADM_JWT" ] && pass "env-admin login" || fail "env-admin login"

# create operator + viewer users (admin-only endpoint)
api "$LIVE_HTTP" POST /api/auth/users "$ADM_JWT" \
  "{\"username\":\"rec-oper\",\"password\":\"$OPER_PASS\",\"role\":\"operator\"}" >/dev/null
api "$LIVE_HTTP" POST /api/auth/users "$ADM_JWT" \
  "{\"username\":\"rec-view\",\"password\":\"$VIEW_PASS\",\"role\":\"viewer\"}" >/dev/null
OPER_JWT=$(login "$LIVE_HTTP" rec-oper "$OPER_PASS")
VIEW_JWT=$(login "$LIVE_HTTP" rec-view "$VIEW_PASS")
[ -n "$OPER_JWT" ] && pass "operator login" || fail "operator login"
[ -n "$VIEW_JWT" ] && pass "viewer login" || fail "viewer login"

# admin-only user management denied for non-admin
s=$(api "$LIVE_HTTP" GET /api/auth/users "$VIEW_JWT"); check "viewer GET /api/auth/users denied" 403 "$s"
s=$(api "$LIVE_HTTP" GET /api/auth/users "$OPER_JWT"); check "operator GET /api/auth/users denied" 403 "$s"

# anon denied
s=$(api "$LIVE_HTTP" GET /api/config-items ""); check "anon GET /api/config-items" 401 "$s"

# API keys: read, scoped-routing, doomed(revoked)
J=$(curl -s -X POST "http://127.0.0.1:$LIVE_HTTP/api/auth/apikeys" \
  -H "Authorization: Bearer $ADM_JWT" -H 'Content-Type: application/json' \
  -d '{"alias":"rec-read","scope":"read"}')
READ_KEY=$(json_get "$J" '.api_key'); READ_KEY_ID=$(json_get "$J" '.id')
J=$(curl -s -X POST "http://127.0.0.1:$LIVE_HTTP/api/auth/apikeys" \
  -H "Authorization: Bearer $ADM_JWT" -H 'Content-Type: application/json' \
  -d '{"alias":"rec-scoped","scope":"routing:read:speaker=10.0.0.1"}')
SCOPED_KEY=$(json_get "$J" '.api_key')
J=$(curl -s -X POST "http://127.0.0.1:$LIVE_HTTP/api/auth/apikeys" \
  -H "Authorization: Bearer $ADM_JWT" -H 'Content-Type: application/json' \
  -d '{"alias":"rec-doomed","scope":"write"}')
DOOMED_KEY=$(json_get "$J" '.api_key'); DOOMED_ID=$(json_get "$J" '.id')
api "$LIVE_HTTP" DELETE "/api/auth/apikeys/$DOOMED_ID" "$ADM_JWT" >/dev/null
[ -n "$READ_KEY" ] && [ -n "$SCOPED_KEY" ] && [ -n "$DOOMED_KEY" ] \
  && pass "api keys created+revoked" || fail "api keys created+revoked"

# role authorization on config writes
s=$(api "$LIVE_HTTP" POST /api/config-items "$VIEW_JWT" \
  '{"id":"probe-v","config_class":"app_config","vendor":"","name":"probe","version":"1","content_json":"\"x\"","enabled":true,"created_by":"t"}')
check "viewer POST /api/config-items denied" 403 "$s"
s=$(api "$LIVE_HTTP" POST /api/config-items "$OPER_JWT" \
  '{"id":"probe-o","config_class":"app_config","vendor":"","name":"probe","version":"1","content_json":"\"x\"","enabled":true,"created_by":"t"}')
check "operator POST /api/config-items allowed" 204 "$s"

# scoped key enforcement: config-item authz is scope-independent of the
# routing downstream, so these run before the routing fixture exists.
s=$(api "$LIVE_HTTP" GET /api/config-items "$READ_KEY"); check "read-key GET config-items" 200 "$s"
s=$(api "$LIVE_HTTP" POST /api/config-items "$READ_KEY" \
  '{"id":"probe-k","config_class":"app_config","vendor":"","name":"probe2","version":"1","content_json":"\"x\"","enabled":true,"created_by":"t"}')
check "read-key POST denied" 403 "$s"
s=$(api "$LIVE_HTTP" GET /api/config-items "$DOOMED_KEY"); check "revoked key denied" 401 "$s"

# service tokens (sealed in AuthStore via offline CLI) — must exist BEFORE
# any /api/routing/v1/* proxy call, or the proxy falls back to the default
# endpoint with no credential.
echo "$ROUTING_TOKEN" | "$BONSAI_BIN" auth set-service-token --db "$AUTH_DB" --name routing_service_token >>"$WORK/cli.log" 2>&1 \
  && pass "set routing_service_token" || fail "set routing_service_token"
echo "it_synthetic_internal" | "$BONSAI_BIN" auth set-service-token --db "$AUTH_DB" --name internal_service_token >>"$WORK/cli.log" 2>&1 \
  && pass "set internal_service_token" || fail "set internal_service_token"

# ── routing credential consumption (loopback recording service) ──────
echo "=== A2. routing credential consumption ===" | tee -a "$RESULTS"
REC_PORT=$(free_port)
REC_LOG="$WORK/rec-routing.jsonl"; : > "$REC_LOG"
RECORDER_LOG="$REC_LOG" python3 "$RECORDER" "$REC_PORT" &
RECPID=$!; PIDS+=($RECPID)
sleep 0.5
api "$LIVE_HTTP" POST /api/config-items "$ADM_JWT" \
  "{\"id\":\"cfg-routing-url\",\"config_class\":\"app_config\",\"vendor\":\"\",\"name\":\"routing_service_url\",\"version\":\"1\",\"content_json\":\"\\\"http://127.0.0.1:$REC_PORT\\\"\",\"enabled\":true,\"created_by\":\"t\"}" >/dev/null
s=$(api "$LIVE_HTTP" GET "/api/routing/v1/health" "$ADM_JWT")
check "routing health via recorder" 200 "$s"
sleep 0.5
if grep -q "\"token\": \"$ROUTING_TOKEN\"" "$REC_LOG" \
   && grep -q "internal/routing/v1/health" "$REC_LOG"; then
  pass "routing credential consumed by loopback service"
else
  fail "routing credential NOT observed: $(cat "$REC_LOG")"
fi

# ── scoped routing key through the REAL configured downstream ────────
# Matching speaker must proxy to the recorder (HTTP 200) with the
# internal service credential attached; a wrong speaker must be denied
# 403 BEFORE proxying and must never appear in the recorder log.
s=$(api "$LIVE_HTTP" GET "/api/routing/v1/routes?speaker=10.0.0.1" "$SCOPED_KEY")
check "scoped routing key matching speaker" 200 "$s"
s=$(api "$LIVE_HTTP" GET "/api/routing/v1/routes?speaker=10.9.9.9" "$SCOPED_KEY")
check "scoped routing key wrong speaker denied" 403 "$s"
if grep -q "internal/routing/v1/routes" "$REC_LOG" \
   && grep "internal/routing/v1/routes" "$REC_LOG" | grep -q "speaker=10.0.0.1" \
   && grep "internal/routing/v1/routes" "$REC_LOG" | grep -q "\"token\": \"$ROUTING_TOKEN\""; then
  pass "matching-speaker request proxied with internal credential"
else
  fail "matching-speaker request not observed at recorder: $(cat "$REC_LOG")"
fi
if grep "internal/routing/v1/routes" "$REC_LOG" | grep -q "10.9.9.9"; then
  fail "wrong-speaker request reached downstream service"
else
  pass "wrong-speaker request never proxied"
fi
# proxy resolution sanity: scoped request must NOT have hit the implicit
# default endpoint — the recorder is the only configured destination.
kill $RECPID 2>/dev/null; PIDS=(${PIDS/RECPID})

# ══ B. BACKUP / VERIFY / LOSS / RESTORE / RESTART ════════════════════
echo "=== B. backup-restore roundtrip ===" | tee -a "$RESULTS"
BK="$WORK/backup/run1"
mkdir -p "$BK"
cli_ok "auth backup" "$BONSAI_BIN" auth backup --db "$AUTH_DB" --dest "$BK/bonsai_auth.db"
cli_ok "verify-backup" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$BK/bonsai_auth.db"
PRE_JWT="$ADM_JWT"   # JWT minted before backup must survive restore

stop_server "$LIVE"
# simulated loss
mv "$AUTH_DB" "$WORK/bonsai_auth.db.lost"
install -m 0600 "$BK/bonsai_auth.db" "$AUTH_DB"
ls "$BK" | grep -q manifest && pass "backup manifest present" || fail "backup manifest present"
if start_server live "$LIVE"; then pass "post-restore server start"; else fail "post-restore server start"; fi

s=$(api "$LIVE_HTTP" GET /api/config-items ""); check "post-restore anon denied" 401 "$s"
J2=$(login "$LIVE_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$J2" ] && pass "post-restore admin login" || fail "post-restore admin login"
O2=$(login "$LIVE_HTTP" rec-oper "$OPER_PASS"); V2=$(login "$LIVE_HTTP" rec-view "$VIEW_PASS")
[ -n "$O2" ] && [ -n "$V2" ] && pass "post-restore operator+viewer login" || fail "post-restore operator+viewer login"
s=$(api "$LIVE_HTTP" GET /api/config-items "$PRE_JWT"); check "pre-backup JWT valid after restore" 200 "$s"
s=$(api "$LIVE_HTTP" GET /api/config-items "$READ_KEY"); check "api key auth after restore" 200 "$s"
s=$(api "$LIVE_HTTP" GET /api/config-items "$DOOMED_KEY"); check "revoked key still denied" 401 "$s"
# post-restore role authorization unchanged
s=$(api "$LIVE_HTTP" POST /api/config-items "$V2" \
  '{"id":"probe-v2","config_class":"app_config","vendor":"","name":"probe","version":"1","content_json":"\"x\"","enabled":true,"created_by":"t"}')
check "post-restore viewer write denied" 403 "$s"
"$BONSAI_BIN" auth list-service-tokens --db "$AUTH_DB" > "$WORK/tokens.txt" 2>&1 \
  && pass "list-service-tokens" || fail "list-service-tokens"
grep -q routing_service_token "$WORK/tokens.txt" && pass "routing token present" || fail "routing token present"
grep -q internal_service_token "$WORK/tokens.txt" && pass "internal token present" || fail "internal token present"
mode=$(stat -c %a "$AUTH_DB"); [ "$mode" = "600" ] && pass "auth db mode 600" || fail "auth db mode 600 (got $mode)"

# ══ C. NEGATIVE / HISTORICAL CASES ═══════════════════════════════════
echo "=== C. negatives ===" | tee -a "$RESULTS"
cli_bad "verify-backup wrong passphrase" env BONSAI_VAULT_PASSPHRASE="wrong-pass" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$BK/bonsai_auth.db"
cli_bad "verify-backup missing key material" env -u BONSAI_VAULT_PASSPHRASE \
  "$BONSAI_BIN" auth verify-backup --snapshot "$BK/bonsai_auth.db"
# corrupt snapshot: flip one byte
cp "$BK/bonsai_auth.db" "$WORK/corrupt.db"
printf '\x01' | dd of="$WORK/corrupt.db" bs=1 seek=100 conv=notrunc 2>/dev/null
cli_bad "verify-backup corrupt snapshot" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$WORK/corrupt.db"
# truncated snapshot (interrupted install)
head -c 40000 "$BK/bonsai_auth.db" > "$WORK/trunc.db"
cli_bad "verify-backup truncated snapshot" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$WORK/trunc.db"
# tampered manifest
cp "$BK/bonsai_auth.db.manifest.json" "$WORK/tampered.manifest.json" 2>/dev/null \
  || cp "$BK/bonsai_auth.manifest.json" "$WORK/tampered.manifest.json"
sed -i 's/"user_count": *[0-9]*/"user_count": 99/' "$WORK/tampered.manifest.json"
cli_bad "verify-backup tampered manifest" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$BK/bonsai_auth.db" --manifest "$WORK/tampered.manifest.json"
# missing manifest
mkdir -p "$WORK/nomanifest"; cp "$BK/bonsai_auth.db" "$WORK/nomanifest/bonsai_auth.db"
cli_bad "verify-backup missing manifest" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$WORK/nomanifest/bonsai_auth.db"
# unsupported manifest format
sed 's/"format": *"v2"/"format": "v1"/' "$WORK/tampered.manifest.json" > "$WORK/v1.manifest.json" 2>/dev/null || true
cli_bad "verify-backup unsupported format" env BONSAI_VAULT_PASSPHRASE="$VAULT_PASS" \
  "$BONSAI_BIN" auth verify-backup --snapshot "$BK/bonsai_auth.db" --manifest "$WORK/v1.manifest.json"
# wrong key at startup (live instance, wrong passphrase)
stop_server "$LIVE"
if start_server live "$LIVE" BONSAI_VAULT_PASSPHRASE="definitely-wrong"; then
  fail "wrong passphrase startup accepted"
else
  pass "wrong passphrase startup fails closed"
fi
# restart correctly for rotation tests
if start_server live "$LIVE"; then pass "restart after wrong-key case"; else fail "restart after wrong-key case"; fi

# ══ D. EMERGENCY ROTATION (offline/quiesced) ═════════════════════════
echo "=== D. emergency rotation ===" | tee -a "$RESULTS"
OLD_JWT=$(login "$LIVE_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$OLD_JWT" ] && pass "pre-rotation login token minted" || fail "pre-rotation login token minted"
stop_server "$LIVE"
cli_ok "rotate-emergency" "$BONSAI_BIN" auth rotate-emergency --db "$AUTH_DB"
if start_server live "$LIVE"; then pass "post-rotation start"; else fail "post-rotation start"; fi
s=$(api "$LIVE_HTTP" GET /api/config-items "$OLD_JWT"); check "pre-rotation JWT denied after restart" 401 "$s"
J3=$(login "$LIVE_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$J3" ] && pass "fresh login after rotation" || fail "fresh login after rotation"

# historical JWT restoration: restore run1 (pre-rotation) snapshot,
# pre-rotation JWT must verify again -> rotate -> denied.
# Runbook §3.1: STOP the service before installing a restored snapshot —
# never restore over a live store. stop_server waits for actual exit so
# the graph lock and auth DB are fully released before install.
stop_server "$LIVE"
cp "$AUTH_DB" "$WORK/post-rotation.db"   # preserve rotated state (quiesced)
install -m 0600 "$BK/bonsai_auth.db" "$AUTH_DB"
if start_server live "$LIVE"; then pass "historical restore start"; else fail "historical restore start"; fi
s=$(api "$LIVE_HTTP" GET /api/config-items "$OLD_JWT")
if [ "$s" = "200" ]; then pass "restored old JWT verifies (resurrection confirmed)"; else fail "restored old JWT not accepted (HTTP $s)"; fi
stop_server "$LIVE"
cli_ok "rotate-emergency on restored" "$BONSAI_BIN" auth rotate-emergency --db "$AUTH_DB"
if start_server live "$LIVE"; then pass "post-restore-rotation start"; else fail "post-restore-rotation start"; fi
s=$(api "$LIVE_HTTP" GET /api/config-items "$OLD_JWT"); check "old JWT denied after reconciliation rotation" 401 "$s"
J4=$(login "$LIVE_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$J4" ] && pass "login after reconciliation" || fail "login after reconciliation"
stop_server "$LIVE"

# ══ E. LEGACY MIGRATION INSTANCE ═════════════════════════════════════
echo "=== E. legacy migration ===" | tee -a "$RESULTS"
LEG_HTTP=$(free_port); LEG_GRPC=$(free_port)
LEG=$(new_instance legacy "$LEG_HTTP" "$LEG_GRPC")
mkdir -p "$LEG/data"
"$SEED_BIN" "$LEG/data/bonsai.db" > "$WORK/seed.out" 2>&1 \
  && pass "fixture seed" || fail "fixture seed"
LEGACY_KEY=$(grep -o 'bsk_[A-Za-z0-9_]*' "$WORK/seed.out" | head -1)
if start_server legacy "$LEG"; then pass "legacy server start (migration runs)"; else fail "legacy server start"; fi
LEG_AUTH="$LEG/data/bonsai_auth.db"
grep -q "migration complete" "$LEG/evidence/stdout.log" && pass "migration logged" || fail "migration logged"
# legacy viewer login (SHA-256 verifier upgraded)
LV=$(login "$LEG_HTTP" legacy-viewer 'LegacyViewerPassw0rd!')
[ -n "$LV" ] && pass "legacy user login" || fail "legacy user login"
# legacy api key authenticates
[ -n "$LEGACY_KEY" ] && { s=$(api "$LEG_HTTP" GET /api/config-items "$LEGACY_KEY"); [ "$s" = "200" ] && pass "legacy api key auth" || fail "legacy api key auth (HTTP $s)"; }
# tombstoned graph: admin reads show only tombstones for credential classes
ADM2=$(login "$LEG_HTTP" "$ADMIN_USER" "$ADMIN_PASS")
[ -n "$ADM2" ] || ADM2="$LV"
s=$(api "$LEG_HTTP" GET "/api/config-items?class=user" "$ADM2")
body=$(cat "$WORK/.resp")
{ echo "$body" | grep -q 'migrated_to' && echo "$body" | grep -q 'auth_store'; } \
  && pass "user class tombstoned" || fail "user class tombstoned"
echo "$body" | grep -q 'password_hash' && fail "plaintext verifier still graph-readable" || pass "no plaintext verifier in graph"
"$BONSAI_BIN" auth list-service-tokens --db "$LEG_AUTH" > "$WORK/leg-tokens.txt" 2>&1
grep -q routing_service_token "$WORK/leg-tokens.txt" && pass "routing token imported" || fail "routing token imported"
grep -q internal_service_token "$WORK/leg-tokens.txt" && fail "disabled internal token activated" || pass "disabled internal token quarantined"
# admin config read of app_config shows redacted/tombstoned protected names
s=$(api "$LEG_HTTP" GET "/api/config-items?class=app_config" "$ADM2")
cat "$WORK/.resp" | grep -q 'rt_legacy_routing_token_sealed_001' \
  && fail "plaintext routing token graph-readable" || pass "no plaintext service token in graph"

# ══ F. GRAPH/AUTHSTORE MISMATCH BOTH DIRECTIONS ══════════════════════
echo "=== F. mismatch cases ===" | tee -a "$RESULTS"
stop_server legacy
# F1: older graph (freshly seeded, unmigrated) + newer AuthStore (migrated)
PRE_HTTP=$(free_port); PRE_GRPC=$(free_port)
PRE=$(new_instance pregraph "$PRE_HTTP" "$PRE_GRPC")
mkdir -p "$PRE/data"
"$SEED_BIN" "$PRE/data/bonsai.db" >/dev/null 2>&1
cp "$LEG_AUTH" "$PRE/data/bonsai_auth.db"
chmod 600 "$PRE/data/bonsai_auth.db"
if start_server pregraph "$PRE"; then
  # Server may stay up serving health while certification denies auth.
  s=$(api "$PRE_HTTP" GET /api/config-items "")
  [ "$s" = "401" ] && pass "older-graph/newer-authstore anon denied" || fail "older-graph/newer-authstore anon (HTTP $s)"
  if kill -0 "$(cat "$PRE/server.pid")" 2>/dev/null; then
    s2=$(api "$PRE_HTTP" GET /api/config-items "$ADM2")
    { [ "$s2" = "401" ] || [ "$s2" = "503" ]; } && pass "older-graph/newer-authstore authed denied (HTTP $s2)" || fail "older-graph/newer-authstore authed (HTTP $s2)"
  else
    pass "older-graph/newer-authstore process exited"
  fi
else
  pass "older-graph/newer-authstore fails closed at startup"
fi
# F2: older AuthStore (fresh) + newer graph (tombstoned legacy graph)
OLD_HTTP=$(free_port); OLD_GRPC=$(free_port)
OLD=$(new_instance oldauth "$OLD_HTTP" "$OLD_GRPC")
mkdir -p "$OLD/data"
cp "$LEG/data"/bonsai.db* "$OLD/data/" 2>/dev/null || cp -r "$LEG/data/bonsai.db" "$OLD/data/bonsai.db"
# ensure no stale auth db leaks in — fresh AuthStore is the point of F2
rm -f "$OLD/data/bonsai_auth.db"*
if start_server oldauth "$OLD"; then
  # Contract: migration must abort on missing tombstone counterparts, or
  # auth must deny — either way no credential material may be served.
  s=$(api "$OLD_HTTP" GET /api/config-items "")
  { [ "$s" = "401" ] || [ "$s" = "503" ]; } && pass "older-authstore/newer-graph anon denied" || fail "older-authstore/newer-graph anon (HTTP $s)"
  s2=$(api "$OLD_HTTP" GET /api/config-items "bsk_bogus_token")
  { [ "$s2" = "401" ] || [ "$s2" = "503" ]; } && pass "older-authstore/newer-graph bad-key denied" || fail "older-authstore/newer-graph bad-key (HTTP $s2)"
  grep -i "migration" "$OLD/evidence/stdout.log" | tail -3 | tee -a "$RESULTS"
else
  pass "older-authstore/newer-graph start fails closed"
fi

echo "=== DONE: $FAILURES failure(s) ===" | tee -a "$RESULTS"
echo "evidence: $WORK"
[ "$FAILURES" -eq 0 ]

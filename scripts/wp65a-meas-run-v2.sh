#!/bin/bash
# usage: wp65a-meas-run-v2.sh <scale> <mode> <rate> <corpus> <transport> <tag>
set -u
SCALE=$1; MODE=$2; RATE=$3; CORPUS=$4; TRANS=$5; TAG=$6
RUNID="$TAG-$(date +%s)"
E2E=${WP65A_MEAS_ROOT:-/tmp}/wp65a-m2-$RUNID
BIN=${BONSAI_BIN:-$(git rev-parse --show-toplevel)/target/debug/bonsai}
HTTP=127.0.0.1:$((43000 + RANDOM % 5000))
METR=127.0.0.1:$((48000 + RANDOM % 5000))
UDP=127.0.0.1:$((54000 + RANDOM % 5000))
TCP=$UDP
CPID=""
cleanup() {
  if [ -n "$CPID" ]; then
    kill -TERM $CPID 2>/dev/null
    for i in $(seq 1 15); do kill -0 $CPID 2>/dev/null || break; sleep 1; done
    kill -9 $CPID 2>/dev/null
  fi
}
trap cleanup EXIT
rm -rf $E2E; mkdir -p $E2E/runtime $E2E/archive
cat > $E2E/bonsai.toml <<TOML
graph_path = "$E2E/runtime/bonsai.db"
http_addr = "$HTTP"
metrics_addr = "$METR"
[runtime]
mode = "all"
[credentials]
path = "$E2E/creds"
passphrase_env = "BONSAI_VAULT_PASSPHRASE"
[retention]
enabled = false
max_state_change_events = 0
[signals.syslog]
enabled = true
udp_addr = "$UDP"
tcp_addr = "$TCP"
archive_path = "$E2E/archive/syslog.jsonl"
TOML
sha256sum $BIN | awk '{print $1}' > $E2E/bin.sha256
git rev-parse HEAD > $E2E/source.sha
echo "{\"run_id\":\"$RUNID\",\"tag\":\"$TAG\",\"deployment\":\"native-host\",\"network_ns\":\"host\",\"sender_dest\":\"$UDP\",\"tcp_dest\":\"$TCP\",\"http\":\"$HTTP\",\"metrics\":\"$METR\",\"core_pid_file\":\"$E2E/core.pid\",\"workdir\":\"$E2E\"}" > $E2E/identity.json
BONSAI_VAULT_PASSPHRASE=meas BONSAI_ALLOW_INSECURE_NO_AUTH=1 $BIN --config=$E2E/bonsai.toml > $E2E/core.log 2>&1 &
CPID=$!
echo $CPID > $E2E/core.pid
# Readiness: metrics endpoint AND stats endpoint must return HTTP 200,
# and the recorded PID must be alive — not merely a port accepting TCP.
READY=0
for i in $(seq 1 90); do
  kill -0 $CPID 2>/dev/null || { echo "core died during startup"; tail -20 $E2E/core.log; exit 1; }
  if curl -sf --max-time 2 "http://$METR/metrics" >/dev/null 2>&1 \
     && curl -sf --max-time 2 "http://$HTTP/api/ml/embeddings/stats" >/dev/null 2>&1; then
    READY=1; break
  fi
  sleep 1
done
[ "$READY" = "1" ] || { echo "readiness timeout"; tail -20 $E2E/core.log; exit 1; }
sleep 2
python3 -m py_compile $(git rev-parse --show-toplevel)/scripts/wp65a_measurements_v2.py || exit 1
PYTHONPATH=$(git rev-parse --show-toplevel)/python /home/arjuna/Desktop/bonsai/.venv/bin/python \
  $(git rev-parse --show-toplevel)/scripts/wp65a_measurements_v2.py \
  --workdir $E2E --api-url http://$HTTP --metrics-url http://$METR/metrics \
  --syslog-udp $UDP --syslog-tcp $TCP \
  --scale $SCALE --corpus $CORPUS --mode $MODE --rate $RATE --transport $TRANS \
  --core-pid $CPID --graph-path $E2E/runtime/bonsai.db \
  --archive-path $E2E/archive/syslog.jsonl --registry $E2E/template_registry.db \
  --report $E2E/report.json
RC=$?
# Graceful shutdown: SIGTERM, bounded wait for checkpoint, then SIGKILL.
kill -TERM $CPID 2>/dev/null
for i in $(seq 1 20); do kill -0 $CPID 2>/dev/null || break; sleep 1; done
kill -9 $CPID 2>/dev/null
echo "MEAS-V2 $RUNID rc=$RC report=$E2E/report.json"
exit $RC

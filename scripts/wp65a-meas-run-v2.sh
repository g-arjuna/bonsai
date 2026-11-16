#!/bin/bash
# usage: wp65a-meas-run-v2.sh <scale> <mode> <rate> <corpus> <transport> <tag>
set -u
SCALE=$1; MODE=$2; RATE=$3; CORPUS=$4; TRANS=$5; TAG=$6
E2E=${WP65A_MEAS_ROOT:-/tmp}/wp65a-m2-$TAG
BIN=${BONSAI_BIN:-$(git rev-parse --show-toplevel)/target/debug/bonsai}
HTTP=127.0.0.1:$((43000 + RANDOM % 5000))
METR=127.0.0.1:$((48000 + RANDOM % 5000))
UDP=127.0.0.1:$((54000 + RANDOM % 5000))
TCP=$UDP
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
BONSAI_VAULT_PASSPHRASE=meas BONSAI_ALLOW_INSECURE_NO_AUTH=1 $BIN --config=$E2E/bonsai.toml > $E2E/core.log 2>&1 &
CPID=$!
echo $CPID > $E2E/core.pid
for i in $(seq 1 90); do curl -s --max-time 2 "http://$METR/metrics" >/dev/null 2>&1 && break; sleep 1; done
for i in $(seq 1 90); do curl -s --max-time 2 "http://$HTTP/api/ml/embeddings/stats" >/dev/null 2>&1 && break; sleep 1; done
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
kill $CPID 2>/dev/null; sleep 2; kill -9 $CPID 2>/dev/null
echo "MEAS-V2 $TAG rc=$RC report=$E2E/report.json"

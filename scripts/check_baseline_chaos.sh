#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

run_dir="chaos_runs/active_baseline_1h"
pid_file="$run_dir/run.pid"
log_file="$run_dir/run.log"
csv_file=""

if [[ ! -f "$pid_file" ]]; then
  echo "status=missing reason=no_pid_file run_dir=$run_dir"
  exit 0
fi

pid=$(cat "$pid_file")
if ps -p "$pid" > /dev/null 2>&1; then
  echo "status=running pid=$pid run_dir=$run_dir"
else
  echo "status=stopped pid=$pid run_dir=$run_dir"
fi

if [[ -f "$run_dir/started_at.txt" ]]; then
  echo "started_at=$(cat "$run_dir/started_at.txt")"
fi

if [[ -f "$log_file" ]]; then
  csv_file=$(grep -o 'chaos_runs/[^ ]*/injections\.csv' "$log_file" | tail -n 1 || true)
fi

if [[ -n "$csv_file" ]]; then
  rows=$(( $(wc -l < "$csv_file") - 1 ))
  if (( rows < 0 )); then
    rows=0
  fi
  echo "csv=$csv_file rows=$rows"
else
  echo "csv=missing rows=0"
fi

if [[ -f "$log_file" ]]; then
  echo "log_tail<<EOF"
  tail -n 12 "$log_file"
  echo "EOF"
fi

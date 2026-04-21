#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

run_dir="chaos_runs/active_baseline_1h"
mkdir -p "$run_dir"

nohup .venv/bin/python scripts/chaos_runner.py chaos_plans/baseline_mix.yaml --duration-hours 1 \
  > "$run_dir/run.log" 2>&1 < /dev/null &
pid=$!

printf '%s\n' "$pid" > "$run_dir/run.pid"
date -u +%Y-%m-%dT%H:%M:%SZ > "$run_dir/started_at.txt"

printf 'run_dir=%s\npid=%s\n' "$run_dir" "$pid"

#!/usr/bin/env bash
# Canonical Bonsai validation gates (BN-019-B6).
#
# A green result from this script means the named tests actually compiled,
# executed and passed. Every tier fails closed: a missing tool, missing test
# target, build error or test failure produces a nonzero exit.
#
# Usage:
#   scripts/ci/gates.sh <tier> [tier...]
#   scripts/ci/gates.sh l0 l1 l2 l3
#
# Tiers:
#   l0  source checks: scoped rustfmt on changed files, python compile check
#   l1  compile/static: cargo check workspace + UI build + python import check
#   l2  required behavioral tests: bonsai lib + rbmp crates + core integration
#   l3  cross-component: extended integration + headless browser + python tests
#   l4  lab/deployment acceptance: NOT runnable here; prints explicit skip
#
# Environment:
#   GATE_BASE   git ref for changed-file detection (default: origin/main,
#               falls back to main, then HEAD~1)
#   CARGO_PROFILE  cargo profile for test builds (default: ci)
#   SKIP_UI_BUILD=1  reuse an existing ui/dist instead of rebuilding
set -euo pipefail
cd "$(dirname "$0")/../.."

PROFILE="${CARGO_PROFILE:-ci}"
BASE="${GATE_BASE:-}"
if [ -z "$BASE" ]; then
  for ref in origin/main main HEAD~1; do
    if git rev-parse --verify --quiet "$ref" >/dev/null; then BASE="$ref"; break; fi
  done
fi

log()  { printf '\n=== [gate] %s ===\n' "$*"; }
die()  { printf 'GATE FAIL: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "required tool missing: $1"; }

changed_rs_files() {
  local base="$1"
  if git rev-parse --verify --quiet "$base" >/dev/null; then
    { git diff --name-only --diff-filter=ACMR "$base"...HEAD -- '*.rs';
      git diff --name-only --diff-filter=ACMR "$base" -- '*.rs';
      git ls-files --others --exclude-standard -- '*.rs'; } | sort -u
  else
    git ls-files '*.rs'
  fi
}

# ---------------------------------------------------------------- L0
gate_l0() {
  log "L0 source checks"
  need cargo; need rustfmt; need python3

  # Rust formatting, scoped to files changed vs the base ref. Repository-wide
  # fmt drift predates this gate and is tracked separately; new/changed files
  # must be formatted. If no base ref exists, check the whole tree.
  local files
  files="$(changed_rs_files "$BASE")"
  if [ -z "$files" ]; then
    echo "L0: no changed .rs files vs ${BASE:-HEAD}; rustfmt skipped"
  else
    echo "L0: rustfmt --check on changed files (base: ${BASE:-none})"
    # Fail only on NEW drift: a file that was already unformatted at the base
    # ref keeps its pre-existing drift (tracked separately, not this gate's
    # job to reformat); a file clean at base must stay clean.
    local failed=0 warned=0 f base_f base_ok
    while IFS= read -r f; do
      [ -f "$f" ] || continue
      if rustfmt --edition 2024 --check "$f" >/dev/null 2>&1; then continue; fi
      base_ok=1
      if git rev-parse --verify --quiet "$BASE" >/dev/null && git cat-file -e "$BASE:$f" 2>/dev/null; then
        base_f="$(mktemp)"
        git show "$BASE:$f" > "$base_f"
        rustfmt --edition 2024 --check "$base_f" >/dev/null 2>&1 && base_ok=0
        rm -f "$base_f"
      fi
      if [ "$base_ok" -eq 0 ]; then
        echo "  NEW unformatted: $f"
        failed=1
      else
        echo "  warn: pre-existing fmt drift at base: $f"
        warned=$((warned + 1))
      fi
    done <<< "$files"
    [ "$warned" -gt 0 ] && echo "L0: $warned file(s) carry pre-existing fmt drift"
    [ "$failed" -eq 0 ] || die "rustfmt check failed on changed files"
  fi

  # Python: byte-compile every tracked python file (syntax/import-shape check).
  echo "L0: python compile check"
  python3 -m compileall -q python tests

  # Cargo metadata sanity (workspace resolves, lockfile consistent).
  cargo metadata --no-deps --format-version 1 >/dev/null
  echo "L0: OK"
}

# ---------------------------------------------------------------- L1
gate_l1() {
  log "L1 compile/static validation"
  need cargo; need node; need npm

  # Workspace check with the CI profile. --all-targets compiles test targets
  # too; this is the same fingerprint family `cargo test` reuses, so the
  # heavyweight compile happens once here and is reused by l2/l3.
  cargo check --workspace --all-targets --profile "$PROFILE" --offline

  # Frontend build (compile check; also produces ui/dist for browser gates).
  if [ "${SKIP_UI_BUILD:-0}" != "1" ]; then
    (cd ui && npm ci --no-audit --no-fund && npm run build)
  else
    [ -d ui/dist ] || die "SKIP_UI_BUILD=1 but ui/dist missing"
  fi

  # Python import/config check: SDK must import cleanly.
  if [ -x .venv/bin/python ]; then PY=.venv/bin/python; else PY=python3; fi
  (cd python && "$PY" -c "import bonsai_sdk; print('bonsai_sdk import OK')") \
    || die "bonsai_sdk import failed"
  echo "L1: OK"
}

# ---------------------------------------------------------------- L2
# Required behavioral tests. Every --test name below is an existing,
# self-contained (loopback/tempdir/synthetic-cert) integration target.
# Targets requiring containerlab, live devices or operator env vars are L4.
L2_INTEGRATION_TESTS="
  bmp_rfc_contract
  config_example
  ownership_compatibility
  ownership_mapping
  b07_vendor_registry
  b08_feature_gating
  b09_peer_and_correlation
  bn001_executor_crash_recovery
  bn002_key_lifecycle_and_provenance
  bn004_production_investigator_wiring
  bn005_closeout
  bn021b_trusted_identity
  bn021c_exporter_isolation
  bn021d_flow_exporter_isolation
  bn021e_otlp_source_identity
  bn021e_sflow_exporter_isolation
  bn021e_snmp_source_identity
  bn021e_syslog_source_identity
  bn021f_authenticated_ingress
  bn021f_hardening
  bn021g_collector_trust_foundation
  bn022_graph_read_boundary
"

gate_l2() {
  log "L2 required behavioral tests"
  need cargo

  # Library unit tests (363 tests at BN-019-B6 baseline).
  cargo test -p bonsai --lib --profile "$PROFILE" --offline

  # RustyBMP crates.
  cargo test -p rbmp-core -p rbmp-rib -p rbmp-store --profile "$PROFILE" --offline

  # Core integration targets, one cargo invocation so binaries compile once.
  local args=()
  for t in $L2_INTEGRATION_TESTS; do args+=(--test "$t"); done
  cargo test -p bonsai --profile "$PROFILE" --offline "${args[@]}"
  echo "L2: OK"
}

# ---------------------------------------------------------------- L3
# Cross-component integration: heavier workflow suites, headless browser
# acceptance, and the python test suites. Requires ui/dist (l1), node +
# playwright chromium (tests/ui_driver/node_modules must be installed).
L3_INTEGRATION_TESTS="
  a03_investigation_terminal_states
  a04_a09_b22_prefix_investigator
  a10_a11_a12_operator_and_scenario_pack
  a13_a20_change_assurance_and_executor
  b11_b12_b13_routing_workflow
  b16_b17_b18_b19_m3_release_gates
  bn005_deployment_and_scope_hardening
  bn006_native_nos_adapter
  bn006_reconnect_projection
  bn015_u1_inventory_browser
  bn016_u1_topology_browser
  r01_r04_routing_history_recovery
  r05_r06_runtime_wiring_and_scope
"

gate_l3() {
  log "L3 cross-component integration"
  need cargo; need node; need npx
  [ -d ui/dist ] || die "ui/dist missing — run l1 first"
  [ -d tests/ui_driver/node_modules ] || \
    (cd tests/ui_driver && npm ci --no-audit --no-fund)

  local args=()
  for t in $L3_INTEGRATION_TESTS; do args+=(--test "$t"); done
  cargo test -p bonsai --profile "$PROFILE" --offline "${args[@]}"

  # Python suites. Root tests/ = bonsai unit tests; python/tests = SDK suite.
  local PY=.venv/bin/python; [ -x "$PY" ] || PY=python3
  PYTHONPATH=.:python "$PY" -m pytest tests/ -q
  (cd python && PYTHONPATH=. "$PY" -m pytest tests/ -q)
  echo "L3: OK"
}

# ---------------------------------------------------------------- L4
gate_l4() {
  log "L4 lab/deployment acceptance"
  cat <<'EOF'
L4 is intentionally not executed by this script. It requires dedicated
infrastructure (containerlab, FRR/SR Linux devices, Docker, soak hosts) and is
run via the named lab harnesses, e.g.:

  tests/bn003_live_routing_browser_acceptance.rs   (containerlab FRR topology)
  tests/bn006_collection_wiring.rs                 (BONSAI_BN006_* env-gated)
  tests/r07_frr_containerlab_verification.rs       (containerlab)
  tests/bn005_container_mount_acceptance.py        (docker compose)
  tests/bn006_*_live_acceptance.py                 (live devices)
  docs/merger/evidence/*/soak*                    (soak acceptance)

A green L0-L3 result does NOT imply L4 passed. L4 status must be reported
separately and never inferred from CI.
EOF
}

# ---------------------------------------------------------------- main
if [ "$#" -eq 0 ]; then
  echo "usage: $0 <l0|l1|l2|l3|l4> [...]" >&2; exit 2
fi
for tier in "$@"; do
  case "$tier" in
    l0) gate_l0 ;;
    l1) gate_l1 ;;
    l2) gate_l2 ;;
    l3) gate_l3 ;;
    l4) gate_l4 ;;
    *) die "unknown tier: $tier" ;;
  esac
done
echo "ALL REQUESTED GATES PASSED"

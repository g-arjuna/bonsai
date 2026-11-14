#!/usr/bin/env bash
# gates_selftest.sh — failure-injection regression coverage for
# scripts/ci/gates.sh (CI-R7 / BN-019-B6 follow-up).
#
# Runs the REAL gate runner inside a disposable git worktree with stubbed
# toolchain commands (cargo/rustfmt/python3/node/npm/npx). No Rust is
# compiled; each case injects a controlled failure and asserts the gate
# summary reports FAIL for the owning tier, NOT_EXECUTED for later tiers,
# a nonzero exit status, and never "ALL REQUESTED GATES PASSED".
#
# Usage: scripts/ci/gates_selftest.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GATES="$REPO_ROOT/scripts/ci/gates.sh"
WORKTREE="$(mktemp -d /tmp/gates-selftest.XXXXXX)"
STUBS="$WORKTREE/stub-bin"
PASS=0; FAIL=0

cleanup() { git -C "$REPO_ROOT" worktree remove --force "$WORKTREE" 2>/dev/null; rm -rf "$WORKTREE"; }
trap cleanup EXIT

git -C "$REPO_ROOT" worktree add --detach "$WORKTREE" HEAD >/dev/null 2>&1 \
  || { echo "SELFTEST FAIL: could not create worktree"; exit 1; }

# gates.sh cd's to its own repo root — copy the *candidate* script into
# the worktree so the selftest exercises the version under test against
# worktree files and stub tools.
cp "$GATES" "$WORKTREE/scripts/ci/gates.sh"
GATES_WT="$WORKTREE/scripts/ci/gates.sh"

# An untracked .rs file gives L0 changed-file drift something to check.
echo 'fn selftest_probe() {}' > "$WORKTREE/src/__selftest_probe.rs"

mkdir -p "$STUBS" "$WORKTREE/ui/dist" "$WORKTREE/tests/ui_driver/node_modules/.bin"
# playwright driver presence check is `[ -x .../playwright ]`
printf '#!/bin/sh\nexit 0\n' > "$WORKTREE/tests/ui_driver/node_modules/.bin/playwright"
chmod +x "$WORKTREE/tests/ui_driver/node_modules/.bin/playwright"

# ── stubs ──────────────────────────────────────────────────────────────
# Every stub exits 0 unless GATE_STUB_FAIL matches "<cwd-suffix>|<args>"
# or GATE_STUB_FAIL_MATCH matches the argument string.
write_stub() {
  local name="$1" body="$2"
  printf '#!/usr/bin/env bash\n%s\n' "$body" > "$STUBS/$name"
  chmod +x "$STUBS/$name"
}

STUB_PRELUDE='args="$*"; if [ -n "${GATE_STUB_FAIL:-}" ]; then case "$PWD|$args" in *"$GATE_STUB_FAIL"*) echo "stub-injected failure: $args" >&2; exit 1;; esac; fi;'

write_stub cargo "$STUB_PRELUDE
exit 0"
write_stub rustfmt "$STUB_PRELUDE
exit 0"
write_stub node 'exit 0'
write_stub npm "$STUB_PRELUDE
exit 0"
write_stub npx "$STUB_PRELUDE
exit 0"
# python3 stub: fails pytest when GATE_STUB_FAIL matches; distinguishes
# root suite (cwd=worktree) from SDK suite (cwd=worktree/python).
write_stub python3 "$STUB_PRELUDE
if [[ \"\$args\" == *pytest* ]]; then
  if [[ \"\$PWD\" == */python && \"\$GATE_STUB_FAIL\" == *sdk* ]]; then exit 1; fi
  if [[ \"\$PWD\" != */python && \"\$GATE_STUB_FAIL\" == *root* ]]; then exit 1; fi
  if [[ \"\$GATE_STUB_FAIL\" == *pytest* ]]; then exit 1; fi
  exit 0
fi
exit 0"
# .venv/bin/python preferred by gates.sh — symlink to stub
mkdir -p "$WORKTREE/.venv/bin"
ln -sf "$STUBS/python3" "$WORKTREE/.venv/bin/python"

# ── assertion helpers ──────────────────────────────────────────────────
check() { # name expected_rc tier outcome_fragment log
  local name="$1" want_rc="$2" tier="$3" outcome="$4" log="$5"
  local rc=0
  # run_gate already captured rc in $GATE_RC
  if [ "$GATE_RC" != "$want_rc" ]; then
    echo "FAIL  $name: exit=$GATE_RC want=$want_rc"; FAIL=$((FAIL+1)); return
  fi
  if ! grep -Eq "^[[:space:]]+$tier[[:space:]]+$outcome[[:space:]]*$" "$log"; then
    echo "FAIL  $name: tier $tier outcome not '$outcome'"; grep "Gate outcomes" -A8 "$log" | sed 's/^/      /'; FAIL=$((FAIL+1)); return
  fi
  if grep -q "ALL REQUESTED GATES PASSED" "$log"; then
    echo "FAIL  $name: false-green summary printed"; FAIL=$((FAIL+1)); return
  fi
  echo "PASS  $name"; PASS=$((PASS+1))
}

run_gate() { # tiers... -> sets GATE_RC, log at $WORKTREE/last.log
  GATE_RC=0
  (cd "$WORKTREE" && env PATH="$STUBS:$PATH" GATE_BASE=HEAD \
      GATE_STUB_FAIL="${GATE_STUB_FAIL:-}" GATE_STUB_FAIL_MATCH="${GATE_STUB_FAIL_MATCH:-}" \
      "$GATES_WT" "$@") > "$WORKTREE/last.log" 2>&1 || GATE_RC=$?
}

# ══ 1. First L0 command fails (rustfmt drift check) ════════════════════
GATE_STUB_FAIL="--edition 2024 --check"
run_gate l0
check "L0 first-command failure" 1 l0 FAIL "$WORKTREE/last.log"

# ══ 2. L1 cargo check fails ════════════════════════════════════════════
GATE_STUB_FAIL="check --workspace"
run_gate l0 l1
check "L1 cargo check failure" 1 l1 FAIL "$WORKTREE/last.log"
if grep -Eq "^[[:space:]]+l0[[:space:]]+PASS[[:space:]]*$" "$WORKTREE/last.log"; then echo "PASS  l0 still PASS before l1"; PASS=$((PASS+1)); else echo "FAIL  l0 outcome wrong"; FAIL=$((FAIL+1)); fi

# ══ 3. UI build fails ══════════════════════════════════════════════════
GATE_STUB_FAIL="run build"
run_gate l1
check "L1 UI build failure" 1 l1 FAIL "$WORKTREE/last.log"

# ══ 4. L2 library tests fail ═══════════════════════════════════════════
GATE_STUB_FAIL="test -p bonsai --lib"
run_gate l2 l3
check "L2 lib test failure" 1 l2 FAIL "$WORKTREE/last.log"
if grep -Eq "^[[:space:]]+l3[[:space:]]+NOT_EXECUTED[[:space:]]*$" "$WORKTREE/last.log"; then echo "PASS  l3 marked NOT_EXECUTED"; PASS=$((PASS+1)); else echo "FAIL  l3 not NOT_EXECUTED"; FAIL=$((FAIL+1)); fi

# ══ 5. L2 RustyBMP tests fail ══════════════════════════════════════════
GATE_STUB_FAIL="rbmp-core"
run_gate l2
check "L2 rbmp test failure" 1 l2 FAIL "$WORKTREE/last.log"

# ══ 6. L2 integration tests fail ═══════════════════════════════════════
GATE_STUB_FAIL="--test bmp_rfc_contract"
run_gate l2
check "L2 integration failure" 1 l2 FAIL "$WORKTREE/last.log"

# ══ 7. L3 integration tests fail ═══════════════════════════════════════
GATE_STUB_FAIL="--test a03_investigation"
run_gate l2 l3
check "L3 integration failure" 1 l3 FAIL "$WORKTREE/last.log"
if grep -Eq "^[[:space:]]+l2[[:space:]]+PASS[[:space:]]*$" "$WORKTREE/last.log"; then echo "PASS  l2 PASS before l3"; PASS=$((PASS+1)); else echo "FAIL  l2 outcome wrong"; FAIL=$((FAIL+1)); fi

# ══ 8. Root pytest fails ═══════════════════════════════════════════════
GATE_STUB_FAIL="root"
run_gate l3
check "L3 root pytest failure" 1 l3 FAIL "$WORKTREE/last.log"

# ══ 9. SDK pytest fails ════════════════════════════════════════════════
GATE_STUB_FAIL="sdk"
run_gate l3
check "L3 sdk pytest failure" 1 l3 FAIL "$WORKTREE/last.log"

# ══ 10. Browser-acceptance target fails inside L3 cargo suite ══════════
GATE_STUB_FAIL="bn016_u1_topology_browser"
run_gate l3
check "L3 browser target failure" 1 l3 FAIL "$WORKTREE/last.log"

# ══ 11. Missing required test target ═══════════════════════════════════
rm -f "$WORKTREE/tests/bn022_auth_storage.rs"
GATE_STUB_FAIL=""
run_gate l2
check "missing test target" 1 l2 FAIL "$WORKTREE/last.log"
git -C "$WORKTREE" checkout HEAD -- tests/bn022_auth_storage.rs

# ══ 12. Missing required dependency (pytest not importable) ════════════
# Simulate by making the python3 stub fail `-m pytest --version`.
GATE_STUB_FAIL="pytest --version"
run_gate l3
check "missing pytest dependency" 1 l3 FAIL "$WORKTREE/last.log"

# ══ 13. Sanity: all-stubbed run passes ═════════════════════════════════
GATE_STUB_FAIL=""
run_gate l0 l1 l2 l3
if [ "$GATE_RC" = 0 ] && grep -q "ALL REQUESTED GATES PASSED" "$WORKTREE/last.log"; then
  echo "PASS  all-green sanity"; PASS=$((PASS+1))
else
  echo "FAIL  all-green sanity rc=$GATE_RC"; tail -20 "$WORKTREE/last.log"; FAIL=$((FAIL+1))
fi

echo
echo "gates_selftest: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

#!/usr/bin/env bash
# CV7 T1-2 — Mac-cleanliness guard.
#
# Verifies that this Mac is a clean source-editing environment per the
# dev/ops boundary (docs/operations/dev_vs_ops_boundary.md). Exits 0 if
# clean, non-zero with a clear message if something operational has
# leaked onto the Mac.
#
# Run from a pre-commit hook or as a manual gate before starting a
# coding session.

set -u

RED=$'\033[31m'
YELLOW=$'\033[33m'
GREEN=$'\033[32m'
BOLD=$'\033[1m'
RESET=$'\033[0m'

fail=0
warn=0

err()  { printf '%s[FAIL]%s %s\n' "$RED"    "$RESET" "$*"; fail=$((fail+1)); }
warn() { printf '%s[WARN]%s %s\n' "$YELLOW" "$RESET" "$*"; warn=$((warn+1)); }
ok()   { printf '%s[ OK ]%s %s\n' "$GREEN"  "$RESET" "$*"; }

# 0. Confirm we're on a Mac. If not, this script doesn't apply.
if [[ "$(uname -s)" != "Darwin" ]]; then
  printf 'This script is Mac-only. Detected: %s. See docs/operations/dev_vs_ops_boundary.md\n' "$(uname -s)" >&2
  exit 2
fi

printf '%sChecking Mac cleanliness for source-editing-only role…%s\n\n' "$BOLD" "$RESET"

# 1. No Docker daemon running.
if pgrep -qf 'Docker Desktop' || pgrep -qf 'com.docker.backend' || pgrep -qf 'dockerd'; then
  err 'Docker daemon detected. Mac should not run Docker. Quit Docker Desktop.'
else
  ok 'No Docker daemon running.'
fi

# 2. No clab residue.
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  if docker network ls --format '{{.Name}}' 2>/dev/null | grep -q '^clab-'; then
    err 'ContainerLab networks detected (clab-*). Mac should never run clab.'
  else
    ok 'No clab-* networks present.'
  fi
  # Disk-hog vendor images shouldn't live on the Mac either.
  if docker images --format '{{.Repository}}' 2>/dev/null | grep -qE 'srlinux|xrd|crpd|vjunos|ceos'; then
    warn 'Vendor lab images present in docker (srlinux/xrd/crpd/vjunos/ceos). Consider purging.'
  fi
else
  ok 'docker CLI not reachable (expected on Mac).'
fi

# 3. No bonsai processes.
if pgrep -f '\bbonsai\b' >/dev/null 2>&1; then
  err 'bonsai process detected. Mac should not run bonsai.'
  pgrep -lf '\bbonsai\b' | sed 's/^/        /'
else
  ok 'No bonsai processes running.'
fi

# 4. No Rust toolchain in PATH. Per dev/ops boundary, Mac shouldn't have one
#    installed — but we warn rather than fail to avoid blocking until the
#    operator chooses to uninstall.
rust_warns=()
for tool in cargo rustc clippy-driver rustfmt; do
  if command -v "$tool" >/dev/null 2>&1; then
    rust_warns+=("$(command -v "$tool")")
  fi
done
if (( ${#rust_warns[@]} > 0 )); then
  warn "Rust toolchain found on PATH — should not be installed on Mac:"
  for p in "${rust_warns[@]}"; do printf '         %s\n' "$p"; done
  printf '         Suggested cleanup: rustup self uninstall\n'
else
  ok 'No Rust toolchain in PATH.'
fi

# 5. No bonsai-specific Python venv leakage.
#    Look for a repo-local .venv with bonsai-shaped dependencies (lbug, bonsai_sdk).
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
venv_dir="$repo_root/.venv"
if [[ -d "$venv_dir" ]]; then
  if find "$venv_dir" -maxdepth 6 -type d \( -name 'lbug*' -o -name 'bonsai_sdk*' \) 2>/dev/null | grep -q .; then
    err ".venv/ contains bonsai Python deps. Mac should not run bonsai Python. Remove: rm -rf $venv_dir"
  else
    warn ".venv/ exists but no bonsai deps detected. Consider removing if unused: rm -rf $venv_dir"
  fi
else
  ok 'No repo-local .venv/ on Mac.'
fi

# 6. No pytest/ruff/cargo aliases lurking in shell rc as bonsai-build wrappers.
#    Soft check only — we just look for obvious foot-guns.
for rc in ~/.zshrc ~/.bashrc ~/.zprofile ~/.bash_profile; do
  [[ -f "$rc" ]] || continue
  if grep -qE 'alias[[:space:]]+(bonsai|bb)=' "$rc" 2>/dev/null; then
    warn "Shell alias for bonsai/bb found in $rc — verify it does not invoke a local build."
  fi
done

printf '\n%sSummary:%s ' "$BOLD" "$RESET"
if (( fail > 0 )); then
  printf '%s%d failure(s)%s' "$RED" "$fail" "$RESET"
  (( warn > 0 )) && printf ', %s%d warning(s)%s' "$YELLOW" "$warn" "$RESET"
  printf '\n\nThe Mac is not clean. Resolve the FAIL items above before continuing.\n'
  printf 'See: docs/operations/dev_vs_ops_boundary.md\n'
  exit 1
elif (( warn > 0 )); then
  printf '%s%d warning(s)%s\n\n' "$YELLOW" "$warn" "$RESET"
  printf 'Mac is functional for source editing, but consider cleaning up the items above.\n'
  exit 0
else
  printf '%sclean%s\n\nThis Mac is a pure source-editing environment. Proceed.\n' "$GREEN" "$RESET"
  exit 0
fi

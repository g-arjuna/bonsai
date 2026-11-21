#!/usr/bin/env bash
# DEPRECATED — replaced by scripts/ci/build-benchmark.sh (BN-019).
#
# The old implementation ran `cargo clean`, touched tracked source files and
# `docker build --no-cache` on every invocation. That behavior is destructive
# on a shared build host and is no longer supported.
#
# This wrapper only forwards to the safe, non-destructive benchmark:
#   scripts/ci/build-benchmark.sh "$@"
set -euo pipefail
cd "$(dirname "$0")/.."
echo "build_bench.sh is deprecated; forwarding to scripts/ci/build-benchmark.sh" >&2
exec scripts/ci/build-benchmark.sh "$@"

#!/usr/bin/env bash
# Assemble a prebuilt lbug/LadybugDB native artifact directory
# (LBUG_LIBRARY_DIR / LBUG_INCLUDE_DIR mode of the pinned lbug 0.15.3
# build.rs). Once prepared, every Cargo unit kind (check/build/test,
# any profile, any trusted worktree) compiles only the small cxx bridge
# instead of re-running the ~35-77 min CMake build of ~500 C++ TUs.
#
# Usage:
#   scripts/ci/lbug-native-prepare.sh --from <lbug-OUT-dir> --dest <dir>
#
#   --from   a completed bundled build-script OUT dir, i.e. the directory
#            containing build/ (CMake tree) — e.g.
#            target/<profile>/build/lbug-<hash>/out  from a prior build,
#            or a dedicated one-off `cargo build` OUT dir.
#   --dest   managed native dir (recommended:
#            ~/.cache/bonsai-ci/lbug-native). Created if absent; lib/
#            entries are symlinks into --from (same filesystem required),
#            include/ entries are real copies so the dir is relocatable
#            and mountable at an identical path in the CI container.
#
# Output contract:
#   <dest>/lib/           all native static archives (liblbug + third_party)
#   <dest>/include/...    merged header roots mirroring the bundled
#                         build's -I list, minus paths that never existed
#   <dest>/env.sh         exports LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR/
#                         CXXFLAGS for host shells; paths are absolute
#                         under <dest> so the same file works inside the
#                         CI container when <dest> is bind-mounted at
#                         the identical path.
#   <dest>/manifest.json  lbug crate version, donor path, file inventory.
#
# Correctness rules:
#   - The donor MUST come from the same lbug crate version as Cargo.lock
#     (manifest records it; build-lane/run-local warn on drift).
#   - Do NOT mix env-mode and bundled-mode invocations on the same
#     CARGO_TARGET_DIR: LBUG_* env vars are not Cargo-fingerprinted, so
#     cargo cannot detect the switch. Use a dedicated lane or
#     `cargo clean -p lbug` when changing modes.
set -euo pipefail

FROM=""
DEST=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --from) FROM="$2"; shift 2 ;;
    --dest) DEST="$2"; shift 2 ;;
    *) echo "usage: $0 --from <lbug-out-dir> --dest <dir>" >&2; exit 2 ;;
  esac
done
[ -n "$FROM" ] && [ -n "$DEST" ] || { echo "both --from and --dest required" >&2; exit 2; }
[ -d "$FROM/build" ] || { echo "donor $FROM/build not found — pass the lbug OUT dir (contains build/)" >&2; exit 1; }

LBUG_SRC=""
for c in "${CARGO_HOME:-$HOME/.cargo}" /ci-cache/cargo; do
  LBUG_SRC="$(ls -d "$c"/registry/src/*/lbug-0.15.3 2>/dev/null | head -1 || true)"
  [ -n "$LBUG_SRC" ] && break
done
[ -n "$LBUG_SRC" ] && [ -d "$LBUG_SRC/lbug-src/src/include" ] \
  || { echo "lbug-0.15.3 sources not found under cargo registry" >&2; exit 1; }

mkdir -p "$DEST/lib" "$DEST/include"

# ── lib/: symlink every static archive produced by the bundled build ──
count=0
while IFS= read -r a; do
  ln -sfn "$a" "$DEST/lib/$(basename "$a")"
  count=$((count + 1))
done < <(find "$FROM/build" -name "*.a" -type f | sort)
[ "$count" -ge 15 ] || { echo "only $count archives found under $FROM/build — incomplete donor?" >&2; exit 1; }
for need in liblbug.a libre2.a libparquet.a libzstd.a libutf8proc.a; do
  [ -e "$DEST/lib/$need" ] || { echo "missing required archive $need" >&2; exit 1; }
done

# ── include/: real copies of every header root the bundled build used ─
INC="$DEST/include"
mkdir -p "$INC/lbug-src" "$INC/build-src/include" "$INC/fastpfor" "$INC/alp"
cp -a "$LBUG_SRC/lbug-src/src/include/." "$INC/lbug-src/"
mkdir -p "$INC/lbug-src-crate"
cp -a "$LBUG_SRC/include/." "$INC/lbug-src-crate/"
# build.rs always adds CARGO_MANIFEST_DIR/include itself; replicate the
# remaining roots it adds in bundled mode.
cp -a "$FROM/build/src/include/." "$INC/build-src/include/" 2>/dev/null || true
mkdir -p "$INC/build-src/extension/codegen/include"
cp -a "$FROM/build/src/extension/codegen/include/." "$INC/build-src/extension/codegen/include/" 2>/dev/null || true
cp -a "$LBUG_SRC/lbug-src/third_party/fastpfor/." "$INC/fastpfor/" 2>/dev/null || true
cp -a "$LBUG_SRC/lbug-src/third_party/alp/include/." "$INC/alp/" 2>/dev/null || true
# nlohmann_json third_party dir is absent in the vendored crate; the
# bundled -I for it is a no-op and is intentionally not replicated.

# Non-bundled mode compiles the cxx bridge WITHOUT -DLBUG_BUNDLED, so
# lbug_rs.h/lbug_arrow.h take the `#include <lbug.hpp>` branch — the
# amalgamated single-file header that upstream only generates when
# BUILD_SINGLE_FILE_HEADER=ON (our bundled donor builds it OFF). Provide
# an equivalent shim that includes exactly the granular headers the
# bundled branch uses; it lives directly under LBUG_INCLUDE_DIR so
# <lbug.hpp> resolves.
cat > "$INC/lbug-src/lbug.hpp" <<'EOF'
// Shim for lbug 0.15.3 prebuilt-native mode: mirrors the include set
// used by lbug_rs.h/lbug_arrow.h under LBUG_BUNDLED (build.rs bundled
// path). Generated by scripts/ci/lbug-native-prepare.sh.
#pragma once
#include "common/type_utils.h"
#include "common/types/int128_t.h"
#include "common/types/types.h"
#include "common/types/value/nested.h"
#include "common/types/value/node.h"
#include "common/types/value/recursive_rel.h"
#include "common/types/value/rel.h"
#include "common/types/value/value.h"
#include "main/lbug.h"
#include "storage/storage_version_info.h"
EOF

VERSION="$(grep -m1 '^version' "$LBUG_SRC/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"
cat > "$DEST/env.sh" <<EOF
# Generated by scripts/ci/lbug-native-prepare.sh — source and inspect
# before use. Paths are absolute under the dest dir; mount dest at the
# identical path inside the CI container for the vars to resolve.
export LBUG_LIBRARY_DIR="$DEST/lib"
export LBUG_INCLUDE_DIR="$INC/lbug-src"
export CXXFLAGS="-I$INC/build-src -I$INC/build-src/include -I$INC/fastpfor -I$INC/alp ${CXXFLAGS:-}"
EOF
chmod +x "$DEST/env.sh"

python3 - "$DEST" "$FROM" "$LBUG_SRC" "$VERSION" "$count" <<'PY'
import json, sys, os, hashlib
dest, frm, src, ver, n = sys.argv[1:6]
libs = sorted(os.listdir(os.path.join(dest, "lib")))
inv = hashlib.sha256("\n".join(libs).encode()).hexdigest()[:16]
json.dump({
    "lbug_version": ver,
    "donor_out": frm,
    "lbug_src": src,
    "archive_count": int(n),
    "lib_inventory_sha256_16": inv,
    "created_by": "scripts/ci/lbug-native-prepare.sh",
}, open(os.path.join(dest, "manifest.json"), "w"), indent=2)
PY

echo "lbug-native: prepared $DEST ($count archives, lbug $VERSION)"
echo "lbug-native: use via 'source $DEST/env.sh' or --lbug-native $DEST"

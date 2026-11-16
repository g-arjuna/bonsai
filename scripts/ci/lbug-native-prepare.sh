#!/usr/bin/env bash
# Assemble a prebuilt lbug/LadybugDB native artifact directory
# (LBUG_LIBRARY_DIR / LBUG_INCLUDE_DIR mode of the pinned lbug 0.15.3
# build.rs). Once prepared, every Cargo unit kind (check/build/test,
# any profile, any trusted worktree) compiles only the small cxx bridge
# instead of re-running the ~35-77 min CMake build of ~500 C++ TUs.
#
# Usage:
#   scripts/ci/lbug-native-prepare.sh --from <lbug-OUT-dir> --dest <dir>
#                                      [--materialize auto|symlink|hardlink|copy]
#   scripts/ci/lbug-native-prepare.sh --verify <dir>
#
#   --from   a completed bundled build-script OUT dir, i.e. the directory
#            containing build/ (CMake tree) — e.g.
#            target/<profile>/build/lbug-<hash>/out  from a prior build,
#            or a dedicated one-off `cargo build` OUT dir.
#   --dest   managed native dir (recommended:
#            ~/.cache/bonsai-ci/lbug-native). Created if absent.
#   --materialize
#            how lib/ entries reference the donor archives:
#              auto      hardlink when --from and --dest share a
#                        filesystem, else real copy (default)
#              symlink   symlinks into --from — cheapest, but the donor
#                        path must also exist at the identical path
#                        wherever the dir is consumed (e.g. bind-mounted
#                        into the CI container)
#              hardlink  hardlinks — survives donor deletion, requires
#                        same filesystem
#              copy      independent real copies — relocatable anywhere,
#                        costs ~2-3 GB disk
#            include/ entries are always real copies so headers are
#            relocatable regardless of mode.
#   --verify fail-closed compatibility check of a prepared dir (see
#            "Verification contract" below). Used by build-lane.sh and
#            run-local.sh before trusting --lbug-native.
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
#   <dest>/manifest.json  provenance + content inventory (see below).
#
# Manifest fields (manifest_version 2):
#   lbug_version        crate version resolved from the cargo registry
#   lbug_src            registry source dir the headers were copied from
#   donor_out           --from dir the archives were taken from
#   materialize         symlink|hardlink|copy actually used
#   cargo_profile       debug|release|ci... inferred from donor path
#   host_triple         rustc host target at prepare time
#   rustc / rustc_commit / cargo / cmake / cxx
#                       toolchain identity at prepare time
#   linkage             "static" — all archives are .a
#   cxxflags            CXXFLAGS written to env.sh
#   archive_count       number of archives in lib/
#   lib_total_bytes     sum of resolved archive sizes
#   lib_inventory_sha256
#                       sha256 over sorted "name<TAB>size<TAB>content-sha256"
#                       lines — detects donor mutation, truncation or
#                       manifest/directory drift (filename-list hashing
#                       alone cannot).
#
# Verification contract (--verify, exit 0 = trusted):
#   1. manifest.json parses and has manifest_version 2.
#   2. lbug_version matches the `lbug` entry in this repo's Cargo.lock.
#   3. host_triple matches `rustc -vV` host; rustc_commit matches the
#      current rustc commit hash (artifact ABI provenance).
#   4. Every lib/ entry exists, resolves (no broken symlink), is a
#      regular file after resolution, and the recomputed content
#      inventory hash equals lib_inventory_sha256.
#   5. materialize mode agrees with the actual lib/ entry types.
#   6. Required archives (liblbug + core third_party) are present.
#   Prints a JSON verdict {"ok": bool, "failures": [...]} on stdout and
#   exits nonzero on any failure. Bounded: O(number of archives).
#
# Correctness rules:
#   - The donor MUST come from the same lbug crate version as Cargo.lock
#     (enforced by --verify; prepare records it).
#   - Do NOT mix env-mode and bundled-mode invocations on the same
#     CARGO_TARGET_DIR: LBUG_* env vars are not Cargo-fingerprinted, so
#     cargo cannot detect the switch. build-lane.sh fails closed on a
#     recorded lane-mode mismatch; otherwise use a dedicated lane or
#     `cargo clean -p lbug` when changing modes.
#   - Donor lifecycle: the prepared dir is only as durable as its
#     materialization. symlink mode pins the donor target dir — do not
#     delete donor worktrees/targets while a symlink-mode dir is in use
#     (run-local.sh refuses symlink dirs whose targets escape the mounts
#     it creates; prefer hardlink/copy for managed caches).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

verify_dir() {
  python3 - "$1" "$REPO_ROOT" <<'PY'
import json, sys, os, hashlib, subprocess, re

dest, repo = sys.argv[1], sys.argv[2]
failures = []

def fail(msg):
    failures.append(msg)

mf_path = os.path.join(dest, "manifest.json")
if not os.path.isfile(mf_path):
    print(json.dumps({"ok": False, "failures": ["manifest.json missing"]}))
    sys.exit(1)
try:
    mf = json.load(open(mf_path))
except Exception as e:
    print(json.dumps({"ok": False, "failures": [f"manifest.json unparseable: {e}"]}))
    sys.exit(1)

if mf.get("manifest_version") != 2:
    fail("manifest_version != 2 (re-run lbug-native-prepare.sh)")

# 1. lbug version vs Cargo.lock
lock = os.path.join(repo, "Cargo.lock")
try:
    txt = open(lock).read()
    m = re.search(r'name = "lbug"\s*\nversion = "([^"]+)"', txt)
    lock_ver = m.group(1) if m else None
except Exception:
    lock_ver = None
if lock_ver and mf.get("lbug_version") != lock_ver:
    fail(f"lbug_version {mf.get('lbug_version')} != Cargo.lock {lock_ver}")
elif not lock_ver:
    fail("could not resolve lbug version from Cargo.lock")

# 2. toolchain/ABI provenance
try:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True,
                         timeout=30).stdout
    kv = dict(l.split(": ", 1) for l in out.splitlines() if ": " in l)
    if mf.get("host_triple") and mf["host_triple"] != kv.get("host"):
        fail(f"host_triple {mf.get('host_triple')} != rustc host {kv.get('host')}")
    if mf.get("rustc_commit") and mf["rustc_commit"] != kv.get("commit-hash"):
        fail(f"rustc_commit {mf.get('rustc_commit')} != current {kv.get('commit-hash')}")
except Exception as e:
    fail(f"rustc -vV probe failed: {e}")

# 3. archive inventory: existence, resolution, content hash
libdir = os.path.join(dest, "lib")
entries = []
try:
    names = sorted(os.listdir(libdir))
except Exception as e:
    fail(f"lib/ unreadable: {e}")
    names = []
lines = []
total = 0
resolved_outside = []
for n in names:
    p = os.path.join(libdir, n)
    if os.path.islink(p):
        tgt = os.path.realpath(p)
        if not os.path.isfile(tgt):
            fail(f"broken/unresolvable link lib/{n} -> {os.readlink(p)}")
            continue
        entries.append("symlink")
        resolved_outside.append(tgt)
    elif os.path.isfile(p):
        entries.append("file")
        tgt = p
    else:
        fail(f"lib/{n} is neither file nor symlink")
        continue
    try:
        h = hashlib.sha256()
        sz = 0
        with open(tgt, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk); sz += len(chunk)
        lines.append(f"{n}\t{sz}\t{h.hexdigest()}")
        total += sz
    except Exception as e:
        fail(f"cannot hash lib/{n}: {e}")

if mf.get("archive_count") is not None and len(names) != mf["archive_count"]:
    fail(f"archive_count manifest={mf['archive_count']} actual={len(names)}")
if mf.get("lib_total_bytes") is not None and total != mf["lib_total_bytes"]:
    fail(f"lib_total_bytes manifest={mf['lib_total_bytes']} actual={total}")
inv = hashlib.sha256("\n".join(sorted(lines)).encode()).hexdigest()
if mf.get("lib_inventory_sha256") and inv != mf["lib_inventory_sha256"]:
    fail("lib inventory content hash mismatch (donor mutated or dir drift)")

# 4. materialize mode agrees with entry types
mode = mf.get("materialize")
if mode == "symlink" and entries and any(e != "symlink" for e in entries):
    fail("manifest says symlink but non-symlink entries exist in lib/")
if mode in ("hardlink", "copy") and entries and any(e == "symlink" for e in entries):
    fail(f"manifest says {mode} but symlinks exist in lib/")
if mode is None:
    fail("manifest missing materialize mode")

# 5. required archives
for need in ("liblbug.a", "libre2.a", "libparquet.a", "libzstd.a", "libutf8proc.a"):
    if need not in names:
        fail(f"missing required archive {need}")

# 6. env.sh present and consistent
if not os.path.isfile(os.path.join(dest, "env.sh")):
    fail("env.sh missing")

ok = not failures
out = {"ok": ok, "dest": dest, "failures": failures,
       "archive_count": len(names), "lib_total_bytes": total,
       "lbug_version": mf.get("lbug_version"),
       "materialize": mode,
       "symlink_targets_outside_dest": sorted({os.path.dirname(t) for t in resolved_outside})}
print(json.dumps(out))
sys.exit(0 if ok else 1)
PY
}

if [ "${1:-}" = "--verify" ]; then
  [ "$#" -eq 2 ] || { echo "usage: $0 --verify <dir>" >&2; exit 2; }
  verify_dir "$2"
  exit $?
fi

FROM=""
DEST=""
MATERIALIZE="auto"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --from) FROM="$2"; shift 2 ;;
    --dest) DEST="$2"; shift 2 ;;
    --materialize) MATERIALIZE="$2"; shift 2 ;;
    *) echo "usage: $0 --from <lbug-out-dir> --dest <dir> [--materialize auto|symlink|hardlink|copy]" >&2; exit 2 ;;
  esac
done
[ -n "$FROM" ] && [ -n "$DEST" ] || { echo "both --from and --dest required" >&2; exit 2; }
case "$MATERIALIZE" in auto|symlink|hardlink|copy) ;; *) echo "bad --materialize $MATERIALIZE" >&2; exit 2 ;; esac
[ -d "$FROM/build" ] || { echo "donor $FROM/build not found — pass the lbug OUT dir (contains build/)" >&2; exit 1; }

LBUG_SRC=""
for c in "${CARGO_HOME:-$HOME/.cargo}" /ci-cache/cargo; do
  LBUG_SRC="$(ls -d "$c"/registry/src/*/lbug-0.15.3 2>/dev/null | head -1 || true)"
  [ -n "$LBUG_SRC" ] && break
done
[ -n "$LBUG_SRC" ] && [ -d "$LBUG_SRC/lbug-src/src/include" ] \
  || { echo "lbug-0.15.3 sources not found under cargo registry" >&2; exit 1; }

mkdir -p "$DEST/lib" "$DEST/include"

# ── lib/: materialize every static archive produced by the bundled build ──
# Resolve the effective mode once: auto = hardlink, falling back to copy
# when donor and dest are on different filesystems.
MODE="$MATERIALIZE"
if [ "$MODE" = "auto" ]; then
  MODE="hardlink"
  probe_src="$(find "$FROM/build" -name '*.a' -type f | head -1)"
  if [ -n "$probe_src" ] && [ "$(stat -c %d "$probe_src")" != "$(stat -c %d "$DEST")" ]; then
    MODE="copy"
    echo "lbug-native: donor/dest on different filesystems — materializing copies" >&2
  fi
fi

count=0
while IFS= read -r a; do
  name="$(basename "$a")"
  case "$MODE" in
    symlink)  ln -sfn "$a" "$DEST/lib/$name" ;;
    hardlink) ln -f "$a" "$DEST/lib/$name" 2>/dev/null || cp -f "$a" "$DEST/lib/$name" ;;
    copy)     cp -f "$a" "$DEST/lib/$name" ;;
  esac
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
CXXFLAGS_OUT="-I$INC/build-src -I$INC/build-src/include -I$INC/fastpfor -I$INC/alp ${CXXFLAGS:-}"
cat > "$DEST/env.sh" <<EOF
# Generated by scripts/ci/lbug-native-prepare.sh — source and inspect
# before use. Paths are absolute under $DEST; mount dest at the
# identical path inside the CI container for the vars to resolve.
export LBUG_LIBRARY_DIR="$DEST/lib"
export LBUG_INCLUDE_DIR="$INC/lbug-src"
export CXXFLAGS="$CXXFLAGS_OUT"
EOF
chmod +x "$DEST/env.sh"

# Infer the cargo profile from the donor path (target/<profile>/build/...).
PROFILE="unknown"
if [[ "$FROM" =~ /target/([^/]+)/build/ ]]; then PROFILE="${BASH_REMATCH[1]}"; fi

RUSTC_VV="$(rustc -vV 2>/dev/null || true)"
RUSTC_REL="$(printf '%s\n' "$RUSTC_VV" | awk '/^release:/{print $2}')"
RUSTC_COMMIT="$(printf '%s\n' "$RUSTC_VV" | awk '/^commit-hash:/{print $2}')"
HOST_TRIPLE="$(printf '%s\n' "$RUSTC_VV" | awk '/^host:/{print $2}')"
CARGO_V="$(cargo --version 2>/dev/null | awk '{print $2}')"
CMAKE_V="$(cmake --version 2>/dev/null | awk 'NR==1{print $3}')"
CXX_V="$(c++ --version 2>/dev/null | awk 'NR==1{print $4}' | tr -d ')')"
NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Content inventory: hash resolved archive bytes, not just filenames.
python3 - "$DEST" "$FROM" "$LBUG_SRC" "$VERSION" "$count" "$MODE" \
        "$PROFILE" "$RUSTC_REL" "$RUSTC_COMMIT" "$HOST_TRIPLE" \
        "$CARGO_V" "$CMAKE_V" "$CXX_V" "$NOW" "$CXXFLAGS_OUT" <<'PY'
import json, sys, os, hashlib
(dest, frm, src, ver, n, mode, profile, rustc, commit, triple,
 cargo, cmake, cxx, now, cxxflags) = sys.argv[1:16]
libdir = os.path.join(dest, "lib")
lines, total = [], 0
for name in sorted(os.listdir(libdir)):
    p = os.path.join(libdir, name)
    tgt = os.path.realpath(p) if os.path.islink(p) else p
    if not os.path.isfile(tgt):
        continue
    h = hashlib.sha256(); sz = 0
    with open(tgt, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk); sz += len(chunk)
    lines.append(f"{name}\t{sz}\t{h.hexdigest()}")
    total += sz
inv = hashlib.sha256("\n".join(lines).encode()).hexdigest()
json.dump({
    "manifest_version": 2,
    "created_utc": now,
    "created_by": "scripts/ci/lbug-native-prepare.sh",
    "lbug_version": ver,
    "lbug_src": src,
    "donor_out": frm,
    "materialize": mode,
    "cargo_profile": profile,
    "host_triple": triple,
    "rustc": rustc,
    "rustc_commit": commit,
    "cargo": cargo,
    "cmake": cmake,
    "cxx": cxx,
    "linkage": "static",
    "cxxflags": cxxflags,
    "archive_count": int(n),
    "lib_total_bytes": total,
    "lib_inventory_sha256": inv,
}, open(os.path.join(dest, "manifest.json"), "w"), indent=2)
PY

# Self-check before reporting success.
if ! verify_dir "$DEST" >/dev/null; then
  echo "lbug-native: prepared $DEST but self-verify FAILED:" >&2
  verify_dir "$DEST" >&2 || true
  exit 1
fi

echo "lbug-native: prepared $DEST ($count archives, $MODE, lbug $VERSION, profile $PROFILE)"
echo "lbug-native: verify with '$0 --verify $DEST'; use via 'source $DEST/env.sh' or --lbug-native $DEST"

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
#                                      [--replace]
#   scripts/ci/lbug-native-prepare.sh --verify <dir> [--gate-env]
#
#   --from   a completed bundled build-script OUT dir, i.e. the directory
#            containing build/ (CMake tree) — e.g.
#            target/<profile>/build/lbug-<hash>/out  from a prior build,
#            or a dedicated one-off `cargo build` OUT dir.
#   --dest   managed native dir (recommended:
#            ~/.cache/bonsai-ci/lbug-native). Created if absent.
#            If it already contains a manifest.json, --replace is
#            required to republish over it.
#   --materialize
#            how lib/ entries reference the donor archives:
#              auto      hardlink when --from and --dest share a
#                        filesystem, else real copy (default)
#              symlink   symlinks into --from — cheapest, but the donor
#                        path must also exist at the identical path
#                        wherever the dir is consumed (e.g. bind-mounted
#                        into the CI container)
#              hardlink  hardlinks — survives donor deletion (the bundle
#                        keeps its own directory entries to the same
#                        inodes), requires same filesystem
#              copy      independent real copies — relocatable anywhere,
#                        costs ~2-3 GB disk
#            include/ entries are always real copies so headers are
#            relocatable regardless of mode.
#   --replace allow republishing over an existing prepared --dest.
#            Publication is atomic (staged dir + rename) and serialized
#            against consumers by an exclusive flock on <dest>.lock —
#            build-lane.sh/run-local.sh hold the shared side while a
#            bundle is in use, so replacement never mutates archives a
#            running build is reading. Hardlinked inodes are never
#            written in place: staging writes only fresh files.
#   --verify fail-closed compatibility check of a prepared dir (see
#            "Verification contract" below). Used by build-lane.sh and
#            run-local.sh before trusting --lbug-native.
#   --gate-env  verify mode for the container gate environment: enforces
#            bundle integrity, identity and ABI provenance but does not
#            require the *preparing* host's rustc/cmake/cxx versions to
#            equal the gate toolchain (the C++ archives are toolchain-
#            independent; the cxx bridge is compiled fresh by whichever
#            rustc runs cargo). host_triple is ALWAYS enforced.
#
# Output contract:
#   <dest>/lib/           all native static archives (liblbug + third_party)
#   <dest>/include/...    merged header roots mirroring the bundled
#                         build's -I list, minus paths that never existed
#   <dest>/env.sh         exports LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR/
#                         CXXFLAGS for host shells; convenience only —
#                         trusted consumers read manifest.json directly
#   <dest>/manifest.json  provenance + content inventory (see below)
#
# Manifest fields (manifest_version 4 — ALL mandatory for --verify):
#   bundle_id            sha256 content identity over the full bundle
#                        inventory (archives + headers + shim), hex
#   lbug_version         crate version resolved from the cargo registry
#   lbug_crate_sha256    Cargo.lock checksum for the lbug crate — pins
#                        the exact crate source the donor was built from
#   lbug_src             registry source dir headers were copied from
#   donor_out            --from dir the archives were taken from
#   donor_provenance     object recorded from the donor CMake build:
#                        cmake_cxx_compiler, cmake_build_type,
#                        cmake_cxx_flags, cmake_generator, cxx_dumpmachine,
#                        libc_version — the ABI the archives were ACTUALLY
#                        compiled with, not just the prepare-time toolchain
#   materialize          symlink|hardlink|copy actually used
#   cargo_profile        debug|release|ci... inferred from donor path
#   dest_path            absolute path the bundle was published at
#   host_triple          rustc host target at prepare time
#   rustc / rustc_commit / cargo / cmake / cxx
#                       toolchain identity at prepare time
#   linkage              "static" — all archives are .a
#   cxxflags             generated CXXFLAGS written to env.sh (include
#                        roots only; consumer CXXFLAGS are appended by
#                        the consumer, not baked in)
#   archive_count        number of archives in lib/ (must be > 0)
#   lib_total_bytes      sum of resolved archive sizes (must be > 0)
#   lib_inventory_sha256 sha256 over sorted "name<TAB>size<TAB>content-sha256"
#                        lines of resolved archives
#   include_inventory_sha256
#                       sha256 over sorted "relpath<TAB>size<TAB>sha256"
#                        lines of every file under include/ — covers
#                        copied/generated headers and the lbug.hpp shim
#
# Verification contract (--verify, exit 0 = trusted):
#   1. manifest.json parses; manifest_version == 3; EVERY mandatory
#      field above is present, non-empty and well-formed. An absent
#      hash is a failure, never a skipped check.
#   2. lbug_version AND lbug_crate_sha256 match this repo's Cargo.lock.
#   3. host_triple matches `rustc -vV` host (always). rustc_commit
#      matches current rustc unless --gate-env. --gate-env ADDITIONALLY
#      probes the live environment's native ABI: c++ target triple,
#      libc identity and C++ compiler major.minor must match the donor
#      provenance — recorded metadata alone is not accepted as proof.
#   4. Recomputed lib AND include inventories equal the manifest
#      hashes; bundle_id recomputes equal. Every lib/ entry exists,
#      resolves (no broken symlink) and is a regular file.
#   5. materialize mode agrees with the actual lib/ entry types.
#   6. Required archives (liblbug + core third_party) are present.
#   7. dest_path equals the verified dir's realpath; env.sh exists and
#      its exports match the manifest/dir (guards stale env.sh).
#   Bounded: O(number of archives + headers).
#
# Correctness rules:
#   - The donor MUST come from the same lbug crate version as Cargo.lock
#     (enforced by --verify; prepare records it).
#   - Do NOT mix env-mode and bundled-mode invocations on the same
#     CARGO_TARGET_DIR: LBUG_* env vars are not Cargo-fingerprinted, so
#     cargo cannot detect the switch. build-lane.sh fails closed on a
#     recorded lane-mode/fingerprint mismatch; otherwise use a dedicated
#     lane or `cargo clean -p lbug` (--reset-lbug-artifacts) when
#     changing modes or native bundles.
#   - Donor lifecycle: the prepared dir is only as durable as its
#     materialization. symlink mode pins the donor target dir — do not
#     delete donor worktrees/targets while a symlink-mode dir is in use
#     (run-local.sh refuses symlink dirs whose targets escape the mounts
#     it creates; prefer hardlink/copy for managed caches). hardlink and
#     copy modes are donor-independent: deleting the donor is safe.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

verify_dir() {
  python3 - "$1" "$REPO_ROOT" "$2" <<'PY'
import json, sys, os, hashlib, subprocess, re

dest, repo, mode_arg = sys.argv[1], sys.argv[2], sys.argv[3]
gate_env = mode_arg == "gate"
stage_mode = os.environ.get("LBUG_VERIFY_STAGE") == "1"
failures = []

def fail(msg):
    failures.append(msg)

def is_hex64(v):
    return isinstance(v, str) and re.fullmatch(r"[0-9a-f]{64}", v) is not None

mf_path = os.path.join(dest, "manifest.json")
if not os.path.isfile(mf_path):
    print(json.dumps({"ok": False, "failures": ["manifest.json missing"]}))
    sys.exit(1)
try:
    mf = json.load(open(mf_path))
except Exception as e:
    print(json.dumps({"ok": False, "failures": [f"manifest.json unparseable: {e}"]}))
    sys.exit(1)
if not isinstance(mf, dict):
    print(json.dumps({"ok": False, "failures": ["manifest.json is not an object"]}))
    sys.exit(1)

# ── 1. mandatory fields: present, non-empty, well-formed ──────────────
MANDATORY_STR = ["bundle_id", "lbug_version", "lbug_crate_sha256", "lbug_src",
                 "donor_out", "materialize", "cargo_profile", "dest_path",
                 "host_triple", "rustc", "rustc_commit", "cargo", "cmake",
                 "cxx", "linkage", "cxxflags", "lib_inventory_sha256",
                 "include_inventory_sha256"]
for k in MANDATORY_STR:
    v = mf.get(k)
    if not isinstance(v, str) or not v.strip():
        fail(f"mandatory field {k} missing/empty")
for k in ("archive_count", "lib_total_bytes"):
    v = mf.get(k)
    if not isinstance(v, int) or isinstance(v, bool) or v <= 0:
        fail(f"mandatory field {k} missing/not a positive integer")
for k in ("bundle_id", "lib_inventory_sha256", "include_inventory_sha256",
          "lbug_crate_sha256"):
    v = mf.get(k)
    if isinstance(v, str) and v.strip() and not is_hex64(v):
        fail(f"field {k} is not a sha256 hex digest")
if mf.get("manifest_version") != 4:
    fail("manifest_version != 4 (re-run lbug-native-prepare.sh)")
if mf.get("linkage") not in (None, "") and mf.get("linkage") != "static":
    fail(f"linkage {mf.get('linkage')!r} != 'static'")
if mf.get("materialize") not in (None, "") and mf.get("materialize") not in ("symlink", "hardlink", "copy"):
    fail(f"materialize {mf.get('materialize')!r} unknown")
dp = mf.get("donor_provenance")
if not isinstance(dp, dict):
    fail("donor_provenance missing/not an object")
else:
    for k in ("cmake_cxx_compiler", "cmake_cxx_version", "cmake_build_type",
              "cmake_generator", "cxx_dumpmachine", "libc_version"):
        if not isinstance(dp.get(k), str) or not dp[k].strip():
            fail(f"donor_provenance.{k} missing/empty")
    if not isinstance(dp.get("cmake_cxx_flags"), str):
        fail("donor_provenance.cmake_cxx_flags missing/not a string")

# ── 2. lbug version + crate source checksum vs Cargo.lock ─────────────
lock = os.path.join(repo, "Cargo.lock")
lock_ver = lock_sha = None
try:
    txt = open(lock).read()
    m = re.search(r'name = "lbug"\s*\nversion = "([^"]+)"(?:[^[]*?)checksum = "([0-9a-f]{64})"', txt)
    if m:
        lock_ver, lock_sha = m.group(1), m.group(2)
except Exception:
    pass
if not lock_ver:
    fail("could not resolve lbug version+checksum from Cargo.lock")
else:
    if mf.get("lbug_version") != lock_ver:
        fail(f"lbug_version {mf.get('lbug_version')} != Cargo.lock {lock_ver}")
    if mf.get("lbug_crate_sha256") != lock_sha:
        fail("lbug_crate_sha256 != Cargo.lock checksum (crate source identity)")

# ── 3. toolchain/ABI provenance ───────────────────────────────────────
try:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True,
                         timeout=30).stdout
    kv = dict(l.split(": ", 1) for l in out.splitlines() if ": " in l)
    if mf.get("host_triple") and mf["host_triple"] != kv.get("host"):
        fail(f"host_triple {mf.get('host_triple')} != rustc host {kv.get('host')}")
    if not gate_env:
        if mf.get("rustc_commit") and mf["rustc_commit"] != kv.get("commit-hash"):
            fail(f"rustc_commit {mf.get('rustc_commit')} != current {kv.get('commit-hash')}")
except Exception as e:
    fail(f"rustc -vV probe failed: {e}")

# ── 3b. gate-env: probe ACTUAL native ABI compatibility ───────────────
# Recorded provenance alone does not establish that these archives can
# link in the gate container. Probe the LIVE toolchain: compiler target
# triple, libc identity and C++ compiler version must match the donor
# build or the static archives/bridge may be ABI-incompatible. The
# definitive proof remains the linked-runtime acceptance test; this
# check rejects obviously incompatible donors before any build starts.
if gate_env and isinstance(dp, dict):
    def _probe(cmd):
        try:
            return subprocess.run(cmd, capture_output=True, text=True,
                                  timeout=30).stdout
        except Exception:
            return ""
    cur_dump = (_probe(["c++", "-dumpmachine"]).strip()
                or _probe(["cc", "-dumpmachine"]).strip())
    if not cur_dump:
        fail("gate-env ABI: cannot probe c++ -dumpmachine")
    elif dp.get("cxx_dumpmachine") and cur_dump != dp["cxx_dumpmachine"]:
        fail(f"gate-env ABI: compiler target {cur_dump} != donor {dp['cxx_dumpmachine']}")
    libc_lines = _probe(["ldd", "--version"]).splitlines()
    cur_libc = libc_lines[0].strip() if libc_lines else ""
    if not cur_libc:
        fail("gate-env ABI: cannot probe ldd --version")
    elif dp.get("libc_version") and cur_libc != dp["libc_version"]:
        fail(f"gate-env ABI: libc '{cur_libc}' != donor '{dp['libc_version']}'")
    def _mm(ver):
        m = re.search(r"(\d+)\.(\d+)", ver or "")
        return f"{m.group(1)}.{m.group(2)}" if m else ""
    cur_cxxv = ""
    for line in _probe(["c++", "--version"]).splitlines():
        if line.strip():
            f = line.split()
            cur_cxxv = f[3] if len(f) > 3 else f[-1]
            break
    donor_mm = _mm(dp.get("cmake_cxx_version"))
    cur_mm = _mm(cur_cxxv)
    if not donor_mm or not cur_mm:
        fail("gate-env ABI: cannot determine C++ compiler version")
    elif donor_mm != cur_mm:
        fail(f"gate-env ABI: C++ {cur_mm} != donor compiler {donor_mm} "
             f"({dp.get('cmake_cxx_version')}) — archives may be ABI-incompatible")

# ── 4. dest_path must equal the verified dir's realpath ───────────────
# (skipped for staging self-verify: manifest already records the final
#  publish path while the files still sit in the staging dir)
if not stage_mode:
    try:
        if mf.get("dest_path") and os.path.realpath(dest) != os.path.realpath(mf["dest_path"]):
            fail(f"dest_path {mf.get('dest_path')} != verified dir {os.path.realpath(dest)}")
    except Exception:
        pass

# ── 5. archive + header inventories (full bundle integrity) ───────────
libdir = os.path.join(dest, "lib")
incdir = os.path.join(dest, "include")
entries = []
lines = []
total = 0
try:
    names = sorted(os.listdir(libdir))
except Exception as e:
    fail(f"lib/ unreadable: {e}")
    names = []
for n in names:
    p = os.path.join(libdir, n)
    if os.path.islink(p):
        tgt = os.path.realpath(p)
        if not os.path.isfile(tgt):
            fail(f"broken/unresolvable link lib/{n} -> {os.readlink(p)}")
            continue
        entries.append("symlink")
    elif os.path.isfile(p):
        entries.append("file")
        tgt = p
    else:
        fail(f"lib/{n} is neither file nor symlink")
        continue
    try:
        h = hashlib.sha256(); sz = 0
        with open(tgt, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk); sz += len(chunk)
        lines.append(f"{n}\t{sz}\t{h.hexdigest()}")
        total += sz
    except Exception as e:
        fail(f"cannot hash lib/{n}: {e}")

ilines = []
try:
    for root, _dirs, files in os.walk(incdir):
        for fn in sorted(files):
            p = os.path.join(root, fn)
            rel = os.path.relpath(p, incdir)
            try:
                h = hashlib.sha256(); sz = 0
                with open(p, "rb") as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk); sz += len(chunk)
                ilines.append(f"{rel}\t{sz}\t{h.hexdigest()}")
            except Exception as e:
                fail(f"cannot hash include/{rel}: {e}")
except Exception as e:
    fail(f"include/ unreadable: {e}")

if mf.get("archive_count") is not None and len(names) != mf["archive_count"]:
    fail(f"archive_count manifest={mf['archive_count']} actual={len(names)}")
if mf.get("lib_total_bytes") is not None and total != mf["lib_total_bytes"]:
    fail(f"lib_total_bytes manifest={mf['lib_total_bytes']} actual={total}")
inv = hashlib.sha256("\n".join(sorted(lines)).encode()).hexdigest()
if is_hex64(mf.get("lib_inventory_sha256") or "") and inv != mf["lib_inventory_sha256"]:
    fail("lib inventory content hash mismatch (donor mutated or dir drift)")
iinv = hashlib.sha256("\n".join(sorted(ilines)).encode()).hexdigest()
if is_hex64(mf.get("include_inventory_sha256") or "") and iinv != mf["include_inventory_sha256"]:
    fail("include inventory content hash mismatch (headers mutated)")
# bundle_id v4: content inventories PLUS effective build identity —
# donor ABI provenance, bridge CXXFLAGS roots and linkage. Two bundles
# with identical archive/header bytes but different build provenance
# MUST NOT share an identity.
prov_lines = [f"{k}={v}" for k, v in sorted(dp.items())] if isinstance(dp, dict) else []
prov_lines += [f"cxxflags={mf.get('cxxflags') or ''}",
               f"linkage={mf.get('linkage') or ''}"]
prov_hash = hashlib.sha256("\n".join(sorted(prov_lines)).encode()).hexdigest()
bundle = hashlib.sha256(f"{mf.get('lbug_version')}|{inv}|{iinv}|{prov_hash}".encode()).hexdigest()
if is_hex64(mf.get("bundle_id") or "") and bundle != mf["bundle_id"]:
    fail("bundle_id mismatch (content/provenance identity drift)")

# ── 6. materialize mode agrees with entry types ───────────────────────
mode = mf.get("materialize")
if mode == "symlink" and entries and any(e != "symlink" for e in entries):
    fail("manifest says symlink but non-symlink entries exist in lib/")
if mode in ("hardlink", "copy") and entries and any(e == "symlink" for e in entries):
    fail(f"manifest says {mode} but symlinks exist in lib/")

# ── 7. required archives ──────────────────────────────────────────────
for need in ("liblbug.a", "libre2.a", "libparquet.a", "libzstd.a", "libutf8proc.a"):
    if need not in names:
        fail(f"missing required archive {need}")

# ── 8. env.sh exists and is consistent with manifest/dest ─────────────
env_path = os.path.join(dest, "env.sh")
if not os.path.isfile(env_path):
    fail("env.sh missing")
else:
    try:
        etext = open(env_path).read()
        env_dest = mf.get("dest_path") or dest
        exp_lib = f'export LBUG_LIBRARY_DIR="{os.path.join(env_dest, "lib")}"'
        exp_inc = f'export LBUG_INCLUDE_DIR="{os.path.join(env_dest, "include", "lbug-src")}"'
        exp_cxx = f'export CXXFLAGS="{mf.get("cxxflags", "")}"'
        for label, needle in (("LBUG_LIBRARY_DIR", exp_lib),
                              ("LBUG_INCLUDE_DIR", exp_inc),
                              ("CXXFLAGS", exp_cxx)):
            if needle not in etext:
                fail(f"env.sh {label} inconsistent with manifest/dest (stale env.sh?)")
    except Exception as e:
        fail(f"env.sh unreadable: {e}")

ok = not failures
out = {"ok": ok, "dest": dest, "failures": failures,
       "archive_count": len(names), "lib_total_bytes": total,
       "include_files": len(ilines),
       "bundle_id": mf.get("bundle_id"),
       "lbug_version": mf.get("lbug_version"),
       "materialize": mode,
       "gate_env": gate_env,
       "symlink_targets_outside_dest": sorted({os.path.dirname(os.path.realpath(os.path.join(libdir, n))) for n in names if os.path.islink(os.path.join(libdir, n))})}
print(json.dumps(out))
sys.exit(0 if ok else 1)
PY
}

if [ "${1:-}" = "--verify" ]; then
  GATE_ENV=0
  shift
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --gate-env) GATE_ENV=1; shift ;;
      *) DEST_ARG="$1"; shift ;;
    esac
  done
  [ -n "${DEST_ARG:-}" ] || { echo "usage: $0 --verify <dir> [--gate-env]" >&2; exit 2; }
  [ "$GATE_ENV" = "1" ] && verify_dir "$DEST_ARG" gate || verify_dir "$DEST_ARG" strict
  exit $?
fi

FROM=""
DEST=""
MATERIALIZE="auto"
REPLACE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --from) FROM="$2"; shift 2 ;;
    --dest) DEST="$2"; shift 2 ;;
    --materialize) MATERIALIZE="$2"; shift 2 ;;
    --replace) REPLACE=1; shift ;;
    *) echo "usage: $0 --from <lbug-out-dir> --dest <dir> [--materialize auto|symlink|hardlink|copy] [--replace]" >&2; exit 2 ;;
  esac
done
[ -n "$FROM" ] && [ -n "$DEST" ] || { echo "both --from and --dest required" >&2; exit 2; }
case "$MATERIALIZE" in auto|symlink|hardlink|copy) ;; *) echo "bad --materialize $MATERIALIZE" >&2; exit 2 ;; esac
[ -d "$FROM/build" ] || { echo "donor $FROM/build not found — pass the lbug OUT dir (contains build/)" >&2; exit 1; }
# Canonicalize DEST (physical path) so the publication lock, manifest
# dest_path and every consumer's <dest>.lock resolve to the same inode
# even when the caller passes a symlinked path.
DEST="$(cd "$(dirname "$DEST")" 2>/dev/null && pwd -P)/$(basename "$DEST")"
mkdir -p "$(dirname "$DEST")"

# ── atomic publication lock: exclusive vs consumers' shared flock ─────
# Consumers (build-lane.sh / run-local.sh) hold `flock -s` on
# <dest>.lock for the whole build; prepare holds `flock -x` so a bundle
# is never replaced while archives are being read.
exec 8>"$DEST.lock"
echo "lbug-native: waiting for bundle lock $DEST.lock" >&2
flock 8

if [ -f "$DEST/manifest.json" ] && [ "$REPLACE" != "1" ]; then
  echo "lbug-native: $DEST already prepared — pass --replace to republish" >&2
  exit 1
fi

# Remove abandoned publication leftovers (.old.<pid> retired bundles,
# .staging.<pid> staging dirs) from crashed prepares. The exclusive
# flock held here guarantees no live prepare or consumer can still
# reference them — consumers only ever use the canonical $DEST path.
for stale in "$DEST".old.* "$DEST".staging.*; do
  if [ -e "$stale" ] || [ -L "$stale" ]; then
    echo "lbug-native: removing abandoned $stale" >&2
    rm -rf "$stale"
  fi
done

LBUG_SRC=""
for c in "${CARGO_HOME:-$HOME/.cargo}" /ci-cache/cargo; do
  LBUG_SRC="$(ls -d "$c"/registry/src/*/lbug-0.15.3 2>/dev/null | head -1 || true)"
  [ -n "$LBUG_SRC" ] && break
done
[ -n "$LBUG_SRC" ] && [ -d "$LBUG_SRC/lbug-src/src/include" ] \
  || { echo "lbug-0.15.3 sources not found under cargo registry" >&2; exit 1; }

# ── stage the bundle: never write into a live dest ────────────────────
STAGE="$DEST.staging.$$"
OLD=""
cleanup_stage() { rm -rf "$STAGE"; [ -n "$OLD" ] && rm -rf "$OLD"; }
trap cleanup_stage EXIT
rm -rf "$STAGE"
mkdir -p "$STAGE/lib" "$STAGE/include"

# ── donor provenance: record what the donor was ACTUALLY compiled with ──
CMAKE_CACHE="$(find "$FROM/build" -name CMakeCache.txt | head -1)"
[ -n "$CMAKE_CACHE" ] || { echo "no CMakeCache.txt under $FROM/build — incomplete donor?" >&2; exit 1; }
cmake_get() { grep -m1 "^$1:" "$CMAKE_CACHE" | sed 's/^[^:]*:[A-Z]*=//'; }
DP_COMPILER="$(cmake_get CMAKE_CXX_COMPILER)"
DP_BUILD_TYPE="$(cmake_get CMAKE_BUILD_TYPE)"
DP_CXXFLAGS="$(cmake_get CMAKE_CXX_FLAGS)"
DP_GENERATOR="$(cmake_get CMAKE_GENERATOR)"
[ -n "$DP_COMPILER" ] && [ -n "$DP_BUILD_TYPE" ] \
  || { echo "CMakeCache missing CMAKE_CXX_COMPILER/CMAKE_BUILD_TYPE" >&2; exit 1; }
DP_DUMPMACHINE="$("$DP_COMPILER" -dumpmachine 2>/dev/null || echo unknown)"
DP_CXX_VERSION="$("$DP_COMPILER" --version 2>/dev/null | awk 'NR==1{print $4}' | tr -d ')' || echo unknown)"
DP_LIBC="$(ldd --version 2>/dev/null | sed -n '1p')"

# ── lib/: materialize every static archive produced by the bundled build ──
MODE="$MATERIALIZE"
if [ "$MODE" = "auto" ]; then
  MODE="hardlink"
  probe_src="$(find "$FROM/build" -name '*.a' -type f | head -1)"
  if [ -n "$probe_src" ] && [ "$(stat -c %d "$probe_src")" != "$(stat -c %d "$STAGE")" ]; then
    MODE="copy"
    echo "lbug-native: donor/dest on different filesystems — materializing copies" >&2
  fi
fi

count=0
while IFS= read -r a; do
  name="$(basename "$a")"
  case "$MODE" in
    symlink)  ln -s "$a" "$STAGE/lib/$name" ;;
    hardlink) ln "$a" "$STAGE/lib/$name" 2>/dev/null || cp "$a" "$STAGE/lib/$name" ;;
    copy)     cp "$a" "$STAGE/lib/$name" ;;
  esac
  count=$((count + 1))
done < <(find "$FROM/build" -name "*.a" -type f | sort)
[ "$count" -ge 15 ] || { echo "only $count archives found under $FROM/build — incomplete donor?" >&2; exit 1; }
for need in liblbug.a libre2.a libparquet.a libzstd.a libutf8proc.a; do
  [ -e "$STAGE/lib/$need" ] || { echo "missing required archive $need" >&2; exit 1; }
done

# ── include/: real copies of every header root the bundled build used ─
INC="$STAGE/include"
mkdir -p "$INC/lbug-src" "$INC/build-src/include" "$INC/fastpfor" "$INC/alp"
cp -a "$LBUG_SRC/lbug-src/src/include/." "$INC/lbug-src/"
mkdir -p "$INC/lbug-src-crate"
cp -a "$LBUG_SRC/include/." "$INC/lbug-src-crate/"
cp -a "$FROM/build/src/include/." "$INC/build-src/include/" 2>/dev/null || true
mkdir -p "$INC/build-src/extension/codegen/include"
cp -a "$FROM/build/src/extension/codegen/include/." "$INC/build-src/extension/codegen/include/" 2>/dev/null || true
cp -a "$LBUG_SRC/lbug-src/third_party/fastpfor/." "$INC/fastpfor/" 2>/dev/null || true
cp -a "$LBUG_SRC/lbug-src/third_party/alp/include/." "$INC/alp/" 2>/dev/null || true

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
# env.sh/cxxflags reference the FINAL publish path, not the staging dir.
CXXFLAGS_OUT="-I$DEST/include/build-src -I$DEST/include/build-src/include -I$DEST/include/fastpfor -I$DEST/include/alp"
# env.sh is a convenience for humans; trusted consumers read
# manifest.json (cxxflags + paths) via build-lane-guard.sh env.
cat > "$STAGE/env.sh" <<EOF
# Generated by scripts/ci/lbug-native-prepare.sh — convenience only.
# Trusted consumers (build-lane.sh, run-local.sh) read manifest.json
# instead of sourcing this file. Paths are absolute under $DEST.
export LBUG_LIBRARY_DIR="$DEST/lib"
export LBUG_INCLUDE_DIR="$DEST/include/lbug-src"
export CXXFLAGS="$CXXFLAGS_OUT"
EOF
chmod +x "$STAGE/env.sh"

# ── provenance + inventories + manifest (all inside staging) ──────────
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
LOCK_SHA="$(python3 - "$REPO_ROOT/Cargo.lock" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
m = re.search(r'name = "lbug"\s*\nversion = "([^"]+)"(?:[^[]*?)checksum = "([0-9a-f]{64})"', t)
print(m.group(2) if m else "")
PY
)"
[ -n "$LOCK_SHA" ] || { echo "lbug checksum not found in Cargo.lock" >&2; exit 1; }

python3 - "$STAGE" "$FROM" "$LBUG_SRC" "$VERSION" "$count" "$MODE" \
        "$PROFILE" "$RUSTC_REL" "$RUSTC_COMMIT" "$HOST_TRIPLE" \
        "$CARGO_V" "$CMAKE_V" "$CXX_V" "$NOW" "$CXXFLAGS_OUT" "$DEST" \
        "$DP_COMPILER" "$DP_BUILD_TYPE" "$DP_CXXFLAGS" "$DP_GENERATOR" \
        "$DP_DUMPMACHINE" "$DP_CXX_VERSION" "$DP_LIBC" "$LOCK_SHA" <<'PY'
import json, sys, os, hashlib
(stage, frm, src, ver, n, mode, profile, rustc, commit, triple,
 cargo, cmake, cxx, now, cxxflags, dest,
 dp_comp, dp_btype, dp_cflags, dp_gen, dp_dump, dp_cxxver, dp_libc,
 lock_sha) = sys.argv[1:]

def inv(root):
    lines = []
    for dirpath, _d, files in os.walk(root):
        for fn in sorted(files):
            p = os.path.join(dirpath, fn)
            rel = os.path.relpath(p, root)
            h = hashlib.sha256(); sz = 0
            with open(p, "rb") as f:
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk); sz += len(chunk)
            lines.append(f"{rel}\t{sz}\t{h.hexdigest()}")
    return "\n".join(sorted(lines))

libdir = os.path.join(stage, "lib")
lib_lines = []
total = 0
for name in sorted(os.listdir(libdir)):
    p = os.path.join(libdir, name)
    tgt = os.path.realpath(p) if os.path.islink(p) else p
    if not os.path.isfile(tgt):
        continue
    h = hashlib.sha256(); sz = 0
    with open(tgt, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk); sz += len(chunk)
    lib_lines.append(f"{name}\t{sz}\t{h.hexdigest()}")
    total += sz
lib_inv = hashlib.sha256("\n".join(sorted(lib_lines)).encode()).hexdigest()
inc_inv = hashlib.sha256(inv(os.path.join(stage, "include")).encode()).hexdigest()
# Effective bundle identity (manifest v4): archive+header content is
# NOT sufficient — two donors can emit byte-identical archives/headers
# yet differ in the ABI inputs that produced them. The identity covers
# the full recorded build provenance (donor compiler target/version,
# libc, C++ flags, generator, build type), the bridge CXXFLAGS roots
# and the linkage contract, so incompatible prebuilt A -> prebuilt B
# reuse is a fingerprint mismatch even when bytes happen to match.
donor_prov = {
    "cmake_cxx_compiler": dp_comp,
    "cmake_cxx_version": dp_cxxver,
    "cmake_build_type": dp_btype,
    "cmake_cxx_flags": dp_cflags,
    "cmake_generator": dp_gen,
    "cxx_dumpmachine": dp_dump,
    "libc_version": dp_libc,
}
prov_lines = [f"{k}={v}" for k, v in sorted(donor_prov.items())]
prov_lines += [f"cxxflags={cxxflags}", "linkage=static"]
prov_hash = hashlib.sha256("\n".join(sorted(prov_lines)).encode()).hexdigest()
bundle_id = hashlib.sha256(f"{ver}|{lib_inv}|{inc_inv}|{prov_hash}".encode()).hexdigest()

json.dump({
    "manifest_version": 4,
    "created_utc": now,
    "created_by": "scripts/ci/lbug-native-prepare.sh",
    "bundle_id": bundle_id,
    "lbug_version": ver,
    "lbug_crate_sha256": lock_sha,
    "lbug_src": src,
    "donor_out": frm,
    "donor_provenance": donor_prov,
    "materialize": mode,
    "cargo_profile": profile,
    "dest_path": os.path.realpath(dest),
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
    "lib_inventory_sha256": lib_inv,
    "include_inventory_sha256": inc_inv,
}, open(os.path.join(stage, "manifest.json"), "w"), indent=2)
PY

# Self-check the staged bundle before publication. The manifest already
# records the final publish path; env.sh was written with $DEST paths,
# so verification runs in stage mode (dest_path equality skipped).
python3 - "$STAGE" "$DEST" <<'PY'
import sys, os, json
mf = json.load(open(os.path.join(sys.argv[1], "manifest.json")))
mf["dest_path"] = os.path.realpath(sys.argv[2])
json.dump(mf, open(os.path.join(sys.argv[1], "manifest.json"), "w"), indent=2)
PY
if ! LBUG_VERIFY_STAGE=1 verify_dir "$STAGE" 0 >/dev/null; then
  echo "lbug-native: staged bundle failed self-verify:" >&2
  LBUG_VERIFY_STAGE=1 verify_dir "$STAGE" 0 >&2 || true
  exit 1
fi

# ── publish: retire old dest only around a VERIFIED replacement ───────
# Lifecycle contract:
#   1. The staged bundle already passed self-verify (above).
#   2. The previous valid bundle is moved aside, never deleted up front.
#   3. If the rename or the final verify fails, the previous bundle is
#      restored — a failed publication must not leave a broken or
#      unverified bundle live.
#   4. Only after the published bundle verifies AND the exclusive
#      bundle lock is still held (no consumer can hold a shared lock
#      referencing the retired dir) is the retired bundle removed.
OLD=""
if [ -d "$DEST" ]; then
  OLD="$DEST.old.$$"
  mv "$DEST" "$OLD" || { echo "lbug-native: cannot retire existing $DEST" >&2; exit 1; }
fi
if ! mv "$STAGE" "$DEST"; then
  if [ -n "$OLD" ]; then mv "$OLD" "$DEST"; fi
  echo "lbug-native: publish rename failed; previous bundle restored" >&2
  exit 1
fi
# Final verify at the published path. On failure the new bundle is
# unproven: remove it and restore the previous valid bundle.
if ! verify_dir "$DEST" 0 >/dev/null; then
  verify_dir "$DEST" 0 >&2 || true
  rm -rf "$DEST"
  if [ -n "$OLD" ]; then mv "$OLD" "$DEST"; fi
  echo "lbug-native: published bundle failed final verify; previous bundle restored" >&2
  exit 1
fi
# Success. Consumers verify under the SHARED side of this same lock
# (build-lane.sh/run-local.sh acquire it before --verify, BUILD-07), so
# holding the exclusive lock here proves no consumer can still reference
# the retired bundle — it is now safe to remove.
if [ -n "$OLD" ]; then rm -rf "$OLD"; fi
trap - EXIT
rm -rf "$STAGE" 2>/dev/null || true

echo "lbug-native: prepared $DEST ($count archives, $MODE, lbug $VERSION, profile $PROFILE)"
echo "lbug-native: bundle_id $(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_id"][:16])' "$DEST/manifest.json")…"
echo "lbug-native: verify with '$0 --verify $DEST'; consume via build-lane.sh --lbug-native $DEST or run-local.sh --lbug-native $DEST"

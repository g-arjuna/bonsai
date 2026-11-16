#!/usr/bin/env bash
# build_lane_selftest.sh — failure-injection coverage for the trusted
# build-lane / prebuilt-lbug-native mechanism (BN-019 follow-up,
# BUILD-01..12).
#
# Exercises the REAL scripts with tiny synthetic artifact dirs — no Rust
# is compiled. Covers manifest v4 mandatory fields, full-bundle integrity
# (archives + headers + shim), provenance-aware bundle_id, env.sh
# consistency, lane marker guard (identity + mode + bundle fingerprint +
# CXXFLAGS fingerprint), inherited LBUG_*/CXXFLAGS rejection, prebuilt
# A->B transitions, bundle lock protection, gate-env ABI probes, and
# lbug-native-prepare publish lifecycle (replace/restore/no-leak).
#
# Usage: scripts/ci/build_lane_selftest.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PREPARE="$REPO_ROOT/scripts/ci/lbug-native-prepare.sh"
LANE="$REPO_ROOT/scripts/ci/build-lane.sh"
GUARD="$REPO_ROOT/scripts/ci/build-lane-guard.sh"
RUNLOCAL="$REPO_ROOT/scripts/ci/run-local.sh"
WORK="$(mktemp -d /tmp/build-lane-selftest.XXXXXX)"
PASS=0; FAIL=0

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

ok()   { PASS=$((PASS+1)); echo "PASS  $1"; }
bad()  { FAIL=$((FAIL+1)); echo "FAIL  $1"; }

LOCK_VER="$(python3 - "$REPO_ROOT/Cargo.lock" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
m = re.search(r'name = "lbug"\s*\nversion = "([^"]+)"(?:[^[]*?)checksum = "([0-9a-f]{64})"', t)
print(m.group(1) + " " + m.group(2) if m else " ")
PY
)"
LBUG_VER="${LOCK_VER%% *}"; LBUG_SHA="${LOCK_VER##* }"

RUSTC_VV="$(rustc -vV 2>/dev/null)"
HOST="$(printf '%s\n' "$RUSTC_VV" | awk '/^host:/{print $2}')"
COMMIT="$(printf '%s\n' "$RUSTC_VV" | awk '/^commit-hash:/{print $2}')"

# Live ABI probe values for gate-env fixtures (must match what
# lbug-native-prepare.sh --verify --gate-env probes in THIS environment).
CXX_DUMP="$(c++ -dumpmachine 2>/dev/null || cc -dumpmachine 2>/dev/null || echo unknown)"
CXX_LIBC="$(ldd --version 2>/dev/null | head -1)"
CXX_VER_LINE="$(c++ --version 2>/dev/null | awk 'NR==1{print $0}')"
CXX_VER="$(python3 - "$CXX_VER_LINE" <<'PY'
import sys
f = sys.argv[1].split()
print(f[3] if len(f) > 3 else f[-1])
PY
)"

# ── fixture: synthetic native dir with manifest v4 ────────────────────
# $1=dest $2=kind(file|symlink) $3=content-salt (changes bundle_id)
make_native_dir() {
  local dest="$1" kind="$2" salt="${3:-}"
  mkdir -p "$dest/lib" "$dest/include/lbug-src" "$dest/include/build-src"
  echo '// shim' > "$dest/include/lbug-src/lbug.hpp"
  echo "// hdr $salt" > "$dest/include/build-src/header.h"
  local donor="$WORK/donor-$$-$salt"
  mkdir -p "$donor/build/src"
  for n in liblbug.a libre2.a libparquet.a libzstd.a libutf8proc.a libfoo.a; do
    printf 'FAKE-ARCHIVE-%s-%s' "$n" "$salt" > "$donor/build/src/$n"
    if [ "$kind" = symlink ]; then
      ln -s "$donor/build/src/$n" "$dest/lib/$n"
    else
      cp "$donor/build/src/$n" "$dest/lib/$n"
    fi
  done
  # env.sh consistent with dest paths
  cat > "$dest/env.sh" <<EOF
export LBUG_LIBRARY_DIR="$dest/lib"
export LBUG_INCLUDE_DIR="$dest/include/lbug-src"
export CXXFLAGS="-I$dest/include/build-src -I$dest/include/build-src/include -I$dest/include/fastpfor -I$dest/include/alp"
EOF
  DONOR="$donor" SALT="$salt" CXX_DUMP="$CXX_DUMP" CXX_LIBC="$CXX_LIBC" \
      CXX_VER="$CXX_VER" python3 - "$dest" "$LBUG_VER" "$LBUG_SHA" \
      "$HOST" "$COMMIT" "$kind" <<'PY'
import json, sys, os, hashlib
(dest, ver, lsha, host, commit, kind) = sys.argv[1:7]
donor, salt = os.environ["DONOR"], os.environ["SALT"]
def hfile(p):
    h = hashlib.sha256(); sz = 0
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""): h.update(c); sz += len(c)
    return sz, h.hexdigest()
libdir = os.path.join(dest, "lib")
lines, total = [], 0
for name in sorted(os.listdir(libdir)):
    p = os.path.join(libdir, name)
    tgt = os.path.realpath(p) if os.path.islink(p) else p
    sz, hh = hfile(tgt)
    lines.append(f"{name}\t{sz}\t{hh}"); total += sz
lib_inv = hashlib.sha256("\n".join(sorted(lines)).encode()).hexdigest()
incdir = os.path.join(dest, "include")
ilines = []
for root, _d, files in os.walk(incdir):
    for fn in sorted(files):
        p = os.path.join(root, fn)
        rel = os.path.relpath(p, incdir)
        sz, hh = hfile(p)
        ilines.append(f"{rel}\t{sz}\t{hh}")
iinv = hashlib.sha256("\n".join(sorted(ilines)).encode()).hexdigest()
# bundle_id v4: inventories + effective build identity (donor ABI
# provenance + bridge cxxflags + linkage), mirroring prepare/verify.
cxxf = f"-I{dest}/include/build-src -I{dest}/include/build-src/include -I{dest}/include/fastpfor -I{dest}/include/alp"
donor_prov = {"cmake_cxx_compiler": "/usr/bin/c++",
    "cmake_cxx_version": os.environ["CXX_VER"],
    "cmake_build_type": "RelWithDebInfo", "cmake_cxx_flags": " -fPIC",
    "cmake_generator": "Unix Makefiles",
    "cxx_dumpmachine": os.environ["CXX_DUMP"],
    "libc_version": os.environ["CXX_LIBC"]}
prov_lines = [f"{k}={v}" for k, v in sorted(donor_prov.items())]
prov_lines += [f"cxxflags={cxxf}", "linkage=static"]
prov_hash = hashlib.sha256("\n".join(sorted(prov_lines)).encode()).hexdigest()
bundle = hashlib.sha256(f"{ver}|{lib_inv}|{iinv}|{prov_hash}".encode()).hexdigest()
mode = "symlink" if kind == "symlink" else "copy"
json.dump({
    "manifest_version": 4, "bundle_id": bundle,
    "lbug_version": ver, "lbug_crate_sha256": lsha,
    "lbug_src": donor, "donor_out": donor,
    "donor_provenance": donor_prov,
    "materialize": mode, "cargo_profile": "debug",
    "dest_path": os.path.realpath(dest), "host_triple": host,
    "rustc": "1.95.0", "rustc_commit": commit, "cargo": "1.95.0",
    "cmake": "3.28.3", "cxx": "13.3.0", "linkage": "static",
    "cxxflags": cxxf, "archive_count": len(lines),
    "lib_total_bytes": total, "lib_inventory_sha256": lib_inv,
    "include_inventory_sha256": iinv,
}, open(os.path.join(dest, "manifest.json"), "w"), indent=2)
PY
}

mf_edit() { python3 - "$1" "$2" "$3" <<'PY'
import json, sys
p = sys.argv[1] + "/manifest.json"
d = json.load(open(p))
if sys.argv[3] == "del": d.pop(sys.argv[2], None)
else: d[sys.argv[2]] = sys.argv[3]
json.dump(d, open(p, "w"))
PY
}

STUB_BIN="$WORK/stub-bin"; mkdir -p "$STUB_BIN"
printf '#!/bin/sh\nexit 0\n' > "$STUB_BIN/cargo"
chmod +x "$STUB_BIN/cargo"

ver_ok() { bash "$PREPARE" --verify "$1" >/dev/null 2>&1; }

# ── 1. valid copy-mode dir verifies ───────────────────────────────────
D1="$WORK/native-good"; make_native_dir "$D1" file
ver_ok "$D1" && ok "verify accepts well-formed v4 dir" || bad "verify accepts well-formed v3 dir"

# ── 2. content drift (archive) ────────────────────────────────────────
D2="$WORK/native-drift"; make_native_dir "$D2" file
printf 'TAMPERED' >> "$D2/lib/libfoo.a"
ver_ok "$D2" && bad "verify rejects archive drift" || ok "verify rejects archive drift"

# ── 3. header modification ────────────────────────────────────────────
D3="$WORK/native-hdr"; make_native_dir "$D3" file
echo '// tampered' >> "$D3/include/build-src/header.h"
ver_ok "$D3" && bad "verify rejects header modification" || ok "verify rejects header modification"

# ── 4. env.sh inconsistency ───────────────────────────────────────────
D4="$WORK/native-env"; make_native_dir "$D4" file
sed -i 's|CXXFLAGS=".*|CXXFLAGS="-I/stale/path"|' "$D4/env.sh"
ver_ok "$D4" && bad "verify rejects env.sh mismatch" || ok "verify rejects env.sh mismatch"

# ── 5. mandatory field missing (parametrized spot checks) ─────────────
for field in bundle_id lbug_version host_triple rustc_commit archive_count lib_total_bytes lib_inventory_sha256 include_inventory_sha256 linkage materialize lbug_crate_sha256; do
  Dm="$WORK/native-miss-$field"; make_native_dir "$Dm" file
  mf_edit "$Dm" "$field" del
  ver_ok "$Dm" && bad "verify rejects missing $field" || ok "verify rejects missing $field"
done
Dmv="$WORK/native-mv3"; make_native_dir "$Dmv" file
mf_edit "$Dmv" manifest_version 3
ver_ok "$Dmv" && bad "verify rejects manifest_version 3" || ok "verify rejects manifest_version 3"
Dd="$WORK/native-dp"; make_native_dir "$Dd" file
mf_edit "$Dd" donor_provenance '{}'
ver_ok "$Dd" && bad "verify rejects empty donor_provenance" || ok "verify rejects empty donor_provenance"

# ── 6. broken symlink + missing donor ─────────────────────────────────
D6="$WORK/native-broken"; make_native_dir "$D6" symlink
ln -sfn "$WORK/nonexistent.a" "$D6/lib/libfoo.a"
ver_ok "$D6" && bad "verify rejects broken symlink" || ok "verify rejects broken symlink"

# ── 7. toolchain/ABI mismatch (strict) ────────────────────────────────
D7="$WORK/native-tc"; make_native_dir "$D7" file
mf_edit "$D7" rustc_commit deadbeefdeadbeef
ver_ok "$D7" && bad "verify rejects rustc_commit mismatch" || ok "verify rejects rustc_commit mismatch"
# gate-env mode tolerates preparing-toolchain drift but not host_triple
if bash "$PREPARE" --verify --gate-env "$D7" >/dev/null 2>&1; then ok "gate-env tolerates rustc drift"; else bad "gate-env tolerates rustc drift"; fi
D7b="$WORK/native-triple"; make_native_dir "$D7b" file
mf_edit "$D7b" host_triple wasm32-unknown-unknown
ver_ok "$D7b" && bad "verify rejects host_triple mismatch" || ok "verify rejects host_triple mismatch"
bash "$PREPARE" --verify --gate-env "$D7b" >/dev/null 2>&1 && bad "gate-env rejects host_triple mismatch" || ok "gate-env rejects host_triple mismatch"

# ── 7b. gate-env ABI probes (BUILD-10): live toolchain vs donor ───────
# Fixtures carry THIS host's c++ dumpmachine/libc/cxx version, so a
# well-formed dir passes --gate-env; mutating any ABI input must fail.
bash "$PREPARE" --verify --gate-env "$D1" >/dev/null 2>&1 \
  && ok "gate-env accepts ABI-compatible dir" || bad "gate-env accepts ABI-compatible dir"
dp_edit() { python3 - "$1" "$2" "$3" <<'PY'
import json, sys
p = sys.argv[1] + "/manifest.json"
d = json.load(open(p))
d.setdefault("donor_provenance", {})[sys.argv[2]] = sys.argv[3]
json.dump(d, open(p, "w"))
PY
}
D8a="$WORK/native-abi-dump"; make_native_dir "$D8a" file
dp_edit "$D8a" cxx_dumpmachine riscv64-unknown-linux-gnu
bash "$PREPARE" --verify --gate-env "$D8a" >/dev/null 2>&1 \
  && bad "gate-env rejects donor/compiler target mismatch" || ok "gate-env rejects donor/compiler target mismatch"
D8b="$WORK/native-abi-libc"; make_native_dir "$D8b" file
dp_edit "$D8b" libc_version "ldd (GNU libc) 0.0"
bash "$PREPARE" --verify --gate-env "$D8b" >/dev/null 2>&1 \
  && bad "gate-env rejects libc mismatch" || ok "gate-env rejects libc mismatch"
D8c="$WORK/native-abi-cxx"; make_native_dir "$D8c" file
dp_edit "$D8c" cmake_cxx_version "0.0.0"
bash "$PREPARE" --verify --gate-env "$D8c" >/dev/null 2>&1 \
  && bad "gate-env rejects C++ version mismatch" || ok "gate-env rejects C++ version mismatch"
# Mutating donor provenance also breaks bundle_id v4 recomputation, so
# strict verify rejects it too (via identity drift, not the ABI probe).
bash "$PREPARE" --verify "$D8a" >/dev/null 2>&1 \
  && bad "strict verify rejects provenance-mutated dir" || ok "strict verify rejects provenance-mutated dir"

# ── 8. lane guard: mode + fingerprint + marker robustness ─────────────
LANE_DIR="$WORK/lane"; mkdir -p "$LANE_DIR"
LANE_ID="$(python3 -c 'import hashlib,os,sys;print(hashlib.sha256(os.path.realpath(sys.argv[1]).encode()).hexdigest()[:16])' "$LANE_DIR")"
cat > "$LANE_DIR/.lane-compat.json" <<EOF
{"marker_version":1,"lane_id":"$LANE_ID","lbug_mode":"bundled","lbug_native_id":"","rustc":"x"}
EOF
# bundled lane + prebuilt request -> hard fail (exit 1) without --reset
if bash "$GUARD" check --dir "$LANE_DIR" --mode prebuilt --native-id abc >/dev/null 2>&1; then
  bad "guard rejects bundled->prebuilt without reset"
else ok "guard rejects bundled->prebuilt without reset"; fi
# with --reset -> action reset (exit 3)
if bash "$GUARD" check --dir "$LANE_DIR" --mode prebuilt --native-id abc --reset >/dev/null 2>&1; then
  bad "guard --reset should exit 3"; else
  bash "$GUARD" check --dir "$LANE_DIR" --mode prebuilt --native-id abc --reset >/dev/null 2>&1; [ $? -eq 3 ] && ok "guard --reset reports reset action" || bad "guard --reset reports reset action"; fi
# corrupt marker on populated lane -> fail
echo junk > "$LANE_DIR/junkfile"
echo 'not json' > "$LANE_DIR/.lane-compat.json"
if bash "$GUARD" check --dir "$LANE_DIR" --mode bundled >/dev/null 2>&1; then
  bad "guard rejects corrupt marker on populated lane"; else ok "guard rejects corrupt marker on populated lane"; fi
# populated lane, missing marker -> fail
rm -f "$LANE_DIR/.lane-compat.json"
if bash "$GUARD" check --dir "$LANE_DIR" --mode bundled >/dev/null 2>&1; then
  bad "guard rejects missing marker on populated lane"; else ok "guard rejects missing marker on populated lane"; fi
# fresh lane -> proceed
rm -rf "$LANE_DIR"; mkdir -p "$LANE_DIR"
bash "$GUARD" check --dir "$LANE_DIR" --mode bundled >/dev/null 2>&1 && ok "guard accepts fresh lane" || bad "guard accepts fresh lane"

# ── 9. prebuilt A -> prebuilt B through build-lane.sh ─────────────────
DA="$WORK/native-A"; DB="$WORK/native-B"
make_native_dir "$DA" file aaa; make_native_dir "$DB" file bbb
IDA="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_id"])' "$DA/manifest.json")"
IDB="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_id"])' "$DB/manifest.json")"
LANE2="$WORK/lane2"
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE2" --lbug-native "$DA" -- true >/dev/null 2>&1 \
  && ok "build-lane accepts bundle A" || bad "build-lane accepts bundle A"
# same bundle again -> ok
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE2" --lbug-native "$DA" -- true >/dev/null 2>&1 \
  && ok "build-lane reuses bundle A" || bad "build-lane reuses bundle A"
# bundle B without reset -> fail
if PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE2" --lbug-native "$DB" -- true >/dev/null 2>&1; then
  bad "build-lane rejects prebuilt A->B without reset"
else ok "build-lane rejects prebuilt A->B without reset"; fi
# with reset -> pass (stub cargo clean exits 0)
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE2" --lbug-native "$DB" --reset-lbug-artifacts -- true >/dev/null 2>&1 \
  && ok "build-lane --reset permits A->B" || bad "build-lane --reset permits A->B"
MARK_ID="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["lbug_native_id"])' "$LANE2/.lane-compat.json")"
[ "$MARK_ID" = "$IDB" ] && ok "lane marker records bundle B fingerprint" || bad "lane marker records bundle B fingerprint"

# ── 10. bundled -> prebuilt through build-lane.sh ─────────────────────
LANE3="$WORK/lane3"
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE3" -- true >/dev/null 2>&1 \
  && ok "build-lane bundled run" || bad "build-lane bundled run"
if PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE3" --lbug-native "$DA" -- true >/dev/null 2>&1; then
  bad "build-lane rejects bundled->prebuilt via run"; else ok "build-lane rejects bundled->prebuilt via run"; fi
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE3" --lbug-native "$DA" --reset-lbug-artifacts -- true >/dev/null 2>&1 \
  && ok "build-lane --reset permits bundled->prebuilt" || bad "build-lane --reset permits bundled->prebuilt"

# ── 11. inherited LBUG_* / stale CXXFLAGS rejected ────────────────────
if LBUG_LIBRARY_DIR=/tmp/x PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane4" -- true >/dev/null 2>&1; then
  bad "sanity rejects inherited LBUG_LIBRARY_DIR"; else ok "sanity rejects inherited LBUG_LIBRARY_DIR"; fi
if CXXFLAGS="-I/tmp/foo/include/lbug-src" PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane4" -- true >/dev/null 2>&1; then
  bad "sanity rejects stale lbug CXXFLAGS"; else ok "sanity rejects stale lbug CXXFLAGS"; fi
# prebuilt env comes from manifest, not env.sh: stub command echoes vars
OUT="$(PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane5" --lbug-native "$DA" -- bash -c 'echo "LIB=$LBUG_LIBRARY_DIR INC=$LBUG_INCLUDE_DIR"' 2>/dev/null)"
echo "$OUT" | grep -q "LIB=$DA/lib" && echo "$OUT" | grep -q "INC=$DA/include/lbug-src" \
  && ok "prebuilt env derived from manifest" || bad "prebuilt env derived from manifest (got: $OUT)"

# ── 12. guard env subcommand + run-local untrusted refusal ────────────
EOUT="$(bash "$GUARD" env --dir "$DA")"
echo "$EOUT" | grep -q "LBUG_LIBRARY_DIR" && ok "guard env emits exports" || bad "guard env emits exports"
if bash "$RUNLOCAL" --untrusted --lbug-native "$DA" l0 >/dev/null 2>&1; then
  bad "run-local refuses --lbug-native under --untrusted"; else ok "run-local refuses --lbug-native under --untrusted"; fi
# run-local shared-target-dir with mismatched marker fails BEFORE docker
LANE6="$WORK/lane6"; mkdir -p "$LANE6"
cat > "$LANE6/.lane-compat.json" <<EOF
{"marker_version":1,"lbug_mode":"bundled","lbug_native_id":""}
EOF
if bash "$RUNLOCAL" --trusted --shared-target-dir "$LANE6" --lbug-native "$DA" l0 >/dev/null 2>&1; then
  bad "run-local gate fails on lane mode mismatch"; else ok "run-local gate fails on lane mode mismatch"; fi

# ── 13. lane identity + CXXFLAGS fingerprint (BUILD-08/12) ────────────
# Marker copied to a different lane dir must fail lane_id validation.
LANE7="$WORK/lane7"; mkdir -p "$LANE7"
cp "$LANE2/.lane-compat.json" "$LANE7/.lane-compat.json"
if bash "$GUARD" check --dir "$LANE7" --mode prebuilt --native-id "$IDB" \
     --native-path "$DB" >/dev/null 2>&1; then
  bad "guard rejects marker with foreign lane_id"; else ok "guard rejects marker with foreign lane_id"; fi
# Effective CXXFLAGS drift on the same lane fails closed; --reset remediates.
LANE8="$WORK/lane8"
CXXFLAGS="-O2" PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE8" -- true >/dev/null 2>&1 \
  && ok "build-lane records cxxflags fingerprint" || bad "build-lane records cxxflags fingerprint"
if CXXFLAGS="-O3" PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE8" -- true >/dev/null 2>&1; then
  bad "build-lane rejects CXXFLAGS drift without reset"; else ok "build-lane rejects CXXFLAGS drift without reset"; fi
CXXFLAGS="-O3" PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$LANE8" --reset-lbug-artifacts -- true >/dev/null 2>&1 \
  && ok "build-lane --reset permits CXXFLAGS re-baseline" || bad "build-lane --reset permits CXXFLAGS re-baseline"
# guard env emits TAB-separated assignments (no eval needed)
TE="$(bash "$GUARD" env --dir "$DA")"
printf '%s\n' "$TE" | grep -q $'^LBUG_LIBRARY_DIR\t' \
  && ok "guard env emits tab-separated assignments" || bad "guard env emits tab-separated assignments"

# ── 14. bundle shared lock precedes verification (BUILD-07) ───────────
# While an exclusive lock is held on the bundle, a consumer must block
# BEFORE it can verify/consume — proving verify cannot race a replace.
( exec 9>"$DA.lock"; flock -x 9; sleep 4 ) &
LOCK_HOLDER=$!
sleep 0.5
if timeout 2 env PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane9" --lbug-native "$DA" -- true >/dev/null 2>&1; then
  bad "build-lane blocks while bundle lock held"; else ok "build-lane blocks while bundle lock held"; fi
wait "$LOCK_HOLDER" 2>/dev/null || true
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane9" --lbug-native "$DA" -- true >/dev/null 2>&1 \
  && ok "build-lane proceeds after bundle lock released" || bad "build-lane proceeds after bundle lock released"
# Lock fds must NOT leak into the child command's descendants: a
# persistent helper (e.g. sccache) inheriting fd 9 would hold the lane
# lock past build-lane.sh's exit and deadlock the next run.
PATH="$STUB_BIN:$PATH" bash "$LANE" --dir "$WORK/lane9" --lbug-native "$DA" -- \
  bash -c '[ ! -e /proc/self/fd/9 ] && [ ! -e /proc/self/fd/7 ]' >/dev/null 2>&1 \
  && ok "lane/bundle lock fds not inherited by command" || bad "lane/bundle lock fds not inherited by command"

# ── 15. lbug-native-prepare publish lifecycle (BUILD-11) ──────────────
make_donor() { # $1=dir $2=content-salt
  local d="$1"
  mkdir -p "$d/build/src/include" "$d/build/src/extension/codegen/include"
  cat > "$d/build/CMakeCache.txt" <<'EOF'
CMAKE_CXX_COMPILER:FILEPATH=/usr/bin/c++
CMAKE_BUILD_TYPE:STRING=RelWithDebInfo
CMAKE_CXX_FLAGS:STRING= -fPIC -O2
CMAKE_GENERATOR:INTERNAL=Unix Makefiles
EOF
  local names="liblbug.a libre2.a libparquet.a libzstd.a libutf8proc.a" i
  for i in $(seq 1 11); do names="$names libdep$i.a"; done
  for n in $names; do printf 'FAKE-%s-%s' "$n" "$2" > "$d/build/src/$n"; done
}
DONOR_A="$WORK/donor-A"; DONOR_B="$WORK/donor-B"
make_donor "$DONOR_A" one; make_donor "$DONOR_B" two
PD="$WORK/prepared"
bash "$PREPARE" --from "$DONOR_A" --dest "$PD" --materialize copy >/dev/null 2>&1 \
  && ok "prepare publishes bundle" || bad "prepare publishes bundle"
ver_ok "$PD" && ok "prepared bundle verifies" || bad "prepared bundle verifies"
ID1="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_id"])' "$PD/manifest.json")"
# --replace publishes new bundle; no .old/.staging leftovers remain
bash "$PREPARE" --from "$DONOR_B" --dest "$PD" --materialize copy --replace >/dev/null 2>&1 \
  && ok "prepare --replace republishes" || bad "prepare --replace republishes"
ID2="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["bundle_id"])' "$PD/manifest.json")"
[ "$ID1" != "$ID2" ] && ok "replacement changes bundle_id" || bad "replacement changes bundle_id"
LEFTOVERS="$(find "$WORK" -maxdepth 1 -name 'prepared.old.*' -o -name 'prepared.staging.*' | wc -l)"
[ "$LEFTOVERS" = "0" ] && ok "no .old/.staging leftovers after replace" || bad "no .old/.staging leftovers after replace"
ver_ok "$PD" && ok "replaced bundle verifies" || bad "replaced bundle verifies"
# stale .old/.staging dirs are collected by the next prepare
mkdir -p "$PD.old.999" "$PD.staging.999"
bash "$PREPARE" --from "$DONOR_A" --dest "$PD" --materialize copy --replace >/dev/null 2>&1
[ ! -e "$PD.old.999" ] && [ ! -e "$PD.staging.999" ] \
  && ok "stale .old/.staging collected under lock" || bad "stale .old/.staging collected under lock"
# failed publication (dest blocked by regular file) leaves no staging
PD2="$WORK/blocked"; printf 'x' > "$PD2"
if bash "$PREPARE" --from "$DONOR_A" --dest "$PD2" --materialize copy >/dev/null 2>&1; then
  bad "prepare fails when dest is a regular file"; else ok "prepare fails when dest is a regular file"; fi
[ -f "$PD2" ] && [ "$(find "$WORK" -maxdepth 1 -name 'blocked.staging.*' | wc -l)" = "0" ] \
  && ok "failed prepare cleans staging, preserves dest" || bad "failed prepare cleans staging, preserves dest"
# prepare blocks on the exclusive bundle lock while a consumer holds -s
# (the lock is per-dest: <dest>.lock, so target the same $PD).
( exec 9>"$PD.lock"; flock -x 9; sleep 4 ) &
HOLDER=$!; sleep 0.5
if timeout 3 bash "$PREPARE" --from "$DONOR_B" --dest "$PD" --materialize copy --replace >/dev/null 2>&1; then
  bad "prepare waits on bundle lock"; else ok "prepare waits on bundle lock (timeout while locked)"; fi
wait "$HOLDER" 2>/dev/null || true
[ "$(find "$WORK" -maxdepth 1 -name 'prepared.staging.*' | wc -l)" = "0" ] \
  && ok "killed prepare leaves no staging dir" || bad "killed prepare leaves no staging dir"

# ── 16. run-local legacy --shared-target gets the lane guard ──────────
# A populated target with no compat marker must fail closed BEFORE any
# docker work (BUILD-09). Use a junk file in the worktree target dir.
mkdir -p "$REPO_ROOT/target"; printf 'x' > "$REPO_ROOT/target/.lane-selftest-junk"
if bash "$RUNLOCAL" --trusted --shared-target l0 >/dev/null 2>&1; then
  bad "run-local --shared-target fails on unmarked populated target"; else ok "run-local --shared-target fails on unmarked populated target"; fi
rm -f "$REPO_ROOT/target/.lane-selftest-junk"; rmdir "$REPO_ROOT/target" 2>/dev/null || true

echo "──────────────────────────────────────────"
echo "build-lane selftest: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

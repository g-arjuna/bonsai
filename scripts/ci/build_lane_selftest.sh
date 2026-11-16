#!/usr/bin/env bash
# build_lane_selftest.sh — failure-injection coverage for the trusted
# build-lane / prebuilt-lbug-native mechanism (BN-019 follow-up, BUILD-01..06).
#
# Exercises the REAL scripts with tiny synthetic artifact dirs — no Rust
# is compiled. Covers manifest v3 mandatory fields, full-bundle integrity
# (archives + headers + shim), env.sh consistency, lane marker guard
# (mode + bundle fingerprint), inherited LBUG_* rejection, prebuilt
# A->B transitions, and bundle lock protection.
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

# ── fixture: synthetic native dir with manifest v3 ────────────────────
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
  DONOR="$donor" SALT="$salt" python3 - "$dest" "$LBUG_VER" "$LBUG_SHA" \
      "$HOST" "$COMMIT" "$kind" <<'PY'
import json, sys, os, hashlib
dest, ver, lsha, host, commit, kind = sys.argv[1:7]
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
bundle = hashlib.sha256(f"{ver}|{lib_inv}|{iinv}".encode()).hexdigest()
cxxf = f"-I{dest}/include/build-src -I{dest}/include/build-src/include -I{dest}/include/fastpfor -I{dest}/include/alp"
mode = "symlink" if kind == "symlink" else "copy"
json.dump({
    "manifest_version": 3, "bundle_id": bundle,
    "lbug_version": ver, "lbug_crate_sha256": lsha,
    "lbug_src": donor, "donor_out": donor,
    "donor_provenance": {"cmake_cxx_compiler": "/usr/bin/c++",
        "cmake_build_type": "RelWithDebInfo", "cmake_cxx_flags": " -fPIC",
        "cmake_generator": "Unix Makefiles", "cxx_dumpmachine": "x86_64-linux-gnu",
        "libc_version": "ldd (GNU libc) 2.39"},
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
ver_ok "$D1" && ok "verify accepts well-formed v3 dir" || bad "verify accepts well-formed v3 dir"

# ── 2. content drift (archive) ────────────────────────────────────────
D2="$WORK/native-drift"; make_native_dir "$D2" file
printf 'TAMPERED' >> "$D2/lib/libfoo.a"
ver_ok "$D2" && bad "verify rejects archive drift" || ok "verify rejects archive drift"

# ── 3. header modification ────────────────────────────────────────────
echo '// tampered' >> "$D1/include/build-src/header.h" 2>/dev/null || true
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
Dmv="$WORK/native-mv2"; make_native_dir "$Dmv" file
mf_edit "$Dmv" manifest_version 2
ver_ok "$Dmv" && bad "verify rejects manifest_version 2" || ok "verify rejects manifest_version 2"
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

# ── 8. lane guard: mode + fingerprint + marker robustness ─────────────
LANE_DIR="$WORK/lane"; mkdir -p "$LANE_DIR"
cat > "$LANE_DIR/.lane-compat.json" <<EOF
{"marker_version":1,"lbug_mode":"bundled","lbug_native_id":"","rustc":"x"}
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

echo "──────────────────────────────────────────"
echo "build-lane selftest: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

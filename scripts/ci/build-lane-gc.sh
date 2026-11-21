#!/usr/bin/env bash
# build-lane-gc.sh — safe artifact reporting and retention for the trusted
# shared Cargo build lane (BN-019 build optimization).
#
# The shared lane accumulates hashed Cargo artifacts across source revisions,
# feature unifications and profiles. On the dev host it grew past 100 GB,
# dominated by ~1 GB dev-profile integration-test executables and their
# multi-generation stale copies. This tool reports lane composition and
# removes ONLY provably-safe artifact classes.
#
# SUBCOMMANDS
#   report   (default) Dry-run inventory only. Never deletes, never runs
#            Cargo, never takes the lane lock. Prints JSON + human summary.
#   clean    Retention. WITHOUT --apply it is a dry-run: it computes the
#            exact deletion set and prints it. WITH --apply it deletes.
#
# CLEAN SAFETY CONTRACT
#   * Deletion requires the explicit --apply flag.
#   * The lane's exclusive .lane.lock flock is held for the whole clean —
#     identical to the lock build-lane.sh holds while building — so no Cargo
#     build can run concurrently with deletion.
#   * The lane dir is canonicalized (realpath) and rejected if it is a
#     filesystem root, $HOME, the repository root, a git worktree root, or
#     lacks the .lane.lock marker of a managed lane.
#   * Only regular files matching Cargo's hashed artifact naming inside
#     <lane>/<profile>/{deps,.fingerprint} are ever candidates, plus whole
#     <profile>/incremental trees when --incremental is given. Nothing outside
#     the lane dir is touched: no Docker resources, no lbug-native bundles,
#     no worktrees, no evidence, no source files.
#   * The ci/ profile is excluded unless --include-ci is passed ("protect
#     current CI artifacts").
#   * --stale-units computes the CURRENT unit set by running a Cargo probe
#     (`cargo test --workspace --all-targets --no-run --message-format=json`)
#     inside the lane lock with the lane's recorded lbug mode/env. Every
#     deps artifact + .fingerprint unit NOT in the probe's artifact set is
#     stale: BOTH sides are removed together, so Cargo can never see a fresh
#     fingerprint pointing at a deleted artifact. Worst case for a wrongly
#     classified unit is a rebuild — never an incorrect artifact.
#   * A JSON manifest of every removed path is written before deletion and
#     appended after, under --log-dir (default ~/.cache/bonsai-ci/gc-logs).
#   * Actual recovered space is verified with statvfs before/after.
#
# CATEGORIES (clean; at least one required)
#   --intermediates   deps/*.rcgu.o, *.rcgu.dwo, *.o, *.dwo — post-link
#                     codegen-unit objects. Already consumed into the final
#                     artifact; regenerable on recompile. Always safe.
#   --dwp             deps/*.dwp and <profile>/*.dwp — packed debug-info
#                     files. Binaries still execute; rust-gdb symbolication
#                     is degraded until the owning unit recompiles.
#                     Opt-in because it is lossy for debugging.
#   --incremental     <profile>/incremental/ — pure incremental cache.
#                     Regenerated on demand; next incremental build is slower.
#   --stale-units     artifacts+fingerprints absent from the probe's current
#                     unit set (the multi-generation bulk). Requires probe.
#
# USAGE
#   scripts/ci/build-lane-gc.sh report [--dir LANE]
#   scripts/ci/build-lane-gc.sh clean [--dir LANE] [--apply]
#       [--profile NAME|all] [--include-ci]
#       [--intermediates] [--dwp] [--incremental] [--stale-units]
#       [--lbug-native DIR] [--probe-cmd "..."] [--log-dir DIR]
#
# ENV: BONSAI_BUILD_LANE (default ~/.cache/bonsai-ci/build-lane)
set -euo pipefail

CMD="${1:-report}"; shift || true
case "$CMD" in report|clean) ;; *) echo "usage: $0 report|clean ..." >&2; exit 2 ;; esac

DIR="${BONSAI_BUILD_LANE:-$HOME/.cache/bonsai-ci/build-lane}"
APPLY=0; PROFILE="all"; INCLUDE_CI=0; PROBE=0
DO_INTER=0; DO_DWP=0; DO_INCR=0; DO_STALE=0
LBUG_NATIVE=""; PROBE_CMD=""; LOG_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/bonsai-ci/gc-logs"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir)           DIR="$2"; shift 2 ;;
    --apply)         APPLY=1; shift ;;
    --profile)       PROFILE="$2"; shift 2 ;;
    --include-ci)    INCLUDE_CI=1; shift ;;
    --intermediates) DO_INTER=1; shift ;;
    --dwp)           DO_DWP=1; shift ;;
    --incremental)   DO_INCR=1; shift ;;
    --stale-units)   DO_STALE=1; shift ;;
    --lbug-native)   LBUG_NATIVE="$2"; shift 2 ;;
    --probe-cmd)     PROBE_CMD="$2"; shift 2 ;;
    --probe)         PROBE=1; shift ;;
    --log-dir)       LOG_DIR="$2"; shift 2 ;;
    -h|--help)       sed -n '2,60p' "$0"; exit 0 ;;
    *) echo "build-lane-gc: unknown arg $1" >&2; exit 2 ;;
  esac
done

log() { printf '[gc] %s\n' "$*" >&2; }
die() { printf 'build-lane-gc: GATE FAIL — %s\n' "$*" >&2; exit 1; }

# ── path safety ─────────────────────────────────────────────────────────
[ -d "$DIR" ] || die "lane dir does not exist: $DIR"
REAL_DIR="$(cd "$DIR" && pwd -P)"
REPO_TOP="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$REPO_TOP" ] && REPO_TOP="$(cd "$REPO_TOP" && pwd -P)"
case "$REAL_DIR" in
  /|"$HOME"|"$HOME/"|"${REPO_TOP:-/nonexistent}")
    die "refusing unsafe lane path: $REAL_DIR" ;;
esac
# A managed lane always has the lockfile created by build-lane.sh.
[ -e "$REAL_DIR/.lane.lock" ] || [ -e "$REAL_DIR/.lane-compat.json" ] \
  || die "$REAL_DIR does not look like a managed build lane (no .lane.lock/.lane-compat.json)"
# Never operate on a worktree/source root itself; a target dir nested inside
# a worktree is still only cargo artifacts, but the shared-lane contract is
# what this tool is for — refuse worktree roots outright.
if [ -n "$REPO_TOP" ]; then
  WT_TOP="$(git -C "$REAL_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
  [ -n "$WT_TOP" ] && [ "$(cd "$WT_TOP" && pwd -P)" = "$REAL_DIR" ] \
    && die "refusing to GC a worktree/source root: $REAL_DIR"
fi

# ── profile dir discovery ───────────────────────────────────────────────
# A cargo profile dir contains deps/ + .fingerprint/. dev profile output
# lands in 'debug'.
profile_subdir_for() { # cargo profile name -> dir name
  [ "$1" = "dev" ] && echo debug || echo "$1"
}
list_profile_dirs() {
  local d
  for d in "$REAL_DIR"/*/; do
    [ -d "${d}deps" ] && [ -d "${d}.fingerprint" ] || continue
    basename "$d"
  done
}
SELECTED=""
if [ "$PROFILE" = "all" ]; then
  SELECTED="$(list_profile_dirs)"
else
  SD="$(profile_subdir_for "$PROFILE")"
  [ -d "$REAL_DIR/$SD/deps" ] || die "profile '$PROFILE' (dir $SD) not present in lane"
  SELECTED="$SD"
fi
# ci protection applies to clean only; report shows everything.
if [ "$CMD" = "clean" ]; then
  FILTERED=""
  for sd in $SELECTED; do
    if [ "$sd" = "ci" ] && [ "$INCLUDE_CI" != "1" ]; then
      log "skipping ci/ profile (pass --include-ci to include)"
      continue
    fi
    FILTERED="$FILTERED $sd"
  done
  SELECTED="$(echo $FILTERED | xargs)"
  [ -n "$SELECTED" ] || die "no profile dirs selected"
fi
# --stale-units classifies artifacts against a Cargo probe's live unit set.
# The probe runs with the HOST toolchain/env, so artifacts built by a
# DIFFERENT ecosystem (e.g. the containerized gates' cargo home paths feed
# into dep fingerprints) are indistinguishable from stale generations and
# would be wrongly deleted. The ci/ profile dir is shared with the
# container gate lane and is therefore never eligible for stale-unit GC.
if [ "$CMD" = "clean" ] && [ "$DO_STALE" = "1" ]; then
  case " $SELECTED " in
    *" ci "*) die "--stale-units refuses the ci/ profile dir: it holds container-gate artifacts a host probe cannot see as live (use --intermediates/--dwp/--incremental for ci/)" ;;
  esac
fi
log "lane=$REAL_DIR profiles: $SELECTED"

# ── inventory / report ──────────────────────────────────────────────────
# Emits JSON inventory for one profile dir to stdout.
inventory_profile() {
  python3 - "$1" <<'PY'
import os, sys, json, collections
d = sys.argv[1]
deps = os.path.join(d, "deps"); fp = os.path.join(d, ".fingerprint")
cats = collections.Counter(); cnt = collections.Counter()
top = []
def add(cat, sz, n=1): cats[cat] += sz; cnt[cat] += n
if os.path.isdir(deps):
    for f in os.listdir(deps):
        p = os.path.join(deps, f)
        if not os.path.isfile(p): continue
        sz = os.path.getsize(p)
        if f.startswith("."): k = "link_temp"
        elif ".rcgu.o" in f: k = "intermediate_rcgu_o"
        elif ".rcgu.dwo" in f: k = "intermediate_rcgu_dwo"
        elif f.endswith(".dwo"): k = "dwo"
        elif f.endswith(".dwp"): k = "dwp"
        elif f.endswith(".d"): k = "depinfo"
        elif f.endswith((".rlib", ".rmeta")): k = "rlib_rmeta"
        elif f.endswith((".so", ".a")): k = "lib_archive"
        elif os.access(p, os.X_OK) and "." not in f: k = "executable"
        else: k = "other"
        add(k, sz); top.append((sz, os.path.join("deps", f)))
elif os.path.isdir(fp):
    for e in os.listdir(fp):
        p = os.path.join(fp, e)
        if os.path.isdir(p):
            sz = 0
            for r, _dd, fs in os.walk(p):
                for fn in fs:
                    try: sz += os.path.getsize(os.path.join(r, fn))
                    except OSError: pass
            add("fingerprint", sz)
incr = os.path.join(d, "incremental")
if os.path.isdir(incr):
    sz = 0
    for r, _dd, fs in os.walk(incr):
        for fn in fs:
            try: sz += os.path.getsize(os.path.join(r, fn))
            except OSError: pass
    add("incremental", sz)
total = sum(cats.values())
top.sort(reverse=True)
print(json.dumps({
    "dir": os.path.basename(d),
    "total_bytes": total,
    "categories": {k: {"bytes": cats[k], "files": cnt[k]} for k in cats},
    "top_files": [{"path": p, "bytes": s} for s, p in top[:15]],
}))
PY
}

if [ "$CMD" = "report" ]; then
  OUT="["
  FIRST=1
  for sd in $SELECTED; do
    [ $FIRST = 0 ] && OUT="$OUT,"
    FIRST=0
    OUT="$OUT$(inventory_profile "$REAL_DIR/$sd")"
  done
  OUT="$OUT]"
  LANE_TOTAL=$(du -sb "$REAL_DIR" 2>/dev/null | awk '{print $1}')
  FREE_KB=$(df -k --output=avail "$REAL_DIR" | tail -1)
  python3 - "$OUT" "$REAL_DIR" "$LANE_TOTAL" "$FREE_KB" <<'PY'
import json, sys
profiles = json.loads(sys.argv[1])
report = {"tool": "build-lane-gc.sh", "mode": "report", "lane": sys.argv[2],
          "lane_total_bytes": int(sys.argv[3]), "disk_avail_kib": int(sys.argv[4]),
          "profiles": profiles}
print(json.dumps(report, indent=2))
PY
  exit 0
fi

# ── clean ───────────────────────────────────────────────────────────────
[ "$APPLY" = 1 ] || log "DRY-RUN (pass --apply to delete)"
[ $((DO_INTER + DO_DWP + DO_INCR + DO_STALE)) -gt 0 ] \
  || die "clean requires at least one of --intermediates --dwp --incremental --stale-units"
if [ "$DO_STALE" != "1" ] && [ -n "$PROBE_CMD" ]; then
  log "note: --probe-cmd ignored without --stale-units"
fi
# The stale-unit probe runs Cargo inside the lane lock and may compile
# missing units. Dry-run computes the exact set only with --probe; without
# it, --stale-units reports size estimates only.
if [ "$DO_STALE" = 1 ] && [ "$APPLY" != "1" ] && [ "$PROBE" != "1" ]; then
  log "note: --stale-units dry-run without --probe: stale set not computed (would require a Cargo probe)"
fi

# Lane lock: same file build-lane.sh flocks exclusively while building.
exec 9>"$REAL_DIR/.lane.lock"
log "acquiring exclusive lane lock on $REAL_DIR/.lane.lock"
flock 9
log "lock acquired"

mkdir -p "$LOG_DIR"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
MANIFEST="$LOG_DIR/gc-$TS.jsonl"

FREE_BEFORE=$(df -k --output=avail "$REAL_DIR" | tail -1)

# ── lbug env for probe (prebuilt lanes need LBUG_* or cargo fails) ──────
LANE_MODE=""; NEED_ENV=0
if [ -f "$REAL_DIR/.lane-compat.json" ]; then
  LANE_MODE="$(python3 -c 'import json,sys
try: print(json.load(open(sys.argv[1])).get("lbug_mode",""))
except Exception: print("")' "$REAL_DIR/.lane-compat.json" 2>/dev/null || true)"
fi
PROBE_PROFILE_DIRS=""
for sd in $SELECTED; do
  case "$sd" in debug) PROBE_PROFILE_DIRS="$PROBE_PROFILE_DIRS dev";; *) PROBE_PROFILE_DIRS="$PROBE_PROFILE_DIRS $sd";; esac
done

LIVE_FILES_LIST="$(mktemp)"; STALE_UNITS_JSON="$(mktemp)"
trap 'rm -f "$LIVE_FILES_LIST" "$STALE_UNITS_JSON"' EXIT

if [ "$DO_STALE" = 1 ] && { [ "$APPLY" = 1 ] || [ "$PROBE" = 1 ]; }; then
  # Probe once per distinct cargo profile (dev->debug, others 1:1).
  PROFILES_SEEN=""
  for sd in $SELECTED; do
    case "$sd" in debug) cp="dev";; *) cp="$sd";; esac
    case " $PROFILES_SEEN " in *" $cp "*) continue;; esac
    PROFILES_SEEN="$PROFILES_SEEN $cp"
    if [ -z "$PROBE_CMD" ]; then
      PCMD="cargo test --workspace --all-targets --no-run --profile $cp --offline --message-format=json"
    else
      PCMD="$PROBE_CMD"
    fi
    log "probe ($cp): $PCMD"
    if [ "$LANE_MODE" = "prebuilt" ]; then
      [ -n "$LBUG_NATIVE" ] || die "--stale-units on a prebuilt lane requires --lbug-native DIR (probe must link against the recorded bundle)"
      _nr="$(cd "$LBUG_NATIVE" && pwd -P)"
      bash "$(dirname "$0")/lbug-native-prepare.sh" --verify "$_nr" >/dev/null \
        || die "lbug-native dir failed verification"
      exec 7>"$_nr.lock"; flock -s 7
      while IFS=$'\t' read -r _k _v; do [ -n "$_k" ] && export "$_k=$_v"; done \
        < <(bash "$(dirname "$0")/build-lane-guard.sh" env --dir "$_nr")
    else
      bash "$(dirname "$0")/build-lane-guard.sh" sanity
    fi
    mkdir -p "$LOG_DIR"
    PROBE_OUT="$LOG_DIR/probe-$TS-$cp.jsonl"; PROBE_ERR="$LOG_DIR/probe-$TS-$cp.err"
    CARGO_TARGET_DIR="$REAL_DIR" bash -c "$PCMD" > "$PROBE_OUT" 2> "$PROBE_ERR" \
      || { tail -20 "$PROBE_ERR" >&2; die "probe failed for profile $cp — refusing stale-unit deletion"; }
    python3 - "$PROBE_OUT" "$cp" >> "$LIVE_FILES_LIST" <<'PY'
import json, sys, os
path, prof = sys.argv[1], sys.argv[2]
live = set()
for line in open(path, errors="replace"):
    line = line.strip()
    if not line.startswith("{"): continue
    try: msg = json.loads(line)
    except Exception: continue
    if msg.get("reason") != "compiler-artifact": continue
    for fn in msg.get("filenames") or []:
        live.add(os.path.realpath(fn))
# emit prof<TAB>path lines
for p in sorted(live):
    print(f"{prof}\t{p}")
PY
    log "probe ($cp): $(wc -l < "$PROBE_OUT") artifact messages"
  done
fi

# ── build deletion set ─────────────────────────────────────────────────
# For each selected profile dir compute candidates per category.
CANDIDATES="$(mktemp)"   # lines: <profile_dir>\t<abs_path>\t<category>\t<bytes>
trap 'rm -f "$LIVE_FILES_LIST" "$STALE_UNITS_JSON" "$CANDIDATES"' EXIT

python3 - "$REAL_DIR" "$SELECTED" "$DO_INTER" "$DO_DWP" "$DO_INCR" "$DO_STALE" \
        "$LIVE_FILES_LIST" "$CANDIDATES" <<'PY'
import os, sys, re, json
real, selected, do_inter, do_dwp, do_incr, do_stale, livef, candf = sys.argv[1:9]
do_inter, do_dwp, do_incr, do_stale = (x == "1" for x in (do_inter, do_dwp, do_incr, do_stale))
HASH = re.compile(r'-[0-9a-f]{16}(\.|$)')
# live set: prof<TAB>abspath ; map cargo profile -> subdir
live = {}
if do_stale:
    for line in open(livef, errors="replace"):
        line = line.rstrip("\n")
        if not line: continue
        prof, path = line.split("\t", 1)
        live.setdefault(prof, set()).add(path)
cand = open(candf, "w")
def emit(sd, path, cat):
    try: sz = os.path.getsize(path)
    except OSError: sz = 0
    cand.write(f"{sd}\t{path}\t{cat}\t{sz}\n")
for sd in selected.split():
    prof = "dev" if sd == "debug" else sd
    pdir = os.path.join(real, sd)
    deps = os.path.join(pdir, "deps"); fpdir = os.path.join(pdir, ".fingerprint")
    stale_hashes = set()
    # per-file categories
    if os.path.isdir(deps):
        live_set = live.get(prof, set()) if do_stale else set()
        stale_hashes = set()
        if do_stale:
            # artifact hash = -HEX16 infix; stale if no live artifact shares it
            live_hashes = set()
            for p in live_set:
                m = re.search(r'-([0-9a-f]{16})(\.|$)', os.path.basename(p))
                if m: live_hashes.add(m.group(1))
            for f in os.listdir(deps):
                m = re.search(r'-([0-9a-f]{16})(\.|$)', f)
                if m and m.group(1) not in live_hashes:
                    stale_hashes.add(m.group(1))
        for f in sorted(os.listdir(deps)):
            p = os.path.join(deps, f)
            if not os.path.isfile(p): continue
            is_hash_art = bool(HASH.search(f))
            # Dot-prefixed names are cargo hardlink staging temps; once the
            # lane lock is held no build can own them — always orphans.
            if do_inter and (f.startswith(".") or ".rcgu.o" in f or ".rcgu.dwo" in f or f.endswith(".o") or f.endswith(".dwo")):
                emit(sd, p, "intermediate")
            elif do_dwp and f.endswith(".dwp"):
                emit(sd, p, "dwp")
            elif do_stale and is_hash_art:
                m = re.search(r'-([0-9a-f]{16})(\.|$)', f)
                if m and m.group(1) in stale_hashes:
                    emit(sd, p, "stale-unit")
    # stale fingerprint units: dir name <pkg>-<hash>; hash suffix match
    if do_stale and os.path.isdir(fpdir):
        for e in sorted(os.listdir(fpdir)):
            p = os.path.join(fpdir, e)
            if not os.path.isdir(p): continue
            m = re.search(r'-([0-9a-f]{16})$', e)
            if m and m.group(1) in stale_hashes:
                # emit every file inside
                for r, _dd, fs in os.walk(p):
                    for fn in fs:
                        emit(sd, os.path.join(r, fn), "stale-fingerprint")
                # emit dir itself via marker (rmdir handled later)
                cand.write(f"{sd}\t{p}\tstale-fingerprint-dir\t0\n")
    # top-level dwp files (hardlinked debug info for bins)
    if do_dwp:
        for f in sorted(os.listdir(pdir)):
            p = os.path.join(pdir, f)
            if os.path.isfile(p) and f.endswith(".dwp"):
                emit(sd, p, "dwp")
    # incremental tree
    if do_incr:
        incr = os.path.join(pdir, "incremental")
        if os.path.isdir(incr):
            for r, _dd, fs in os.walk(incr):
                for fn in fs:
                    emit(sd, os.path.join(r, fn), "incremental")
            cand.write(f"{sd}\t{incr}\tincremental-dir\t0\n")
cand.close()
PY

# Summary of candidate set
python3 - "$CANDIDATES" <<'PY'
import sys, collections
cats = collections.Counter(); n = collections.Counter()
for line in open(sys.argv[1]):
    parts = line.rstrip("\n").split("\t")
    if len(parts) < 4: continue
    cats[parts[2]] += int(parts[3]); n[parts[2]] += 1
for c in sorted(cats):
    print(f"  {c:22s} {n[c]:7d} files  {cats[c]/1e9:8.2f} GB", file=sys.stderr)
print(f"  {'TOTAL':22s} {sum(n.values()):7d} files  {sum(cats.values())/1e9:8.2f} GB", file=sys.stderr)
PY

if [ "$APPLY" != "1" ]; then
  log "dry-run complete — re-run with --apply to delete the set above"
  exit 0
fi

# ── apply: manifest first, then delete ─────────────────────────────────
python3 - "$CANDIDATES" "$MANIFEST" "$TS" "$REAL_DIR" <<'PY'
import sys, json, time
candf, man, ts, lane = sys.argv[1:5]
entries = []
for line in open(candf):
    parts = line.rstrip("\n").split("\t")
    if len(parts) < 4: continue
    entries.append({"profile": parts[0], "path": parts[1],
                    "category": parts[2], "bytes": int(parts[3])})
with open(man, "w") as f:
    for e in entries:
        f.write(json.dumps({"ts": ts, "lane": lane, **e, "action": "planned"}) + "\n")
print(f"manifest: {man} ({len(entries)} entries)", file=sys.stderr)
PY

# Delete files; directories (stale-fingerprint-dir, incremental-dir) via rmdir
# after their contents are gone. Sort: files first, dirs last.
python3 - "$CANDIDATES" "$MANIFEST" <<'PY'
import sys, os, json
candf, man = sys.argv[1:3]
entries = []
for line in open(candf):
    parts = line.rstrip("\n").split("\t")
    if len(parts) < 4: continue
    entries.append((parts[0], parts[1], parts[2], int(parts[3])))
files = [e for e in entries if not e[2].endswith("-dir")]
dirs  = [e for e in entries if e[2].endswith("-dir")]
removed = 0; failed = 0; bytes_freed = 0
log = open(man, "a")
for sd, path, cat, sz in files:
    try:
        os.remove(path); removed += 1; bytes_freed += sz
        log.write(json.dumps({"path": path, "category": cat, "bytes": sz, "action": "removed"}) + "\n")
    except FileNotFoundError:
        log.write(json.dumps({"path": path, "category": cat, "bytes": sz, "action": "already-gone"}) + "\n")
    except OSError as e:
        failed += 1
        log.write(json.dumps({"path": path, "category": cat, "bytes": sz, "action": f"error:{e}"}) + "\n")
for sd, path, cat, sz in dirs:
    # remove remaining contents then the dir itself (only if empty afterwards
    # for fingerprint dirs; incremental dirs are removed wholesale)
    try:
        if cat == "incremental-dir":
            import shutil; shutil.rmtree(path, ignore_errors=True)
        else:
            # fingerprint dir: delete any files not already removed, then rmdir
            for r, _dd, fs in os.walk(path, topdown=False):
                for fn in fs:
                    try: os.remove(os.path.join(r, fn))
                    except OSError: pass
                try: os.rmdir(r)
                except OSError: pass
        log.write(json.dumps({"path": path, "category": cat, "bytes": sz, "action": "removed-dir"}) + "\n")
    except OSError as e:
        failed += 1
        log.write(json.dumps({"path": path, "category": cat, "bytes": sz, "action": f"error:{e}"}) + "\n")
log.write(json.dumps({"summary": True, "removed_files": removed, "failed": failed,
                      "bytes_freed_reported": bytes_freed}) + "\n")
log.close()
print(f"removed={removed} failed={failed} reported_bytes={bytes_freed}", file=sys.stderr)
PY

FREE_AFTER=$(df -k --output=avail "$REAL_DIR" | tail -1)
RECOVERED_KB=$((FREE_AFTER - FREE_BEFORE))
python3 - "$MANIFEST" "$FREE_BEFORE" "$FREE_AFTER" "$RECOVERED_KB" <<'PY'
import sys, json
man, fb, fa, rec = sys.argv[1:5]
with open(man, "a") as f:
    f.write(json.dumps({"summary": True, "disk_avail_kib_before": int(fb),
                        "disk_avail_kib_after": int(fa),
                        "disk_recovered_kib": int(rec)}) + "\n")
print(f"disk: {int(fb)//1024} MiB -> {int(fa)//1024} MiB avail (+{int(rec)//1024} MiB)", file=sys.stderr)
PY
log "done — manifest: $MANIFEST"

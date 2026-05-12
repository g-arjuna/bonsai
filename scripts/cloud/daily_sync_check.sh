#!/usr/bin/env bash
# scripts/cloud/daily_sync_check.sh
# Verify cloud daily-sync branches and snapshot artifacts.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUTPUT_DIR="${REPO_ROOT}/runtime/driver_results"
OUTPUT_FILE="${OUTPUT_DIR}/cloud_sync_check.json"
BRANCH_PREFIX="${BRANCH_PREFIX:-sync/cloud-spike}"
TS="$(date +%s)"

mkdir -p "${OUTPUT_DIR}"
cd "${REPO_ROOT}"

write_result() {
  local status="$1"
  local ok="$2"
  local summary="$3"
  local checks_json="$4"
  local latest_branch="${5:-}"
  local branch_count="${6:-0}"

  python3 - <<'PY' "${OUTPUT_FILE}" "${TS}" "${status}" "${ok}" "${summary}" "${checks_json}" "${latest_branch}" "${branch_count}" "${BRANCH_PREFIX}"
import json
import sys

(
    output_file,
    ts_unix,
    status,
    ok,
    summary,
    checks_json,
    latest_branch,
    branch_count,
    branch_prefix,
) = sys.argv[1:]

payload = {
    "driver": "cloud_sync_check",
    "ts_unix": int(ts_unix),
    "status": status,
    "ok": ok == "true",
    "summary": summary,
    "checks": json.loads(checks_json),
    "environment": {
        "branch_prefix": branch_prefix,
        "latest_branch": latest_branch,
        "total_branches": int(branch_count),
    },
}

with open(output_file, "w", encoding="utf-8") as fh:
    json.dump(payload, fh, indent=2)
    fh.write("\n")
PY
}

echo "Checking remote cloud sync branches under ${BRANCH_PREFIX}/..."

remote_refs="$(git ls-remote --heads origin "${BRANCH_PREFIX}/*" 2>/dev/null || true)"
if [[ -z "${remote_refs}" ]]; then
  checks='[
    {"name":"remote_branches_present","check":"remote_branches_present","status":"fail","ok":false},
    {"name":"snapshot_tarball_present","check":"snapshot_tarball_present","status":"skip","ok":false},
    {"name":"readme_present","check":"readme_present","status":"skip","ok":false}
  ]'
  write_result "fail" "false" "no ${BRANCH_PREFIX} branches found on origin" "${checks}"
  echo "FAIL: no ${BRANCH_PREFIX} branches found on origin. Wrote ${OUTPUT_FILE}"
  exit 1
fi

branch_names="$(printf '%s\n' "${remote_refs}" | awk '{print $2}' | sed 's#refs/heads/##')"
total_branches="$(printf '%s\n' "${branch_names}" | sed '/^$/d' | wc -l)"
latest_branch="$(printf '%s\n' "${branch_names}" | sort -r | head -n 1)"

echo "Found ${total_branches} branch(es); latest=${latest_branch}"

git fetch --depth 1 origin "${latest_branch}" >/dev/null 2>&1 || true

files="$(git ls-tree -r --name-only FETCH_HEAD 2>/dev/null || true)"
if [[ -z "${files}" ]]; then
  checks='[
    {"name":"remote_branches_present","check":"remote_branches_present","status":"pass","ok":true},
    {"name":"latest_branch_readable","check":"latest_branch_readable","status":"fail","ok":false},
    {"name":"snapshot_tarball_present","check":"snapshot_tarball_present","status":"skip","ok":false}
  ]'
  write_result "fail" "false" "latest branch ${latest_branch} could not be read locally" "${checks}" "${latest_branch}" "${total_branches}"
  echo "FAIL: unable to inspect ${latest_branch}. Wrote ${OUTPUT_FILE}"
  exit 1
fi

snapshot_count="$(printf '%s\n' "${files}" | grep -E '^snapshot-[0-9]{4}-[0-9]{2}-[0-9]{2}\.tar\.zst$' | wc -l || true)"
has_readme=false
printf '%s\n' "${files}" | grep -qx 'README.md' && has_readme=true
has_snapshot=false
[[ "${snapshot_count}" -gt 0 ]] && has_snapshot=true

checks="$(python3 - <<'PY' "${has_readme}" "${has_snapshot}" "${snapshot_count}"
import json
import sys

has_readme = sys.argv[1] == "true"
has_snapshot = sys.argv[2] == "true"
snapshot_count = int(sys.argv[3])

checks = [
    {"name": "remote_branches_present", "check": "remote_branches_present", "status": "pass", "ok": True},
    {"name": "latest_branch_readable", "check": "latest_branch_readable", "status": "pass", "ok": True},
    {
        "name": "readme_present",
        "check": "readme_present",
        "status": "pass" if has_readme else "fail",
        "ok": has_readme,
    },
    {
        "name": "snapshot_tarball_present",
        "check": "snapshot_tarball_present",
        "status": "pass" if has_snapshot else "fail",
        "ok": has_snapshot,
        "count": snapshot_count,
    },
]
print(json.dumps(checks))
PY
)"

if [[ "${has_readme}" == "true" && "${has_snapshot}" == "true" ]]; then
  write_result "pass" "true" "latest branch ${latest_branch} contains snapshot tarball(s) and README" "${checks}" "${latest_branch}" "${total_branches}"
  echo "PASS: ${latest_branch} contains ${snapshot_count} snapshot tarball(s). Wrote ${OUTPUT_FILE}"
  exit 0
fi

write_result "fail" "false" "latest branch ${latest_branch} missing README or snapshot tarball" "${checks}" "${latest_branch}" "${total_branches}"
echo "FAIL: ${latest_branch} is missing required sync artifacts. Wrote ${OUTPUT_FILE}"
exit 1

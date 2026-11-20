# M6 integrated L0–L3 gate evidence — 2026-11-20

**Result: PASS**

| Item | Value |
|---|---|
| Repository | `g-arjuna/bonsai` |
| PR | #40 `feat/bn015-effective-yang-catalogue` |
| Tested production SHA | `64c538177664abb2b408dffbb06333da8540bf78` |
| Merge content | PR #17 BN-022 (`origin/main` `9d08cc8`) integrated into M6A/M6B head `d170e8e` |
| Command | `CI_BUILD_JOBS=2 scripts/ci/run-local.sh --trusted --shared-target-dir /home/arjuna/Desktop/bonsai/target --lbug-native /home/arjuna/.cache/bonsai-ci/lbug-native l0 l1 l2 l3` |
| Lane | `/home/arjuna/Desktop/bonsai/target`, prebuilt lbug bundle `2b38794b29bd4307f5945da07637e9a9866944ebf3a06b0c74633b60cae58031` |
| Wall time | ~1780 s (2026-11-20 12:38:57–13:08:37 +0530) |
| Full gate log | `/home/arjuna/Desktop/bonsai/target/ci/local-gates-1795158561.log` |
| run-local log dir | `/tmp/bonsai-m6/target/ci/local-run-20261120-123857` |

## Outcome

- L0 source checks: PASS (2 pre-existing fmt-drift files reported at base; no new drift).
- L1 compile/static validation: PASS.
- L2 required behavioral tests: PASS — bonsai lib `462/462`; `bn022_auth_closure` `49/49`; `bn022_auth_storage` `12/12`; `bn022_graph_read_boundary` `5/5`; `bn022_sec19_sec20` `13/13`; `yang_catalogue` `42/42`; rbmp crates and all other named L2 targets passed.
- L3 cross-component integration: PASS — 13 integration targets plus Python suites (`259 passed, 3 skipped, 1 deselected`) and UI driver checks.
- Final gate line: `ALL REQUESTED GATES PASSED`.

## Integration boundaries exercised

- `src/http_server/auth.rs`: BN-022 login-attempt tracker tests and YANG RBAC tests both present and passing (`rbac_tests::yang_mutations_require_admin`, `rbac_tests::yang_reads_stay_read_only`, `tests::*`).
- `scripts/ci/gates.sh`: L2 manifest contains both BN-022 targets and `yang_catalogue`.
- `src/http_server/mod.rs`: `/api/yang/*` routes remain inside `device_routes()` behind `auth::auth_middleware`; compile/activate are Admin-only via `required_role`.
- `src/cli.rs` / `src/main.rs`: both `bonsai auth` and `bonsai yang` command families dispatch correctly.
- `docs/build-times/runs.csv`: BN-022 and M6 evidence rows preserved.

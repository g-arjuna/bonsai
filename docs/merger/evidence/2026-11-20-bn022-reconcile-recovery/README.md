# BN-022 reconciliation recovery evidence (2026-11-20)

`scripts/recovery/final_candidate_recovery.sh` on the PR #17
reconciliation merge.

- Candidate SHA: `2ce3b2c57520558cdce3187de0dd9e10d433c76e`
- Binaries: shared build lane `~/.cache/bonsai-ci/build-lane/debug/`
  (`bonsai`, `wp28_fixture_seed` built `--features test-fixtures`;
  `test-fixtures` only gates the seeder binary — no production code
  path differs).
- Result: **79 PASS / 0 FAIL**, exit 0 (`results.txt`)
- Disposable workdir: `/tmp/bonsai-final-recovery.5TUHMx`
  (KEEP_WORK=1; synthetic credentials only, loopback listeners only —
  no live production credentials or databases touched).
- Coverage: live authn/authz matrix, routing credential consumption via
  loopback recorder, backup/verify/loss/restore/restart, JWT
  rotation/emergency/restore reconciliation, legacy graph→AuthStore
  migration, graph/AuthStore version-mismatch certification failures
  in both directions, and the full negative matrix.

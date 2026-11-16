# BN-022 final-candidate recovery — confirmation run (2026-11-16)

Controlled confirmation run of `scripts/recovery/final_candidate_recovery.sh`
after the bounded A2 harness corrections (process lifecycle, env isolation,
post-restore credential consumption, invalid-permission and corrupt-ciphertext
negatives, explicit certification-failure checks).

- Candidate SHA: `621955fc9b683de20175bf614bc5660904745a51`
  (`feat/bn022-auth-storage` head; src identical to gate-tested `29d1497` —
  `621955f` changes only `tests/b11_b12_b13_routing_workflow.rs`).
- Binary: `target/debug/bonsai`, sha256 in `binary.sha256` / `identity.txt`.
- Result: **79 PASS / 0 FAIL**, exit 0 (`passed-run.txt`, `results.txt`).
- Workdir preserved at `/tmp/bonsai-final-recovery.aJnIrs` (KEEP_WORK=1).

New coverage vs the 549568b exercise (70 PASS):

- Post-restore scoped routing credential consumption with the restored
  sealed service token: matching speaker proxied with internal credential,
  wrong speaker denied before proxying.
- Invalid-permissions negative: permissive-mode (0644) AuthStore snapshot
  opens and is tightened to 0600 — the earlier mode-600 check was a
  positive invariant, not a negative test.
- Corrupt-ciphertext negative: snapshot ciphertext byte flipped AND the
  manifest digest rebound, so rejection must come from age decrypt-verify
  (not the digest gate).
- F1 older-graph/newer-AuthStore: requires HTTP 503 + explicit
  "refusing to serve auth" certification error in the server log.
- F2 older-AuthStore/newer-graph: 503 path requires explicit migration
  error in log; 401 path requires a successful env-admin login as a
  meaningful setup precondition.
- `stop_server` now fails rather than deleting the pid file when the
  child survives SIGKILL; `stop_server legacy` → `stop_server "$LEG"`
  (instance dir, not name). `env -i` whitelist prevents dev-host
  BONSAI_* leakage; readiness verifies the recorded child pid owns the
  listener.

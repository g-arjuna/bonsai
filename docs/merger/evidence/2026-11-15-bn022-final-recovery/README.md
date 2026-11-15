# BN-022 final-candidate operational recovery — acceptance run (2026-11-15)

Harness: `scripts/recovery/final_candidate_recovery.sh` +
`scripts/recovery/recording_service.py` (loopback-only, synthetic
credentials, disposable workdir).

- Candidate SHA: `549568b8d5d3b7fb032dad5a27336b57bcbc359e`
  (`feat/bn022-auth-storage`; binary digest in `identity.txt`)
- Binary: `target/debug/bonsai` sha256 `77920bfe…d301`
- Result: **70 PASS / 0 FAIL**, exit 0 (`passed-run.txt`, `results.txt`)

## Root causes of the two prior failures (both harness defects, not product)

1. **Scoped API-key routing 401** — the scoped-key test ran before
   `set-service-token`, `routing_service_url` configuration and recorder
   startup. With no DB destination configured, the proxy fell back to the
   implicit default `http://127.0.0.1:8081`, where a live dev-host routing
   service correctly rejected the absent internal credential (401,
   forwarded). Wrong-speaker returned 403 earlier at scope enforcement.
   Fix: reordered the harness so service tokens + recording service +
   `routing_service_url` exist before scoped routing tests; added
   assertions that the matching-speaker request is proxied WITH the
   internal credential and the wrong-speaker request never reaches the
   downstream.
2. **Restored historical JWT 401** — the historical-restore step installed
   the pre-rotation snapshot over a RUNNING server (runbook §3.1 requires
   stop-first). The replacement process died on the LadybugDB graph lock
   while `/health` kept answering 200 from the stale post-rotation
   process — a false "start" pass; OLD_JWT was then correctly denied by
   the still-running rotated server. Fix: `stop_server` before install,
   `stop_server` now waits for real exit (SIGTERM→SIGKILL escalation),
   and `start_server` fails closed if a stale listener already owns the
   port.

`failed-run-before-fix.txt` preserves the prior 66 PASS / 2 FAIL run.

## Coverage executed

Admin/Operator/Viewer authn+authz; anon denial; API-key create/verify/
revoke; scoped `routing:read:speaker=` enforcement through the real
proxy path with credential-consumption proof at a loopback recorder;
backup/verify/restore/restart roundtrip; pre-backup JWT + API key
survival; wrong/missing passphrase, corrupt/truncated snapshot, tampered
+ missing + unsupported manifest — all fail closed; offline emergency
rotation (offline/quiesced); historical JWT resurrection confirmed then
reconciled by rotation; legacy graph→AuthStore migration with tombstones
and sealed quarantine; graph/AuthStore mismatch denied in both
directions.

`rec-routing-sanitized.jsonl` records downstream-observed requests with
token values replaced by `present` (no credential material committed).

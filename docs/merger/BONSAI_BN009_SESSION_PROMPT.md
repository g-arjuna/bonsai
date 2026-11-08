# Session prompt — complete BN-009 (sidecar operational lifecycle)

Paste the block below into a fresh Devin session in `/home/arjuna/Desktop/bonsai`.

---

Continue the Bonsai backlog from /home/arjuna/Desktop/bonsai (branch main, all work pushed — pull first). Read AGENTS.md (development contract + build/test discipline), BONSAI_BACKLOG.md (sole active queue), and docs/STATUS.md. Work ONE bounded item at a time. Preserve unrelated local changes — CLAUDE.md deletion and untracked gemini_analysis.txt are the user's; do not commit them.

ENVIRONMENT
- containerlab 0.82 + Docker 29.9.3; external stack healthy.
- Running task-dedicated resources: labs bonsai-bn003-vrf (172.100.208.x, FRR 10.6.2) and bonsai-bn003-r07vrf (172.100.207.x, FRR 10.6.1); lab bonsai-routing processes on 172.100.207.1:11019 (journals /tmp/bn003-journal-r07/) and 172.100.208.1:11029 (journals /tmp/bn003-journal/), token bn003-lab-token.
- Packaged deployment running: `docker compose -f docker-compose.yml -f /tmp/bn019-compose-override.yml --profile standalone` — bonsai-bonsai-standalone-1 (:3000 UI/API, :50051 gRPC, healthy) + bonsai-bonsai-routing-1 (127.0.0.1:11019, healthy). Image bonsai:latest. .env has BONSAI_VAULT_PASSPHRASE, BONSAI_ROUTING_SERVICE_TOKEN, BONSAI_ADMIN_USER=admin, BONSAI_ADMIN_PASS.
- Admin JWT: POST /api/auth/login {"username":"admin","password":<.env BONSAI_ADMIN_PASS>}.

BUILD DISCIPLINE
- sccache MANDATORY (never RUSTC_WRAPPER=). Inner loop: focused cargo test / python pytest only — never bare cargo test on bonsai. Python venv at .venv/bin/python.

COMPLETED (all committed + pushed)
- BN-019 essentially complete: packaged build/boot, volume backup→restore, install.sh install+upgrade drills, bonsai-migrate drill, 12/12 packaged-deployment browser journeys on genuine FRR BMP, 12-min packaged soak, CV5 handoff reconciliation. Only a full-length soak window remains (shared with BN-009).
- BN-009 intake + lifecycle slice + LIVE HEARTBEAT EXERCISE done (evidence docs/merger/evidence/2026-11-08-bn009-live-heartbeat/): collector_engine.py registered over gRPC to packaged deployment, /api/sidecars healthy with advancing status_json, SIGTERM → stale (45s) → lost (120s) verified.

BN-009 REMAINING WORK (in priority order)

1. Sidecar→API auth wiring (defect found in heartbeat exercise): collector_engine's ML job engine, export jobs, syslog/config embedding workers, inference loop call http://localhost:3000/api/* UNAUTHENTICATED → repeated 401s (seen: /api/graph/snapshot, /api/events/unembedded). No credential path exists.
   - Fix shape: add BONSAI_API_TOKEN env support — a shared helper in python/bonsai_ml (e.g. api_auth.py returning {"Authorization": "Bearer <token>"}) injected into all ~15 HTTP call sites (requests.* AND urllib.request.* — check bonsai_ml/{job_engine,export_job,inference_loop,syslog_embedding_worker,config_embedding_worker,detection_clustering,job_progress,snapshot_client}.py and gnn/*). Pass env through collector_engine.py (it already reads BONSAI_API_URL).
   - Verify live: create an API key via POST /api/auth/apikeys (admin JWT, alias e.g. bn009-sidecar, scope as needed), restart collector_engine with BONSAI_API_TOKEN=<key>, confirm the 401s stop in /tmp log and jobs proceed. Evidence under docs/merger/evidence/<date>-bn009-api-auth/.
   - Note: heartbeat timestamp fields (last_heartbeat_at/registered_at) render null in /api/sidecars — investigate whether it's a field-name/population bug worth fixing in the same slice or a separate one.

2. Restart/overload soak acceptance (shared design already written: docs/merger/evidence/2026-11-08-bn019-soak/README.md — do NOT duplicate; extend it): restart/overload the sidecar + routing stack, prove no silent data loss (journals durable, registry stale/lost honest, queue drops counted not hidden). Can reuse the running labs + packaged deployment.

3. If time permits: BN-013 first slice — trace one live incident journey end-to-end (intake done at docs/merger/evidence/2026-11-08-bn013-intake/) and record break points. Needs a live detection source; the rules sidecar + lab telemetry may generate one.

Each item: reproduce/verify first, focused checks then gate, evidence under docs/merger/evidence/, update BONSAI_BACKLOG.md + docs/STATUS.md, commit (do NOT push unless asked — actually previous sessions pushed; push is fine). Report changed files/commands/results/evidence path/next step. Operational device writes stay disabled. No live device writes, broad lab teardown, data deletion, or force-push.

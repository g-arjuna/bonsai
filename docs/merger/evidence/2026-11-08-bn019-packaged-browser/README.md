# BN-019 packaged-deployment browser acceptance — 8 November 2026

Base commit `99af884` (main). Fourth bounded BN-019 slice: run the BN-003
live routing browser-acceptance gate **against the packaged Docker
deployment** (`bonsai:latest` image, compose `standalone` profile) with a
genuine FRR BMP feed — not the in-process test harness.

## Result: PASS — 12/12 Playwright journeys

### Topology used

- Packaged deployment: `bonsai-bonsai-standalone-1` (:3000 UI/API) +
  `bonsai-bonsai-routing-1`.
- Genuine exporters: `clab-bonsai-bn003-r07vrf-frr1` (172.100.207.11,
  global + VRF BLUE `rd vpn export 65000:100`) and `-frr2`
  (172.100.207.12). frr2 BMP exporter + `10.10.1.0/24` origination
  configured via vtysh (same commands as the Rust gate; `write memory`
  unavailable — read-only container FS, runtime config only).
- Lab `bonsai-routing` (PID on 172.100.207.1:11019) stopped for the drill
  and restarted afterwards with its saved environment — journals
  `/tmp/bn003-journal-r07/` continued (2,974+ events).

### Two real defects/gaps found and handled

1. **Docker bridge SNAT destroys exporter identity.** With the stock
   bridge network, published-port traffic arrives at `bonsai-routing`
   from the docker gateway — all speakers collapsed to `172.20.0.1`
   (verified: `sys_name=frr1` but `speaker_addr=172.20.0.1`), which would
   falsify exporter identity and break speaker-scoped API keys. Fixed for
   the drill by running `bonsai-routing` with `network_mode: host`
   (compose override): BMP binds `0.0.0.0:11019` natively, real source IPs
   preserved; internal API moved to host `:8081` (still token-auth), and
   `bonsai-standalone` reached it via `host.docker.internal:8081`.
   → **Follow-up for packaging:** document or support host-network (or
   macvlan/ipvlan) deployment when genuine exporter IPs matter; the
   default bridge mode silently misattributes speakers. Not fixed in code
   here — recorded as a BN-019 finding.
2. **First-run onboarding gate.** Fresh packaged deployment reports
   `is_first_run:true` (`/api/setup/status`) → UI renders onboarding, not
   the app. Cleared by creating one environment via `POST /api/environments`
   (admin JWT) — legitimate operator action, persisted in graph DB.

### Stale-speaker contamination (cleaned, documented)

The earlier bridge-mode attempt left speaker `172.20.0.1` routes in the
durable journals; after journal truncation + container restart the stale
speaker was gone and only `.11`/`.12` remained. Journals truncated were
drill-generated data only (created minutes earlier); admin user,
environment, and scoped API key live in `bonsai.db` and were preserved.
Observation: routes of a dead speaker remained visible as
`complete_for_observed_view` — worth a product look (staleness marking for
disconnected speakers), noted not fixed.

### Control shim

`tests/ui_driver/routing_live_acceptance.spec.js` needs 6 control
endpoints that only exist in the Rust harness. A shim
(`/tmp/bn019-ctrl-shim.py`, copied log in evidence) implemented them
against the real deployment: vtysh via `docker exec` for frr modify/
withdraw/announce + frr1 BMP target toggle, `docker restart
bonsai-bonsai-routing-1` for the restart journey, and completeness polling
through the packaged JWT proxy (`/api/routing/v1/health`).

- Scoped API key created via `POST /api/auth/apikeys` with scope
  `routing:read:speaker=172.100.207.11,vrf_rd=0:0` (journey 2's 403 checks).
- Admin login uses deployment `.env` credentials (`admin`).

### Executed command

```
BONSAI_URL=http://localhost:3000 BONSAI_CTRL_URL=http://127.0.0.1:31081 \
BONSAI_SCOPED_KEY=bsk_5d20… BONSAI_ADMIN_PASS=… \
BONSAI_EVIDENCE_DIR=docs/merger/evidence/2026-11-08-bn019-packaged-browser \
PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome \
npx --prefix tests/ui_driver playwright test \
  --config tests/ui_driver/playwright.config.js \
  tests/ui_driver/routing_live_acceptance.spec.js --reporter=list
# → 12 passed (29.9s)   (log: bn019-playwright3.log)
```

Two earlier runs failed for the reasons above (onboarding gate; stale
speaker) — logs `bn019-playwright.log`, `bn019-playwright2.log` preserved
for provenance. Screenshots `00_…09_*.png` in this directory.

Notable: journey 11 restarted the **actual packaged container** — durable
journals in `bonsai_bonsai_graph_dev` preserved all routes/exporters
(post-restart `docker inspect` health = healthy, API 200).

### Restoration

Compose override reverted to `127.0.0.1:11019` bridge mode; both
containers healthy. Lab `bonsai-routing` restarted on
172.100.207.1:11019 — health `complete_for_observed_view`, peers_up=6,
journal growing. FRR BMP targets reconnected to the lab collector.
Scoped API key `apikey-91f5cf294b9269f5` left enabled in the deployment
(drill artifact; revoke via `DELETE /api/auth/apikeys/{id}` if undesired).

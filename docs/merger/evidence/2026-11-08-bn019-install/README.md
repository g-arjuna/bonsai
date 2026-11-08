# BN-019 install-from-package + upgrade-in-place — 8 November 2026

Base commit `ec50410` (clean clone). Second bounded BN-019 slice after the
packaged build/boot gate and volume backup→restore drill. Exercises
`scripts/install.sh` end-to-end on an isolated Compose project, then re-runs
it as an upgrade-in-place.

## Result: PASS (with recorded limitations)

### Isolation

The running packaged deployment (`bonsai-*`, ports 3000/50051/
127.0.0.1:11019) and the lab `bonsai-routing` instances
(172.100.207.1:11019, 172.100.208.1:11019) were left untouched. The drill ran
in a pristine `git clone` at `/tmp/bn019-install` with:

- `COMPOSE_PROJECT_NAME=bn019inst` (own containers/volumes/network)
- `COMPOSE_FILE=docker-compose.yml:compose-install-override.yml` remapping
  published ports to 13000 (UI), 25051 (gRPC), 127.0.0.1:11020 (BMP) — all
  verified free beforehand.
- Fresh generated `.env` (`BONSAI_VAULT_PASSPHRASE`,
  `BONSAI_ROUTING_SERVICE_TOKEN`, `BONSAI_ADMIN_USER/PASS`; mode 600).

### Run 1 — install-from-package: PASS

```
cd /tmp/bn019-install && COMPOSE_PROJECT_NAME=bn019inst \
  COMPOSE_FILE=.../docker-compose.yml:.../compose-install-override.yml \
  bash scripts/install.sh --no-open        # log: /tmp/bn019-install-run1.log
```

- Detected linux/x86_64, Docker path (no `--source`).
- `.env` passphrase detected — interactive prompt correctly skipped.
- `docker compose build bonsai-standalone` → `bonsai:latest` =
  `sha256:3d5ea1f0331a` (cargo build 8m31s, sccache warm; UI 11s).
- `up -d` created `bn019inst_bonsai-internal` network + both volumes;
  both containers reported **healthy**.
- Smoke on `http://localhost:13000`:
  - `GET /health` → `{"status":"ok", ...}` all subsystems ok
  - `GET /readyz` → 200; `GET /` → 200 text/html
  - `POST /api/auth/login` → JWT issued
  - `/api/routing/v1/{health,speakers,peers,routes,coverage,
    evidence-coverage,events}` → 200; `prefix-journey?prefix=10.0.0.0/8` → 200
    (bare call without `prefix` returns 400 — required param, not a defect)
  - `/api/routing/v1/health` → `completeness_state:"syncing"`, honest empty
    state (no BMP feed attached)

### Run 2 — idempotent re-run: PASS (no-op)

Re-ran `install.sh` unchanged: build fully cached (image byte-identical,
same sha256), `up -d` reported containers "Running" — **no recreation**,
volumes untouched. Installer is safely re-runnable.

### Run 3 — upgrade-in-place: PASS

Planted `.bn019-upgrade-sentinel` in `bn019inst_bonsai_graph_dev`, then
appended an HTML comment to `ui/index.html` (forces a new image without
touching Rust sources) and re-ran `install.sh`:

- New image `bonsai:latest` = `sha256:e5c32496c77f` (rust-builder
  `COPY . .` layer invalidated → 8m16s rebuild; UI stage 11s).
- `up -d` **Recreated** both containers (started 12:17:27Z, new image ID).
- Post-upgrade verification:
  - sentinel file still present in `bonsai_graph_dev`
  - both containers **healthy**; `/health` ok
  - admin login → JWT issued (user DB + vault passphrase persisted)
  - `/api/routing/v1/health` → 200 through the JWT boundary
  - `GET /` serves the new bundle (drill marker present in index.html)

### Notes / limitations (honest scope)

- Port remapping was injected via `COMPOSE_FILE`/`COMPOSE_PROJECT_NAME`
  env vars, which `install.sh` passes through untouched — no script changes
  needed, but the script itself has no port/project flags.
- `install.sh` prints `http://localhost:3000` in the success banner
  regardless of actual published ports — cosmetic; correct for the default
  install.
- The interactive first-run passphrase prompt was not exercised (pre-seeded
  `.env`); only the "already set" path ran.
- `--source` fallback path and `--uninstall`/`--verify` were not run.
- Upgrade = rebuild + `compose up -d`; there is no version pinning,
  rollback, or automated `bonsai-migrate` step in the installer (migration
  remains a manual operator tool — see BN-007).
- Image tag `bonsai:latest` is mutable; the drill rebuilt it twice
  (`3d5ea1f0331a` → `e5c32496c77f`). The pre-existing packaged deployment
  keeps running its original image unaffected.

## Teardown

Sandbox stack removed with project-scoped
`docker compose down -v` (only `bn019inst_*` resources); `/tmp/bn019-install`
clone deleted. Lab containers and the packaged deployment were not touched.

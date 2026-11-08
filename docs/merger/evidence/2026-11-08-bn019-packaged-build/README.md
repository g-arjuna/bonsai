# BN-019 packaged build + boot gate — 8 November 2026

Base commit `f7eefb9` + working-tree fixes below. First bounded slice of the
BN-019 intake plan: `docker build -f docker/Dockerfile.bonsai` +
`docker compose --profile standalone up` + `/health` + routing proxy smoke.

## Result: PASS (boot gate), with three real defects found and fixed

### Defects fixed

1. **`Dockerfile.bonsai` missing build deps** — `etcd-client v0.12.4` build
   script requires `protoc` (`protobuf-compiler`); repo `.cargo/config.toml`
   passes `-fuse-ld=mold` so the chef image also needs `mold`. Both added to
   the chef-stage apt install (`26dbd39` had already fixed the bonpy dist path
   and Rust 1.95 base; these were the remaining gaps).
2. **Container HEALTHCHECK could never pass** — `src/bin/healthcheck.rs`
   probed `/api/readiness`, which is behind `auth_middleware` (401). The
   unauthenticated readiness endpoint is `/readyz`. Fixed the probe path.
3. **`bonsai-routing` had no valid health probe** — the image HEALTHCHECK
   inherited by the routing container probed `:3000/readyz`, but
   `bonsai-routing` serves only the token-authenticated internal API on
   `:8081` (`/internal/routing/v1/*` all require `x-bonsai-routing-token`).
   `healthcheck` now accepts `BONSAI_HTTP_PORT`, `BONSAI_HEALTHCHECK_PATH`,
   and `BONSAI_HEALTHCHECK_TOKEN` env overrides; `docker-compose.yml` sets
   them for `bonsai-routing` (`:8081` + `/internal/routing/v1/health` +
   service token). No auth surface weakened — the probe uses the existing
   required token.

### Build-time fixes (dev-velocity, user-requested)

- `Cargo.toml` `[profile.release] codegen-units` 1 → 16: the `bonsai`
  crate compiled single-threaded under thin-LTO; ~15–50 min per link.
- `Dockerfile.bonsai`: `sccache` installed in chef image,
  `RUSTC_WRAPPER=sccache` + `/root/.cache/sccache` BuildKit cache mount on
  both cargo RUNs (was explicitly disabled before).
- `CARGO_TARGET_{X86_64,AARCH64}_UNKNOWN_LINUX_GNU_RUSTFLAGS` pinned in the
  image ENV so `cargo chef cook` (runs before `.cargo/config.toml` is
  copied) and `cargo build` use identical linker flags — dep fingerprints
  match and cook artifacts are reused instead of risking a full recompile.
- `.dockerignore`: added `.env` / `.env.local` — `COPY . .` was pulling the
  secrets file into the builder stage.

## Executed commands and results

```
docker build -f docker/Dockerfile.bonsai -t bonsai:latest .   # PASS (~49 min cold, then 17 min, 14 min per src change)
docker compose -f docker-compose.yml -f <override> --profile standalone up -d
# override only rebinds bonsai-routing 11019 to 127.0.0.1 (host port already
# used by the task-dedicated lab bonsai-routing on 172.100.207.1:11019)
```

Smoke (all against `http://localhost:3000`):

- `GET /health` → `{"status":"ok","version":"0.2.0", ...}` all subsystems ok
- `GET /readyz` → 200 `{"ready":true}`
- `POST /api/auth/login` (bootstrap admin env) → JWT issued
- `GET /api/routing/v1/{health,speakers,peers,routes,coverage,evidence-coverage,prefix-journey,events}` → all 200 through the Bonsai JWT boundary; `completeness_state:"syncing"` (honest empty state — no BMP feed attached)
- `GET /` → 200 text/html (Svelte UI served from image)
- `docker inspect` → `bonsai-standalone` **healthy**, `bonsai-routing` **healthy**
- in-container `/usr/local/bin/healthcheck` → exit 0 on both services

Image: `bonsai:latest` = `sha256:a1b68e1bc1a1` (188 MB), built from
`f7eefb9` + the fixes above. Re-verified on the final image: both
containers healthy, `/health` ok, admin login + `/api/routing/v1/health`
200.

### Build-time results (measured)

| Stage | Before | After |
|---|---|---|
| `cargo chef cook` (deps) | 30–49 min | **12 s** (sccache hits) |
| `cargo build` (bins) | recompiled all ~600 deps again: 14–47 min | **9m07s** — deps reused, only `rbmp-*` + `bonsai` lib/bins compile |

Root cause of the double compile: cargo-chef's generated skeleton does not
inherit `[profile.release]`, so cooked deps and `cargo build` had different
fingerprints. `CARGO_PROFILE_RELEASE_*` env vars in the Dockerfile now pin
identical effective profiles for both steps. `codegen-units` 1→16 also
restored intra-crate parallelism for the `bonsai` crate.

## Environment notes

- `.env` created locally (gitignored) with generated
  `BONSAI_VAULT_PASSPHRASE`, `BONSAI_ROUTING_SERVICE_TOKEN`, and bootstrap
  `BONSAI_ADMIN_USER`/`BONSAI_ADMIN_PASS` for the smoke login.
- `lab/fast-iteration/p4-ca.pem` placeholder touched (empty file) — the
  compose base bind-mounts it for the `all` profile; `standalone.toml`
  does not reference it. Already covered by `.dockerignore`.
- Lab `bonsai-routing` processes on 172.100.207.1/208.1 untouched; the
  compose port override is local to this verification (`/tmp` file, not
  committed).

## Backup → restore drill (same deployment): PASS

Found and fixed a real gap first: `backup_volumes.sh` /
`restore_volumes.sh` hardcoded unprefixed volume names, but compose
prefixes them with the project name (`bonsai_bonsai_graph_dev`,
`bonsai_bonsai_creds_dev`) — the scripts would have skipped every volume.
Both scripts now resolve logical → real volume names (literal, then
`<project>_` prefix, then unique `*_` suffix match) and restore archives
named after the real volumes, including the fresh-deploy case where the
volume does not exist yet.

Drill executed against the running standalone stack:

1. Planted `.bn019-restore-sentinel` files in `bonsai_bonsai_graph_dev`
   and `bonsai_bonsai_creds_dev`.
2. `scripts/backup_volumes.sh /tmp/bn019-backups/drill1` → both volumes
   archived (36K graph incl. `bonsai.db`, `bonsai_config.db`,
   `routing-bmp.journal`, `routing-events.jsonl`; 4K creds vault).
   Sentinel verified inside the tarball.
3. `compose down`, deleted both sentinels (simulated loss).
4. `echo y | scripts/restore_volumes.sh /tmp/bn019-backups/drill1` → both
   volumes restored; sentinels verified back in-place.
5. `compose up -d` → both containers **healthy** ~30s;
   `/health` = ok, admin login issues JWT, `/api/routing/v1/health` = 200
   on the restored graph DB.

`bonsai-migrate` live drill still open (binary not shipped in the image;
needs a deployed-target run or a documented host-side procedure).

## Not yet done (remaining BN-019 gates)

- ~~Backup→restore drill~~ — done above (volume-level; `bonsai-migrate`
  graph-migration drill still open).
- Install-from-package / upgrade-in-place run.
- Dedicated lab browser acceptance (BN-003 gate exists; needs a run
  against this packaged deployment or recorded reconciliation).
- Soak/endurance (shared design with BN-009).
- Reconcile legacy clean-slate / seven-day handoff criteria.
- ~~Re-verify boot gate on the final image~~ — done (`a1b68e1bc1a1`,
  both containers healthy, smoke green).

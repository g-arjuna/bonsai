# Bonsai DV4 — Batch 8 Session Prompt

## Project Context

Bonsai is a Rust + Svelte network observability platform. The codebase lives at `/Users/arjuna.ganesan/bonsai`. Rust backend in `src/`, Python sidecar/SDK in `python/`, Svelte UI in `ui/src/`. Graph DB is KuzuDB (crate `lbug`). HTTP framework is Axum.

**Dev environment**: Mac for coding, `cargo check` (cmake not installed — lbug build fails, expected). Ubuntu ops box for runtime testing. Workflow: implement → cargo check → commit → push → test on Ubuntu.

**Completed batches 1–7** have shipped ~60 tasks across all epics. The backlog is tracked in `BONSAI_CONSOLIDATED_BACKLOG_DV4.md` — completed tasks are marked `✅ batchN`.

---

## Batch 8 — Recommended Tasks (8 tasks, ~700 LOC)

Pick from the following directly implementable tasks. All are pure code — no lab hardware or external service dependencies.

### 1. D4-3 T1 — Secure credential memory (zeroize)
- **File**: `src/credentials.rs`
- Add `zeroize` crate to `Cargo.toml`. Change `StoredCredential.password: String` → `zeroize::Zeroizing<String>`. Same for `ResolvedCredential.password`.
- Add `impl Drop for VaultState` that zeros entries.
- Audit `vault.resolve()` call sites for long-lived password clones.

### 2. D4-3 T5 — UI-based LLM API key management
- **Files**: `ui/src/routes/Settings.svelte`, `src/http_server/settings.rs`
- Per-provider entry: name, model, API key (masked), custom base URL, active toggle.
- "Test connection" → `POST /api/ai/test` → minimal prompt → show model name + latency.
- Store keys in vault under alias `llm-{provider}`.

### 3. D4-3 T7 — DB + transport security
- **File**: `src/server_startup.rs`
- Enforce `runtime/` directory mode 700 at startup.
- Optional TLS: `[server.tls]` cert/key config via axum-server + rustls. Self-signed cert auto-gen if none provided.

### 4. D4-9 T1 — Python sidecar health HTTP endpoint
- **File**: `python/collector_engine.py`
- Add lightweight HTTP server: `GET /health` → `{status, uptime_secs, rules_loaded, last_detection_at_ns, detections_today, queue_depth}`.
- Config: `[sidecar] health_port = 9200`.

### 5. D4-9 T2 — Rust backend `/api/sidecar/status`
- **Files**: `src/http_server/governance.rs` or new `src/http_server/sidecar.rs`, `src/http_server/mod.rs`
- Proxy to sidecar health URL or return last gRPC heartbeat.
- Show sidecar card in `Collectors.svelte` with rules count, last detection, health badge.

### 6. D4-22 T3 — `install.sh` hardening
- **File**: `scripts/install.sh`
- Idempotency check, dependency version checks (Rust ≥1.70, Docker ≥24.0), ContainerLab install option, `--uninstall` flag.

### 7. D4-22 T4 — GitHub Actions CI workflow
- **File**: `.github/workflows/ci.yml`
- Jobs: build, test (cargo test + pytest), lint (clippy + fmt), ui-build.
- Trigger on push to main + PR. Cargo + npm caching.

### 8. D4-22 T5 — `ui/package-lock.json` audit
- Verify `package-lock.json` is committed. Run `npm audit`. Pin Svelte/Vite/D3 versions.

---

## Key Files to Read First

Before implementing, read these for current state:

| Purpose | File |
|---------|------|
| Credential vault | `src/credentials.rs` |
| AI provider config | `src/ai_provider.rs`, `src/config.rs` (AiConfig) |
| Settings UI | `ui/src/routes/Settings.svelte` |
| Settings API | `src/http_server/settings.rs` |
| Python sidecar | `python/collector_engine.py` |
| Collectors UI | `ui/src/routes/Collectors.svelte` |
| Server startup | `src/server_startup.rs` |
| Install script | `scripts/install.sh` |
| CI workflows | `.github/workflows/` |
| Route wiring | `src/http_server/mod.rs` |
| Cargo deps | `Cargo.toml` |

---

## Constraints

- **No cmake on Mac** — `cargo check` will fail at lbug build. Check for `error[E` lines only; cmake failure is expected.
- **No runtime testing on Mac** — never launch the app locally.
- **Svelte 5** with `$state` runes, not Svelte 4 stores.
- **Follow existing code patterns** — check how other handlers/routes are structured before adding new ones.
- **Mark completed tasks** in `BONSAI_CONSOLIDATED_BACKLOG_DV4.md` with `✅ batch8`.
- **Commit message format**: `batch8: D4-XX TN description, D4-YY TN description, ...`
- **Push after commit**.

---

## Alternative / Stretch Tasks

If the above batch finishes early or some items turn out to be blocked:

- **D4-14 T5** — Vault init documentation (README + install.sh docs)
- **D4-8 T3** — Coverage gap reporter (investigation_runtime.rs)
- **D4-8 T6** — Fault injection RCA test harness (python/inject_fault.py)
- **D4-21 T2** — SSE stream for governance events
- **D4-21 T5** — Shedding indicator in signal receivers
- **D4-9 T4** — Rules visibility + hot-reload from UI

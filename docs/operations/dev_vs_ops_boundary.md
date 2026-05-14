# Bonsai Dev/Ops Boundary

> Authored CV7 T1-1 — 2026-05-14. This is the single hard rule that orients
> every Claude/Codex/Windsurf session and every human collaborator. If you are
> in any doubt about what you may do where, run `bash scripts/dev/whichenv.sh`
> and re-read this page.

There are three environments. Each has exactly one job.

---

## Mac (code-development environment — SOURCE EDITING ONLY)

- **Tools**: Windsurf, Claude Code, Codex (for source authoring)
- **Activities**: read code, write code, write docs, `git add` / `git commit` / `git push`
- **NO Rust toolchain installed.** No `cargo`, no `clippy`, no `rustfmt`. The Mac
  does not compile bonsai.
- **No Python venv for bonsai work.** No `pytest`, no `ruff`. The Mac does not
  run bonsai's Python.
- **No Docker, no containerlab.** The Mac does not run any containers.
- **No local testing of any kind.** No smoke, no e2e, no unit tests. The Mac
  does not verify behaviour.
- Push code → GitHub Actions builds (Tier 6, pending) → Ubuntu laptop and cloud
  pull the built binary
- **Rationale**: the Mac is for the *authoring* loop only. Build/test/run all
  happens elsewhere. This eliminates the entire class of "works on my Mac" bugs
  and removes Docker Desktop overhead.
- **Verify clean**: `bash scripts/dev/check_mac.sh` — confirms no docker daemon,
  no clab residue, no bonsai processes, no Rust toolchain in PATH, no bonsai
  venv leakage.
- **Allowed operations**: `bash scripts/dev/macdev <op>` — see T1-3 wrapper for
  the full set. Anything outside the allowed list refuses explicitly.

---

## Ubuntu laptop (ops-testing environment — ALL TESTING HAPPENS HERE)

- **Tools**: Codex (only — for parity with cloud and to keep dev/ops loops
  separate), bash, git, containerlab, docker, Rust toolchain (interim until
  CI/CD pipeline is live), Python venv
- **Activities**: pull from GitHub, build if needed (interim) or install
  pre-built binary (post-Tier-6), run chaos cycle, run smoke tests, run e2e
  tests, run daily checks, write daily reports
- **NOT for**: writing new features, modifying source files except via
  `git pull origin main`
- **Rationale**: this is where bonsai operationally runs and where every test
  executes; mixing dev work here causes pid mismatches and stale-state bugs.

---

## Cloud (OCI ARM64)

- **Tools**: bash, systemd, docker, git, containerlab
- **Activities**: long-running chaos accumulation, daily check, GitHub sync
- **NOT for**: live development; pulled-from-main only
- **Rationale**: production-style continuous operation. Pulls binaries from
  CI once Tier 6 lands; interim is `cargo build --release` on the host.

---

## The Five Things That Will Trip You Up

1. **Never run `cargo`, `pytest`, `docker`, or `containerlab` on Mac.** If you
   find yourself typing those, stop and SSH to the Ubuntu laptop.
2. **Never modify source files on the Ubuntu laptop or cloud.** Pull-only. If
   you need to change a file, go back to the Mac.
3. **One deployment mode per environment** (Tier 2): laptop = bonsai-as-process,
   cloud = bonsai-as-systemd-service. Never mix.
4. **The `whichenv.sh` script is the source of truth** for which box you are
   on. Trust it over any visual cue.
5. **The dev/ops separation is enforced by tooling, not convention.** The
   `macdev` wrapper refuses build/test ops. The `check_mac.sh` guard refuses
   a dirty Mac. The bash scripts on the laptop/cloud refuse modifying source.

---

## How AI Agents Should Read This

If you are Claude Code, Codex, Windsurf, or any other AI agent that just
started a session:

1. Run `bash scripts/dev/whichenv.sh`. It will print one of:
   `mac-dev`, `ubuntu-ops`, `cloud-ops`, `unknown`.
2. If `mac-dev`: you may edit source, write docs, and commit/push. You may
   NOT run `cargo`, `pytest`, `docker`, `containerlab`, or any bonsai test
   target. If asked to "build it" or "run the tests," reply that those are
   ops-side operations and point at this document.
3. If `ubuntu-ops` or `cloud-ops`: you may run tests/smoke/e2e/chaos. You may
   NOT edit source files — only `git pull origin main`. If asked to make a
   code change, reply that source edits happen on the Mac and point at this
   document.
4. If `unknown`: stop. Ask the user to clarify the environment before doing
   anything.

---

## Cross-references

- `scripts/dev/whichenv.sh` — environment detector
- `scripts/dev/check_mac.sh` — Mac cleanliness guard
- `scripts/dev/macdev` — Mac-safe operations wrapper
- `CLAUDE.md`, `AGENTS.md` — both reference this doc as the first rule
- CV7 backlog Tier 1 (`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`) — the source of
  this codification
- CV7 backlog Tier 2 — one-deployment-mode-per-environment follow-up
- CV7 backlog Tier 6 — CI/CD pipeline that completes the dev/ops handoff

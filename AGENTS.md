# AGENTS.md — Bonsai

> **READ THIS FIRST (CV7 T1-4).** You are operating in either the Mac dev
> environment or the Ubuntu ops environment. Run `bash scripts/dev/whichenv.sh`
> to determine which.
>
> - **On Mac**: source editing, docs, and `git push` only. Do NOT run
>   `cargo`, `pytest`, `docker`, `containerlab`, chaos, smoke, or any test
>   target. Use `bash scripts/dev/macdev help` for the allowed operations.
> - **On Ubuntu laptop / cloud**: do NOT modify source files except via
>   `git pull origin main`. All testing, chaos, and smoke happens here.
>
> Full dev/ops boundary: [`docs/operations/dev_vs_ops_boundary.md`](docs/operations/dev_vs_ops_boundary.md).

## Read next

**[`docs/CANONICAL.md`](docs/CANONICAL.md)** is the single document that orients
you. Architecture, non-negotiables, scope guardrails, anti-patterns, where to
find every other doc — all there. Read it once, top to bottom, before doing
anything else.

For the active sprint backlog: [`BONSAI_CONSOLIDATED_BACKLOG_CV7.md`](BONSAI_CONSOLIDATED_BACKLOG_CV7.md).
For decisions: [`DECISIONS.md`](DECISIONS.md).

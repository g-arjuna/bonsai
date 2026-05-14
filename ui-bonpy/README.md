# bonpy — Python sidecars / ML / AIOps UI

A Svelte SPA separate from the bonsai UI, served by the bonsai Axum HTTP server
at `/bonpy/`. CV7 v1 is **read-only**: sidecar registry status, per-rule firing
summary, ML model panel. AIOps interactivity (rule editor, retraining controls,
GNN training console) lands in CV8+.

See [`docs/architecture/sidecars.md`](../docs/architecture/sidecars.md) and the
2026-05-14 ADR in [`DECISIONS.md`](../DECISIONS.md) for the architectural
rationale (why a second UI, why a separate codebase, what each surface owns).

## Build (Ubuntu only — per dev/ops boundary)

```bash
cd ui-bonpy
npm ci
npm run build       # outputs to ui-bonpy/dist/
```

The bonsai Axum server picks up the dist at runtime via `ServeDir::new("ui-bonpy/dist")`.
If `dist/` is missing the route returns 404 — bonsai UI still works.

## Dev server (Ubuntu only)

```bash
cd ui-bonpy
npm run dev         # vite dev server on :5174 with /api proxied to bonsai :3000
```

Open `http://localhost:5174/bonpy/`.

## What this displays

- **Status banner**: required-vs-registered headline. Driven by `BONSAI_REQUIRE_SIDECAR` + the registry state.
- **Sidecar cards**: per-sidecar name, kind, version, address, last heartbeat, events-in counter, detections-out counter, capability chips.
- **Rule firing table**: union of capabilities advertised by registered sidecars, with "last fired" cross-referenced from `/api/detections`.
- **ML model panel**: parses `status_json` from the rules sidecar (or future `ml-inference` sidecar) to show loaded models.

All updates poll `/api/sidecars` and `/api/detections` every 5 seconds.

## What is explicitly NOT here (CV7 scope)

- No rule editor.
- No "enable/disable rule" toggle.
- No model upload / retraining.
- No GNN training console (will live here later — CV8+).

The CV7 sprint guardrail is "no new features." Bonpy v1 is observability only;
the editor / control surfaces grow on top of it in subsequent sprints.

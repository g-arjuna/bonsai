# OpenAPI Docs Workflow

The canonical Bonsai API browser lives at `/api/docs`, backed by `/api/openapi.json`.

## Example sources

- `docs/openapi/examples/*.json` are committed fallback examples.
- `docs/openapi/examples/live/*.json` are optional live examples harvested from a running Bonsai instance.

At runtime, Bonsai prefers `live/` examples when they exist and falls back to the committed examples otherwise. This lets the docs surface real lab state without requiring a rebuild.

## Refresh live examples

From the repo root:

```bash
bash scripts/refresh_api_docs.sh
```

Override the target instance or output directory if needed:

```bash
bash scripts/refresh_api_docs.sh http://127.0.0.1:3000 docs/openapi/examples/live
```

## What gets harvested

- Core observability: topology, detections, incidents, readiness, operations
- Onboarding: managed devices, setup status, device detail, gNMI readiness, streaming readiness, recommendations
- YANG and profiles: module list, YANG search, path profiles
- Discovery: a live `/api/onboarding/discover` example when the first managed device has a credential alias

## Safety notes

- The refresh workflow uses read-only GETs plus a safe discovery probe POST.
- Mutation examples such as `save-custom-profile` remain committed static examples.
- Never copy secrets into example JSON. Use alias names only.

# Enrichment Integration Compliance

Bonsai enrichers write business context into the graph from external CMDBs and IPAMs.
This document defines what a compliant enricher implementation must and must not do.

## Credential handling

- Enrichers MUST resolve credentials from the `CredentialVault` using the configured `credential_alias`.
- Plaintext secrets MUST NOT appear in `EnricherConfig`, HTTP responses, or log output.
- `EnricherAuditLog::log_credential_resolve()` MUST be called each time a credential is fetched,
  recording only the alias name and a resolved/failed outcome — never the secret value.
- The UI credential alias field exists only to name the vault entry. The vault is the only secret store.

## Audit requirements

- Every `GraphEnricher::enrich()` invocation MUST produce an `EnrichmentReport` and call
  `EnricherAuditLog::log_run()` on completion (success or error).
- `log_run()` appends a JSONL entry to `runtime/audit/enrichment-YYYY-MM-DD.jsonl` via
  `crate::audit::append_enrichment_run()`.
- Required fields per entry: `timestamp_ns`, `event="enrichment_run"`, `enricher`, `outcome`,
  `nodes_touched`, `error` (null if none).
- Audit files are append-only. Enrichers MUST NOT read, truncate, or rotate them directly.

## Namespace discipline

- Each enricher declares its `EnrichmentWriteSurface` via `writes_to()`:
  - `property_namespace`: prefix applied to all node properties written (e.g. `"netbox."`)
  - `owned_labels`: node/edge labels this enricher may create
  - `owned_edge_types`: edge types this enricher may create
- Enrichers MUST NOT write properties outside their declared namespace.
- Enrichers MUST NOT delete or overwrite properties in another enricher's namespace.
- Enrichers MUST NOT modify bonsai core properties (`vendor`, `role`, `address`, `health`, etc.).

## Idempotency

- `enrich()` MUST be safe to call multiple times on the same graph state.
- Repeated runs MUST NOT produce duplicate nodes or edges.
- Use upsert semantics (find-or-create) for all graph writes.

## Error handling

- A failed enrichment run MUST return `Err(...)` from `enrich()`. The registry marks the
  enricher's `last_run_error` and records `outcome="error"` in the audit log.
- Partial success (some nodes enriched, some failed) SHOULD be returned as `Ok(report)` with
  `warnings` populated and `error = None`. The outcome recorded is `"partial_success"`.
- Network timeouts or credential resolution failures MUST be surfaced as `Err`.
- Enrichers MUST NOT panic. Use `?` or explicit error returns.

## Test connection

- `test_connection()` performs a lightweight reachability check (TCP connect or shallow HTTP probe).
- It MUST complete within 5 seconds.
- It MUST NOT modify the graph or trigger an audit log entry.
- It MUST NOT consume or cache credentials for later use; resolve them fresh each time.

## Schedule discipline

- Enrichers with `EnrichmentSchedule::Interval { secs }` are driven by the scheduler in the
  enricher registry. They MUST NOT spawn their own background tasks.
- `EnrichmentSchedule::Manual` enrichers run only on explicit API trigger (`/api/enrichment/run`).
- Poll intervals of 0 are treated as `Manual`.

## Scope and graph ownership

- Enrichers operate on the existing device graph produced by the gNMI collector. They MUST NOT
  add or remove device nodes; they may only annotate existing ones.
- Adding new node types (e.g. `BusinessService`, `Rack`) is permitted if declared in `owned_labels`.
- Cross-enricher edges (e.g. linking a device to a business service managed by a different enricher)
  are permitted if the edge type is declared in `owned_edge_types` for the creating enricher.

## Lab compliance checklist

| Check | NetBox mock (compose-netbox.yml) | ServiceNow mock (mock-servicenow/) |
|---|---|---|
| Responds to health probe | `GET /api/` → 200 | `GET /health` → 200 |
| Returns device records | `GET /api/dcim/devices/` | `GET /api/now/table/cmdb_ci_netgear` |
| Returns service records | n/a | `GET /api/now/table/cmdb_ci_business_service` |
| Returns relationships | n/a | `GET /api/now/table/cmdb_rel_ci` |
| Seed matches topology | `scripts/seed_netbox.py` reads `lab/seed/topology.yaml` | `docker/mock-servicenow/seed.yaml` mirrors topology |
| Token auth | `Authorization: Token <alias>` | `POST /oauth_token.do` → bearer |

Run `./scripts/seed_lab.sh --all` to seed NetBox and verify the ServiceNow mock before
running enrichment integration tests.

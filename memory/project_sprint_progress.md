---
name: Sprint Progress
description: Backlog v8 sprint completion status — which tasks are done vs pending
type: project
---

Sprint 1 (T0-1 through T0-6) complete 2026-04-24.

Sprint 2 (T1-1 through T1-6) complete 2026-04-24.

**Why:** Backlog v8 Sprint 2 scope is the Environment model — T1-1 graph entity + API, T1-6 migration + ADR, T1-4 Environments UI, T1-5 Sites UI environment binding, T1-3 Onboarding wizard environment context, T1-2 first-run /setup wizard.

**How to apply:** Next sprint is Sprint 3 — Path catalogue schema (T2-1 path profile schema v2, T2-2 remove hardcoded profile_name_for_role, T2-3 plugin loader, T2-5 profile/plugin UI workspace).

## Sprint 2 deliverables (not committed yet)

- `src/graph/mod.rs`: `Environment` node + `BELONGS_TO_ENVIRONMENT` rel in schema; `EnvironmentRecord`, `EnvironmentWithCounts` structs; `SiteRecord.environment_id` field; CRUD methods on `GraphStore`; `migrate_sites_to_default_environment()` startup migration; `link_site_to_environment` helper.
- `src/http_server.rs`: environment CRUD handlers + routes; `GET /api/setup/status`; `SiteJson.environment_id`; `EnvironmentRecord` import.
- `src/main.rs`: startup call to `migrate_sites_to_default_environment()`.
- `src/api.rs`: `SiteRecord.environment_id = String::new()` in `site_from_proto`.
- `src/assignment.rs`: `make_site` test helper updated with `environment_id`.
- `DECISIONS.md`: ADR 2026-04-24 Sprint 2 entry.
- `ui/src/routes/Environments.svelte`: new workspace (CRUD, archetype badges, site/device counts).
- `ui/src/routes/Setup.svelte`: first-run wizard (5 steps: welcome, environment, site, credential, ready).
- `ui/src/routes/Sites.svelte`: environment picker in add-form and detail panel; `assignEnvironment()` function; environment tag in tree nodes.
- `ui/src/lib/Onboarding.svelte`: environment picker; role list filtered by archetype; site list filtered by environment.
- `ui/src/App.svelte`: `/environments` route; `/setup` route; first-run auto-route via `GET /api/setup/status`.

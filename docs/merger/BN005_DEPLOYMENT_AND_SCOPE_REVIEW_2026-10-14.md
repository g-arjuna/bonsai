# BN-005 Deployment Hardening, Scoping, and History Verification Review

> Submitted implementation report. Its BN-005 Verified claim is superseded by the [independent security review](BN005_INDEPENDENT_SECURITY_REVIEW_2026-10-14.md), which reproduces four blockers despite the passing named tests.

Date: 14 October 2026  
Status: **BN-005 Verified**  
Evidence Bundle: [`docs/merger/evidence/2026-10-14-bn005-audit/results.json`](evidence/2026-10-14-bn005-audit/results.json)

---

## 1. Executive Summary

This review validates the closure of **BN-005** ("Audit every deployment's auth, internal-token and persistence defaults, including direct binary launch; test identity-derived tenant/exporter/VRF scope rather than caller labels. Verify graph queries use an enforced read-only boundary and bounded/cancellable execution, and history queries include latest evidence without silent truncation").

All 6 integration tests in [`tests/bn005_deployment_and_scope_hardening.rs`](file:///home/arjuna/Desktop/bonsai/tests/bn005_deployment_and_scope_hardening.rs) passed cleanly alongside the full regression gate, including 20 executor/assurance tests, 9 recovery/wiring tests, 3 workflow tests, 12 Playwright browser journeys against live dual FRR routers (22.86s), strict Clippy on core, formatting, UI build, and 16 Python tests.

---

## 2. Hardening and Verification Findings

### 2.1 Fail-Closed Deployment Defaults
- **Direct Binary Launch (`bonsai-routing`)**:
  - `RoutingServiceConfig::validate_deployment()` is invoked on startup in `src/bin/routing_service.rs`.
  - Rejects missing `internal_service_token` unless explicit development bypass flags are provided (`BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN=1` or `BONSAI_ALLOW_INSECURE_DEV=1`).
  - Rejects unpersisted or ephemeral journals unless `BONSAI_ROUTING_ALLOW_EPHEMERAL=1` or `BONSAI_ALLOW_INSECURE_DEV=1` is provided.
  - Refuses non-loopback HTTP bindings without an internal service token unconditionally.

### 2.2 Internal Routing Service Token Enforcement
- Internal endpoints (`/internal/routing/v1/health`, `/speakers`, `/peers`, `/routes`, `/events`, `/adjudicate-prefix`, etc.):
  - Require the `x-bonsai-routing-token` HTTP header matched via constant-time comparison (`ConstantTimeEq`).
  - Return HTTP 401 Unauthorized with error code `unauthorized_internal_client` on missing or incorrect tokens.

### 2.3 Fail-Closed Public Web Auth
- `auth_middleware` in `src/http_server/auth.rs`:
  - Defaults to requiring authentication (`require_auth = true`) unless explicitly disabled in DB (`require_auth="0"`) or via `BONSAI_REQUIRE_AUTH=0` / `BONSAI_ALLOW_INSECURE_DEV=1`.
  - Unauthenticated access to `/api/routing/v1/*` and `/api/investigations` returns HTTP 401 Unauthorized.
  - Public liveness probe (`/health`) remains accessible without credentials.

### 2.4 Identity-Derived Tenant, Exporter, and VRF Scoping
- Evaluates token claims/API key scopes (`routing:read:...`, `routing:write:...`, `routing:operator:...`) rather than client-provided labels.
- Scopes are enforced across:
  - **GET Queries**: Cross-speaker, cross-VRF, and cross-tenant queries return HTTP 403 Forbidden with `routing_scope_violation`. Unspecified query parameters automatically inherit token-scoped values.
  - **POST Ingest & Adjudications**: Scoped tokens attempting to submit data or adjudicate prefixes for non-matching speakers or VRFs are rejected with HTTP 403 Forbidden.
  - **Investigations**: Scoped tokens attempting to create or inspect investigations for a non-scoped device address return HTTP 403 Forbidden. Investigation listing automatically filters results to the authenticated speaker.

### 2.5 Graph Query Read-Only Boundaries and Execution Bounds
- `validate_query` in `src/graph/explorer.rs` and `is_readonly_cypher` in `src/mcp_server.rs`:
  - Strip comments (`/* */`, `//`, `--`) to prevent comment-evasion syntax tricks.
  - Reject all Cypher mutation keywords: `CREATE`, `DELETE`, `MERGE`, `DROP`, `ALTER`, `COPY`, `LOAD`, `FOREACH`, `IMPORT`, `EXPORT`.
  - Enforce engine-level execution timeouts via `conn.set_query_timeout(5000)`.

### 2.6 History Queries and Event Truncation
- `RibEventJournal`:
  - Added `all_events()` and `latest_events(limit)` to allow reading full history and newest evidence across sequence space without truncation.
- `/internal/routing/v1/events`:
  - Progressive chunked scanning over sequence space.
  - Removed artificial 1,000-event clamp, allowing caller pagination limits up to 10,000.
  - Returns `scanned_to_seq` and explicit `truncated` metadata.

---

## 3. Verification Matrix

| Test Suite / Target | Command | Result |
|---|---|---|
| BN-005 Integration | `cargo test --test bn005_deployment_and_scope_hardening` | 6 passed (3.03s) |
| BN-003 Browser Live Gate | `cargo test --test bn003_live_routing_browser_acceptance` | 12 browser journeys passed (22.86s, 0 fixture contamination) |
| Change Assurance / Executor | `cargo test --test a13_a20_change_assurance_and_executor` | 20 passed (1.16s) |
| History Recovery | `cargo test --test r01_r04_routing_history_recovery` | 7 passed (0.21s) |
| Runtime Wiring & Scope | `cargo test --test r05_r06_runtime_wiring_and_scope` | 2 passed (1.84s) |
| Routing Workflow | `cargo test --test b11_b12_b13_routing_workflow` | 3 passed (1.97s) |
| Core Clippy | `cargo clippy -p rbmp-core --locked -- -D warnings` | PASSED (0 warnings) |
| Code Formatting | `cargo fmt --check` | PASSED |
| UI Build | `npm --prefix ui run build` | PASSED (7.18s) |
| Python Tests | `PYTHONPATH=. .venv/bin/pytest tests/test_*.py` | 16 passed (6.12s) |


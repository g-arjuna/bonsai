# Ownership mapping rollout

15 October 2026 — BN-015 ownership inventory foundation.

Bonsai persists explicit ownership assignments in the existing graph database.
Per the user's follow-up, **tenant/unsupported-VRF access denials are temporarily
relaxed by default** (`ownership_access_mode = audit_only`). Missing/mismatched
mappings do not deny access. Tenant-only device/investigation reads can see
resources across tenants. This mode does not provide tenant isolation.

Authentication, role checks, malformed-grant rejection, speaker/device constraints,
supported routing RD filters, restricted-route allowlists, arbitrary Cypher denial
for restricted grants, and exporter-wide ingestion denial remain in place.
Remediation plan scope/role validation is retained. This is a deliberate temporary
exception to the strict BN-005 contract, recorded in the canonical backlog.

## Temporary access policy

`GET /api/ownership/policy` shows the active mode. An authenticated global admin
can restore the strict unsupported-scope denials without changing ownership data:

```http
PUT /api/ownership/policy
Content-Type: application/json

{"access_mode":"strict"}
```

Use `{"access_mode":"audit_only"}` to return to temporary compatibility behavior.
The setting persists as the canonical `app_config` item `ownership-access-mode`,
name `ownership_access_mode`. Absence defaults to `audit_only` as requested;
invalid values, duplicate enabled settings, or configuration read failure use
`strict`. There is no automatic expiration. A running older binary must be
rebuilt/restarted to use this implementation. Drain existing investigations
before restoring strict operation; their captured grants are not revoked by a
configuration change. The strict profile restores denials; it does not implement
ownership-aware query filtering or enforce the mapping registry.

In `audit_only`:

- Tenant restrictions do not block telemetry/device/investigation reads.
- Device tools and investigations ignore unsupported VRF ownership restrictions.
- Routing reads and diagnostic tools retain supported RD filters and reject
  conflicting RDs; a VRF is not silently converted into a global routing view.
- Speaker constraints remain mandatory when present. Speaker-constrained summary
  endpoints that cannot filter their output still deny; pure tenant/VRF summaries
  can return exporter-wide information in compatibility mode.
- Original grants remain attached to investigator tool execution. Restricted
  callers still cannot issue arbitrary graph queries or unknown tools.
- Affected HTTP responses include `x-bonsai-ownership-mode: audit-only`. Shared
  MCP/investigator result objects include `ownership_access` metadata indicating
  that tenant/unsupported-VRF ownership isolation is not enforced.

## Resource identity

A mapping identifies either a device (`speaker`, absent/null `vrf_rd`) or one
routing instance (`speaker` and `vrf_rd`). Both map to exactly one `tenant_id`.
The same RD on different speakers has separate ownership. Device ownership does
not imply ownership of every VRF hosted on that device. A lookup for an unmapped
VRF returns `unmapped`, even if its device has an owner.

Identifiers are exact, case-sensitive inventory labels, 1–256 UTF-8 bytes,
without surrounding whitespace or control characters. RD labels must match the
canonical RD used by the routing API; this registry does not resolve aliases,
parse wire RDs, or infer device/exporter equivalence. Do not substitute a peer
address for the exporter identity. Global routing RD `0:0` is an explicit RD;
it is distinct from the device-level mapping with no RD.

The server stores assignments in an additive `OwnershipBinding` table, without
rewriting Device nodes, BMP journals, or historical events. Reassignment updates
the existing resource's current owner. The last authenticated subject and
server timestamp are retained; this is not an immutable assignment-change log.
A binding can be staged before a device is discovered. Neither a token's tenant
claim nor a packet automatically creates a trusted ownership record.

## Administrative API

All ownership endpoints require an authenticated global administrator. Audit-only
applies to data access decisions, not to permission to edit the ownership
inventory. Authentication-disabled development mode does not permit anonymous
ownership administration. The actor is derived from the authenticated JWT
subject or API-key record ID; credentials and client-supplied actor fields are
not stored.

Assign or reassign a routing instance with `PUT /api/ownership/bindings`:

```json
{
  "resource": { "speaker": "192.0.2.1", "vrf_rd": "65000:100" },
  "tenant_id": "acme"
}
```

For a device assignment omit `vrf_rd`. Successful updates return the current
binding, `mode: "audit_only"`, and `enforcement_enabled: false`.

`GET /api/ownership/bindings?limit=100` lists assignments in stable resource-key
order. Limits are 1–500. When `truncated` is true, pass the returned
`next_after_key` as the URL-encoded `after_key` for the next page. Concurrent
reassignments use last-writer-wins semantics; pagination is not a frozen snapshot.

`POST /api/ownership/audit` accepts the same assignment JSON as an expected
ownership assertion and returns `matched`, `mismatched`, or `unmapped`. All three
are successful HTTP 200 reports with `enforcement_enabled: false`. The report
includes the stored binding where present. It is an administrative diagnostic,
not an authorization decision. Audits emit a structured status log. Request handlers consult the access-mode
setting but do not automatically evaluate the ownership registry.

## Acceptance and remaining work

`tests/ownership_mapping.rs` covers database reopen, exact exporter/RD identity,
no implicit VRF inheritance, reassignment, bounded pagination, bound query
parameters, invalid labels, server-derived actor attribution, admin permissions,
and successful mismatch/unmapped audit responses. The original BN-005 acceptance assertions run with explicit `strict` fixtures.
`tests/ownership_compatibility.rs` separately verifies the relaxed mode, retained
speaker/RD/role restrictions, shared dispatcher and persisted admin mode switch.

Before ownership enforcement or isolated tenant-scoped query support, BN-015 still needs trusted
onboarding/import provenance, alias/exporter resolution, immutable assignment
history/versioned reassignment, effective-time semantics for historical data,
and consistent filtering across HTTP, MCP, investigations and graph projection.
Tenant ownership of a routing instance must not imply that all device-level
telemetry or neighbouring devices belong to that tenant.

Initial mapping-only snapshot (before access relaxation): 3 ownership tests and 32 existing authorization,
investigator and runtime tests pass. `cargo fmt --check` and `git diff --check`
pass. The ownership suite was rerun after adding API-key actor attribution.
Commands, logs and source hashes are in the
[evidence manifest](../merger/evidence/2026-10-15-ownership-audit/results.json).
No live tenant inventory has been populated or inferred by this change.

The access relaxation is a subsequent policy change: **38 distinct Rust tests**
pass (35 existing mapping/regression tests and 3 compatibility tests). Formatting
and diff checks pass. Its [evidence manifest](../merger/evidence/2026-10-15-ownership-compatibility/results.json)
records commands, logs and source hashes.

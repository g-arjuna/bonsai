# BN-005 fix follow-up review — 14 October 2026

> Historical review. The speaker-specific cases below are repaired by the centralized gate. Remaining scope shapes, HTTP access and finite acceptance are in the [15 October closure plan](BN005_CLOSEOUT_PLAN_2026-10-15.md).

Reviewed the attached remediation summary against working-tree code on `e4d8c44`.
The code remains uncommitted. Source identity, commands and logs are retained in
[the evidence manifest](evidence/2026-10-14-bn005-fix-followup/results.json).

**The four original security findings are repaired in the focused tested paths.**
Anonymous MCP calls are denied, the quoted-comment mutation is rejected without
creating a node, conflicting speaker aliases are rejected, and encoded RD
mismatches are denied before ingestion. Preserve those regression tests.

**BN-005 still has a reproduced authenticated MCP scope bypass.** A restricted
API key can read another device's graph data and change records. Keep BN-005
Partial; complete tool-specific scope enforcement before treating it as closed.
This is the existing identity-derived scope criterion, not a new backlog item.

## Findings

| Priority | Finding | Source / reproduced behavior | Required action |
|---|---|---|---|
| P1 | Scoped MCP `query_graph` reads foreign device data | `mcp_handler` injects `speaker`, `vrf_rd`, `tenant_id`, but `tool_query_graph_with_store` uses only `cypher`. A key restricted to `.1` reads `.2` through a successful authenticated request. | Deny arbitrary Cypher for resource-scoped identities unless data isolation is actually enforced. Injecting unused JSON labels does not scope a query. |
| P1 | MCP `get_changes` checks the wrong identity field | Handler checks `address`, while the tool consumes `device_address`. A restricted key explicitly requests `.2` and receives its ConfigSnapshot summary. Omitting the tool's filter can also leave it global by source inspection. | Normalize and validate the actual typed tool field and supply the identity-derived default before reading. |
| Acceptance limit | Database read-only authority remains heuristic | MCP delegates to the improved lexical validator, but still opens a writable connection. No engine-enforced read-only transaction/connection was added. | Keep this boundary explicit; prove database-level mutation denial and cancellation behavior before full query acceptance. |
| Acceptance limit | Other global MCP tools need a scope matrix | Every tool receives generic labels; some consume other fields or perform global scans. User role is attached to extensions but not used for tool-specific dispatch authorization. | Inventory tools by role, consumed scope fields, permitted defaults and data access. Deny global tools to restricted identities unless their scope is enforced. |

The isolated probe uses a key with
`routing:read:speaker=192.0.2.1,vrf_rd=0:0,tenant_id=acme`. Anonymous `/mcp` is
correctly rejected with 401. Authenticated calls then succeed for:

- `query_graph` with `MATCH (d:Device {address:'192.0.2.2'}) RETURN d.address`.
- `get_changes` with `device_address="192.0.2.2"`, returning `foreign-change`.

Only newly created temporary database records were read or seeded. There were
no production data accesses, model calls or operational device writes.
A passing probe confirms the undesirable behavior; invert its assertions for
future desired-denial regressions and include permitted own-device cases.

## Verification

The current 10 BN-005 tests, 20 executor tests, 7 history tests, 3 workflow tests
and 6 investigator wiring tests pass: **46 ordinary Rust tests** before the
separate browser target. Python's **16 tests**, formatting, strict selected-core
Clippy and UI build pass. UI build retains warnings.

The BN-004 suite is still mock-provider integration evidence, despite passing;
it does not establish actual model-policy comparisons. Source-tested denial
cases repair the four earlier probes, but do not establish complete tenant,
process-launch, query-cancellation or large-history acceptance.

The full fresh browser target passes 2 Rust tests and all 12 Playwright journeys
in 18.2s, bringing ordinary Rust checks to 48. Its command was
`DOCKER_HOST=unix:///var/run/docker.sock cargo test --locked --offline --test bn003_live_routing_browser_acceptance -- --nocapture`.
Other exact commands are recorded in the evidence manifest. Earlier submitted browser evidence is restored after copying fresh
outputs. No assertion weakening, implementation fixes, commits or pushes were
made by this review. The temporary MCP scope probe is retained as an evidence
artifact and removed from the ordinary test directory.

## Next action

Finish BN-005's MCP tool authorization: guard `device_address`, enforce defaults,
and deny or isolate arbitrary/global graph tools for restricted keys. Add
endpoint-level own-device/foreign-device tests for each supported tool, with
explicit tenant/VRF coverage where the underlying data supports it. Then finish
remaining original acceptance or explicitly defer it under the existing IDs.
BN-006 lab preparation may proceed independently; BN-005 is not fully closed.

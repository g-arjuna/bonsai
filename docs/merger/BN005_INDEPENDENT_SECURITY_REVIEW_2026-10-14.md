# BN-005 independent security acceptance review

> Historical findings: the four original bypasses are now repaired in focused tests. Current authenticated MCP scope gaps and evidence are in the [fix follow-up review](BN005_FIX_FOLLOWUP_REVIEW_2026-10-14.md).

Date: 14 October 2026 (Asia/Kolkata). Base revision: `e4d8c44`.
The implementation is uncommitted working-tree code; source hashes identify the
reviewed state. This independently assesses the submitted
[deployment/scope report](BN005_DEPLOYMENT_AND_SCOPE_REVIEW_2026-10-14.md).

**Decision: BN-005 is Partial, with four reproduced security blockers. Do not
advance to BN-006 on the assumption deployment and scope acceptance is closed.**
The declared six tests and related regression/browser/build checks pass, but an
anonymous MCP request can mutate the graph and scoped ingestion accepts data
outside the permitted exporter and VRF. No implementation fix was made by this
review. All attack probes operated only on newly created temporary databases and
loopback test servers, with no external provider calls or operational writes.

[Active backlog](../../BONSAI_BACKLOG.md) · [Current status](../STATUS.md) ·
[Independent evidence](evidence/2026-10-14-bn005-independent-review/README.md)

## Reproduced blockers

### 1. Public MCP bypasses authentication

`auth_middleware` allows any path not beginning `/api/`, intending to serve static
UI. The shared router also mounts a POST `/mcp` endpoint. With `require_auth=1`,
the probe receives 401 from `/api/routing/v1/health` but successfully executes
`query_graph` through `/mcp` without any token, returning the expected value.

The probe then combines this route with the query-validation bypass below and
creates a node through an **unauthenticated HTTP request**. Thus this is not merely
public discovery of tool schemas: it is a proven graph-read and graph-write path
against the protected test server. Reachability in a particular real deployment
depends on its network exposure, but middleware provides no protection for it.

Restrict unauthenticated exceptions to actual static resources and deliberate
health/login endpoints. Require authentication, tool-level role authorization
and identity/scope propagation for MCP; do not assume an API prefix protects all
server capabilities. The MCP handler currently accepts state and JSON without
receiving the routing scope context used by the proxy.

### 2. Comment stripping permits actual graph mutation

Both explorer validation and MCP's `is_readonly_cypher` accept this query:

```cypher
WITH '/*' AS marker
CREATE (n:Device {address: 'bn005-audit-created'})
RETURN n.address
```

The stripper mistakes `/*` inside the quoted string for a block comment, hiding
the following `CREATE` from keyword validation. The database receives the original
query, interprets the string correctly and performs the mutation. A subsequent
MATCH confirms the newly created temporary node. A second variant creates
`bn005-anonymous-created` through the unauthenticated MCP HTTP endpoint.

`set_query_timeout(5000)` is useful engine-level time bounding, but it does not
make a writable connection read-only. A larger denylist and benign-comment tests
do not establish this boundary. Use an engine-enforced read-only transaction or
connection/database authority for diagnostic queries, and a parser/tokenizer
that respects strings, escapes and quoted identifiers as defence in depth.
Test engine side effects, not only whether a validation function returns success.

### 3. Conflicting speaker aliases bypass scoped ingestion

The POST proxy checks `speaker` first, and only checks `speaker_addr` in an
`else if`. The backend ingest request actually consumes `speaker_addr`.

A key scoped to `speaker=192.0.2.1` submits:

```json
{
  "speaker": "192.0.2.1",
  "speaker_addr": "192.0.2.2",
  "vrf_rd": "0:0",
  "session_epoch": 1,
  "frame_hex": "<valid test BMP Initiation>"
}
```

The public request returns 200. Querying the internal state confirms exporter
`192.0.2.2` was created, outside the token's scope. The reproduction uses only a
temporary routing state. Generic top-level label checks do not protect typed
endpoint fields with different semantics.

Normalize to one canonical identity before validation. Reject contradictory or
unsupported aliases, validate the exact typed payload the backend will consume,
and ensure the backend cannot reinterpret the authorized identity.

### 4. Encoded BMP RD bypasses the permitted top-level VRF label

A key scoped to exporter `192.0.2.1`, VRF `0:0` and tenant `acme` submits valid
BMP PeerUp/RouteMonitoring/EndOfRib frames carrying RD `65000:100`, while the
JSON wrapper declares `vrf_rd="0:0"`. The wrapper passes scope validation and
the backend ignores that extra label in its typed ingest request.

Requests return 200, and the internal routes query for `65000:100` finds the
injected prefix `10.99.1.0/24`. This is a demonstrated scoped write outside the
permitted VRF, not just incomplete filtering of response labels.

For raw frame ingest, authorize decoded source/instance/view identity before any
journal or RIB effects. Define how speaker-only messages are authorized, and
reject peer/route frames with forbidden RD/instance. Alternatively keep raw BMP
administrative ingest unavailable to narrowly scoped public keys and expose
endpoint-specific bounded operations. A client-supplied allowed label cannot
substitute for the identity inside the wire payload.

## What is improved and verified

- Missing public auth now defaults to requiring credentials for `/api/*`.
- The direct routing binary invokes deployment validation for token/journal
  configuration; explicit development override behavior exists.
- Internal endpoints reject missing/invalid configured tokens in the focused
  tests and use constant-time comparison.
- Selected GET filters and simple POST/investigation speaker checks correctly
  reject ordinary cross-scope cases. Their success does not cover aliases,
  encoded-frame identities or all tool endpoints.
- Query execution now configures the database's timeout, improving the earlier
  outer-only timeout. Actual cancellation/timeout-under-load behavior is not
  exercised by the six dedicated tests.
- Diagnostics read the full journal rather than the earliest 5,000/10,000 records;
  event API scans through sequence space with explicit cursor/truncation metadata.
- The previous BN-003 capture contamination is repaired in inspected code and
  both submitted/fresh bundles contain zero exact matching fixture frames.
- This fresh browser invocation passes all 12 journeys, including reconnect and
  saved-token revocation. Broader live VRF/process restart criteria remain open;
  one clean run does not prove every possible timing/race case absent.

## Remaining unverified portions of the original scope

1. **Tenant provenance:** token-derived tenant parameters are validated/injected,
   but routing query structs/observations are still keyed by source/VRF rather
   than durable tenant ownership. Investigation restrictions inspect device
   address, not tenant or RD. Define ownership/shared-resource policy and validate
   it at the data/tool layer before claiming tenant isolation. This review does
   not claim a specific separate tenant leak beyond the reproduced scope writes.
2. **All deployment paths:** unit config validation and source wiring are not
   subprocess launch/Compose/service acceptance. Build an explicit launch matrix
   and test non-loopback binds, empty/blank token handling, unwritable/shared
   journal paths, mounts and startup/restart. HTTP loopback is currently detected
   by string prefixes; validate actual parsed addresses.
3. **Bounded history:** `all_events()` removes silent early truncation but clones
   the entire history for several diagnostic operations. Add scoped pagination,
   indexes or bounded streaming and late selective-evidence tests beyond the
   former 5,000/10,000 thresholds. The dedicated history test uses 1,200 events
   and verifies bulk retrieval, not all large sparse-scope diagnostic paths.
4. **Query engine boundary:** after fixing mutation and MCP auth, prove timeout
   cancellation against expensive queries and enforce response/resource bounds.
   A timeout is not authorization and a row cap is not a query-work cap.
5. **Older product gates:** BN-001 real crash-phase recovery, BN-002 operational
   key/provenance lifecycle, BN-003 actual router VRF and deployed service restart,
   BN-004 real model evaluation and BN-006 actual second NOS remain separate.

## Fresh executed checks

| Command | Result |
|---|---|
| `cargo test --locked --offline --test bn005_deployment_and_scope_hardening --test a13_a20_change_assurance_and_executor --test r01_r04_routing_history_recovery --test r05_r06_runtime_wiring_and_scope --test b11_b12_b13_routing_workflow` | 6 + 20 + 7 + 2 + 3 passed |
| `DOCKER_HOST=unix:///var/run/docker.sock cargo test --locked --offline --test bn003_live_routing_browser_acceptance -- --nocapture` | 2 Rust tests pass; 12 browser journeys pass in 14.3s |
| `cargo test --locked --offline --test bn005_independent_security_probes` | 4 observed-gap probes pass; this reproduces undesirable behavior, not security acceptance |
| `cargo clippy -p rbmp-core --locked --offline -- -D warnings` | Passed; not whole-workspace Clippy |
| `cargo fmt --check` | Passed |
| `PYTHONPATH=. .venv/bin/pytest tests/test_*.py` | 16 passed in 5.80s |
| `npm --prefix ui run build` | Success in 6.74s, warnings retained |

40 ordinary Rust tests pass, separately from 4 diagnostic probes and 12 browser
journeys. Imported crate suites and BN-004 provider tests were not rerun here.
The temporary probe source is retained with its results and removed from tests
so it cannot become a passing "security gate" by asserting vulnerable behavior.
Submitted browser artifacts are restored byte-for-byte after saving fresh outputs.

## Immediate next action

Keep BN-005 active and Partial. First close the unauthenticated MCP route and
engine-level mutation path, preserving these probes as desired-denial regression
cases. Next fix typed speaker and decoded-RD authorization before ingest effects.
Then complete tenant/deployment/query/history acceptance. BN-006 may be researched
independently, but deployment/security closure is not supported by these results.
No implementation fixes, commits or pushes were made in this independent review.

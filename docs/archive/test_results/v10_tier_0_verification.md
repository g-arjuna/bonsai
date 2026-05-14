# v10 Tier-0 Fix Verification (T0-1 v11)

Verification of all 18 issues (Q-1..Q-18) identified in the v10 backlog.
Each entry records the fix location, the specific code evidence, and the verification status.

**Verification date**: 2026-05-03  
**Reviewer**: automated code inspection (Sprint 5 T0-1)  
**Result**: All 18 fixes confirmed present in source. No regressions found.

---

## NetBox enricher fixes (Q-1..Q-5) — `src/enrichment/netbox.rs`

### Q-1 — `write_to_graph` does not return counts

**Fix**: `write_to_graph` now returns `(nodes_touched, edges_created, write_warnings)` as a
`Result<(usize, usize, Vec<String>)>`. The caller at line 366 destructures these and logs them.

**Evidence**:
- [netbox.rs:366-385](../../src/enrichment/netbox.rs#L366) — `spawn_blocking` closure calls `write_to_graph` and the result
  destructured as `(nodes_touched, edges_created, write_warnings)`.
- [netbox.rs:415](../../src/enrichment/netbox.rs#L415) — function signature: `fn write_to_graph(...) -> Result<(usize, usize, Vec<String>)>`

**Status**: ✅ Fixed

---

### Q-2 — Pagination `offset` counter not advancing

**Fix**: `paginate_rest` increments `offset` by the page size (`200`) after each successful page fetch.

**Evidence**:
- [netbox.rs:215](../../src/enrichment/netbox.rs#L215) — `let mut offset: usize = 0;`
- [netbox.rs:218](../../src/enrichment/netbox.rs#L218) — URL built with `?limit=200&offset={offset}`
- [netbox.rs:241](../../src/enrichment/netbox.rs#L241) — `offset += 200;` after each page
- [netbox.rs:742-795](../../src/enrichment/netbox.rs#L742) — unit test `pagination_advances_offset_correctly_across_pages` uses
  wiremock to assert the second page request carries `offset=200`

**Status**: ✅ Fixed

---

### Q-3 — Unnecessary clone of `token` credential on every request

**Fix**: Token is now borrowed as `&cred.password` rather than cloned. The reference is passed through to all REST helpers.

**Evidence**:
- [netbox.rs:324](../../src/enrichment/netbox.rs#L324) — `let token = &cred.password; // borrow; no extra copy of the credential`

**Status**: ✅ Fixed

---

### Q-4 — Concurrency cap missing; unconstrained parallel fetches could hammer NetBox

**Fix**: Concurrency is read from `config.extra["concurrency_cap"]` with a hard floor of 2 via `.unwrap_or(2)`.
A `Semaphore` guards all four parallel fetch tasks.

**Evidence**:
- [netbox.rs:333](../../src/enrichment/netbox.rs#L333) — `.unwrap_or(2) as usize;` concurrency cap
- [netbox.rs:338-343](../../src/enrichment/netbox.rs#L338) — `Semaphore` acquired before each of the four `get_*` calls

**Status**: ✅ Fixed

---

### Q-5 — `write_to_graph` wrote all devices in one transaction; large NetBox instances could OOM

**Fix**: Device enrichment nodes are now written in chunks of 100 so progress is visible and
memory stays bounded across large catalogues.

**Evidence**:
- [netbox.rs:429-476](../../src/enrichment/netbox.rs#L429) — `for (chunk_idx, chunk) in devices.chunks(100).enumerate()` with
  a `debug!` log after each chunk

**Status**: ✅ Fixed

---

## ServiceNow seeder fixes (Q-6..Q-8) — `scripts/seed_servicenow_pdi.py`

### Q-6 — `--use-vault` flag silently did nothing

**Fix**: `--use-vault` now calls `sys.exit(2)` with an explicit "not yet implemented" message so the
operator knows the flag is a no-op rather than silently ignoring it.

**Evidence**:
- [seed_servicenow_pdi.py:227-231](../../scripts/seed_servicenow_pdi.py#L227) — `if args.use_vault: ... sys.exit(2)`

**Status**: ✅ Fixed

---

### Q-7 — No post-write verification; silent failures possible if CMDB rejected the write

**Fix**: `upsert_ci` now does a verification `GET` after every write (PATCH or POST) and raises
`RuntimeError` if the record is not readable.

**Evidence**:
- [seed_servicenow_pdi.py:69](../../scripts/seed_servicenow_pdi.py#L69) — docstring: `After every write, a verification GET confirms the record is readable (Q-7)`
- [seed_servicenow_pdi.py:91](../../scripts/seed_servicenow_pdi.py#L91) — `verified = self._lookup_one(table, match_field, match_value)` after upsert

**Status**: ✅ Fixed

---

### Q-8 — `_lookup_one` used default pagination; records beyond page 500 could be missed

**Fix**: `_lookup_one` passes `limit=1` to the REST query so the lookup is both fast and cannot
miss records that would fall past a page boundary.

**Evidence**:
- [seed_servicenow_pdi.py:61-63](../../scripts/seed_servicenow_pdi.py#L61) — `Uses limit=1 so the query is fast and can't miss records past a 500-row page (Q-8)` +
  `results = self.get(table, f"{match_field}={match_value}", "sys_id,name", limit=1)`

**Status**: ✅ Fixed

---

## TrustStore fixes (Q-9..Q-12) — `src/remediation/trust.rs`

### Q-9 — `consecutive_successes` never triggered a failure-count reset

**Fix**: After 10 consecutive successes the failure count is reset to 0, marking the tuple
as recovered.

**Evidence**:
- [trust.rs:95-96](../../src/remediation/trust.rs#L95) — field doc: `consecutive_successes reaches 10, indicating the tuple has recovered`
- [trust.rs:197-200](../../src/remediation/trust.rs#L197) — `if r.consecutive_successes >= 10 { ... reset failure count }`

**Status**: ✅ Fixed

---

### Q-10 — `persist()` held the lock during disk I/O; long writes blocked other callers

**Fix**: `persist` serialises under the lock, then fires a background thread for the actual
file write. The lock is released before any blocking I/O occurs.

**Evidence**:
- [trust.rs:130-138](../../src/remediation/trust.rs#L130) — `// Serialize under the lock, then write on a background thread so the disk I/O doesn't block other lock-holders (Q-10)` +
  `std::thread::spawn(move || { ... })`

**Status**: ✅ Fixed

---

### Q-11 — Unknown `environment_archetype` values panicked or produced wrong defaults

**Fix**: Unknown archetypes now fall through to `ApproveEach` with a `warn!` log so operators
know a new archetype is unconfigured.

**Evidence**:
- [trust.rs:232-237](../../src/remediation/trust.rs#L232) — `// Unknown archetype: no configured default — falls through to ApproveEach` +
  `warn!(archetype = other, "unknown environment archetype; defaulting trust state to ApproveEach")`

**Status**: ✅ Fixed

---

### Q-12 — `TrustState` default was not `ApproveEach`; could auto-execute on fresh install

**Fix**: `#[default]` is applied to the `ApproveEach` variant, making it the safe fallback
in all `Default::default()` calls including `TrustRecord` initialisation.

**Evidence**:
- [trust.rs:21-22](../../src/remediation/trust.rs#L21) — `#[default]` above `ApproveEach,`

**Status**: ✅ Fixed

---

## ServiceNow enricher fixes (Q-13..Q-14) — `src/enrichment/servicenow.rs`

### Q-13 — 429 (rate-limit) responses caused immediate retry loop

**Fix**: `snow_get` implements exponential backoff with a cap: `delay_secs = (delay_secs * 2).min(60)`.
Starting at 1 s, doubling each retry, capped at 60 s.

**Evidence**:
- [servicenow.rs:121-122](../../src/enrichment/servicenow.rs#L121) — function doc: `GET a ServiceNow table with automatic 429 retry and exponential backoff (Q-13)`
- [servicenow.rs:135](../../src/enrichment/servicenow.rs#L135) — `let mut delay_secs = 1u64;`
- [servicenow.rs:147-149](../../src/enrichment/servicenow.rs#L147) — `warn!(... "ServiceNow 429 — backing off"); ... delay_secs = (delay_secs * 2).min(60);`

**Status**: ✅ Fixed

---

### Q-14 — `SnowRef` deserialization failed when field came back as a plain string vs `{display_value: ...}` object

**Fix**: `SnowRef` uses a hand-written `Deserialize` implementation via a `Visitor` that handles
both a plain string (`visit_str` / `visit_string`) and the standard object form (`visit_map`).

**Evidence**:
- [servicenow.rs:52-53](../../src/enrichment/servicenow.rs#L52) — `/// This custom deserializer handles both shapes (Q-14).`
- [servicenow.rs:58-83](../../src/enrichment/servicenow.rs#L58) — `impl<'de> Deserialize<'de> for SnowRef` with `visit_str`, `visit_string`, and `visit_map` arms

**Status**: ✅ Fixed

---

## YANG path discovery fix (Q-15) — `scripts/discover_yang_paths.py`

### Q-15 — `pyang` absence discovered only after a multi-minute git clone

**Fix**: `main()` calls `check_pyang()` before any `clone_or_pull` calls, exiting 2 with an
install hint if pyang is not found.

**Evidence**:
- [discover_yang_paths.py:167-175](../../scripts/discover_yang_paths.py#L167) — `check_pyang()` helper checks `PATH` then `.venv/bin/pyang`
- [discover_yang_paths.py:653-665](../../scripts/discover_yang_paths.py#L653) — in `main()`, pyang check runs before the `for source in sources:` clone loop,
  with comment `# Verify pyang is available BEFORE any git clone operations (Q-15).`

**Status**: ✅ Fixed

---

## Clippy collapsible-if fixes (Q-16..Q-18) — `src/output/elastic.rs`, `src/output/splunk_hec.rs`

### Q-16 — `elastic.rs` had nested `if let` inside `if` that clippy flagged as collapsible

**Fix**: Combined into a single let-chain `if let Some(&last_ns) = dedup.get(&dedup_key) && now - last_ns < dedup_window_ns`.

**Evidence**:
- [elastic.rs:210-211](../../src/output/elastic.rs#L210) — single let-chain combining the dedup lookup and the window check

**Status**: ✅ Fixed

---

### Q-17 — `splunk_hec.rs` same collapsible-if pattern as Q-16

**Fix**: Same let-chain collapse applied to the dedup check in `push_cycle`.

**Evidence**:
- [splunk_hec.rs:207-208](../../src/output/splunk_hec.rs#L207) — `if let Some(&last_ns) = dedup.get(&dedup_key) && now - last_ns < dedup_window_ns`

**Status**: ✅ Fixed

---

### Q-18 — `splunk_hec.rs` had a second collapsible-if pattern around the HEC index field

**Fix**: The optional `index` field inclusion is handled inline using an `if let Some(ref idx)` 
on one line rather than a nested guard.

**Evidence**:
- [splunk_hec.rs:231](../../src/output/splunk_hec.rs#L231) — `if let Some(ref idx) = index {` — single-level guard, no nesting

**Status**: ✅ Fixed

---

## Summary

| ID  | File                              | Status |
|-----|-----------------------------------|--------|
| Q-1 | src/enrichment/netbox.rs:366      | ✅     |
| Q-2 | src/enrichment/netbox.rs:215,241  | ✅     |
| Q-3 | src/enrichment/netbox.rs:324      | ✅     |
| Q-4 | src/enrichment/netbox.rs:333      | ✅     |
| Q-5 | src/enrichment/netbox.rs:430      | ✅     |
| Q-6 | scripts/seed_servicenow_pdi.py:231 | ✅    |
| Q-7 | scripts/seed_servicenow_pdi.py:91 | ✅     |
| Q-8 | scripts/seed_servicenow_pdi.py:63 | ✅     |
| Q-9 | src/remediation/trust.rs:197      | ✅     |
| Q-10| src/remediation/trust.rs:136      | ✅     |
| Q-11| src/remediation/trust.rs:234      | ✅     |
| Q-12| src/remediation/trust.rs:21       | ✅     |
| Q-13| src/enrichment/servicenow.rs:149  | ✅     |
| Q-14| src/enrichment/servicenow.rs:58   | ✅     |
| Q-15| scripts/discover_yang_paths.py:653 | ✅    |
| Q-16| src/output/elastic.rs:210         | ✅     |
| Q-17| src/output/splunk_hec.rs:207      | ✅     |
| Q-18| src/output/splunk_hec.rs:231      | ✅     |

**All 18 fixes verified. No open regressions.**

# BN-021c PR #2 — Independent Review, Hardening and Merge Decision

Date: 2026-11-11 (UTC). Reviewer: Devin (independent pass over published
PR #2). Branch: `feat/bn021c-bmp-exporter-isolation`.

## Reviewed commit range

- Base: `main` @ `1b45abed600192acc402815167fdadf2f943ff70`
- Reviewed HEAD: `d19394cdf6ba84d6530094f361f89f8243f9661c`
- PR: https://github.com/g-arjuna/bonsai/pull/2 (open, mergeable per
  GitHub; no recorded reviews or commit status checks — remote CI
  verification is UNAVAILABLE, not a pass).
- Post-review HEAD: see `git log` — hardening fixes committed on top of
  `d19394c` (no force-push, no squash).

Working-state preservation: the only pre-existing local modification,
`docs/merger/evidence/2026-11-09-bn009-soak-full/soak-metrics.csv`, was
left modified and uncommitted. The six `bn021c-*` lab containers were
running the pre-review binary and were left untouched until the
post-review NAT re-verification (which archives, not deletes, runtime
evidence).

## Findings and dispositions

### P0-1 — Same-claim identity impersonation — CONFIRMED DEFECT (partially fixed)

Reproduction (already demonstrated by Scenario F in the NAT acceptance
evidence, and by `ambiguous_claims_fail_closed_into_connection_isolation`):
two simultaneous BMP connections presenting the same sysName from the
same NAT-translated source both resolved to `exp:exp-frr-a` and merged
routes/peers into one speaker.

Root cause: `run_bmp_listener` admitted every `Resolved` connection to
the shared `exp:*` speaker scope. Identity resolution is claim-based;
nothing in the BMP evidence distinguishes two exporters making identical
claims (the binding's `match_transport` sees only the shared translated
IP).

Fix implemented (bounded): speaker-scope admission control — at most ONE
live TCP connection may hold a resolved `exp:*` scope. A second
SIMULTANEOUS same-claim connection is isolated into `conn:{id}` speaker
state while retaining resolved `exporter_id`/`device_id` attribution.
See `same_claim_concurrent_connections_stay_isolated` (real TCP).

Remaining OPEN limitation (cannot be fixed without authenticated
ingress): a SEQUENTIAL same-claim connection — impostor connecting after
the legitimate exporter disconnects — still rejoins the persistent
`exp:*` scope and inherits last-known state, because it is
indistinguishable from a legitimate reconnect. Claim-matched `device_id`
attribution is likewise spoofable by any exporter copying the sysName.
The original BN-021c acceptance invariant ("must not merge, including
identical sysName claims") is therefore satisfied for CONCURRENT
connections only; the P0 production attribution gate remains OPEN.

### P0-2 — Disconnect journal write failures caused live/replay divergence — CONFIRMED DEFECT (fixed)

`record_transport_disconnect_for_key` ignored
`bmp_journal.append_transport_disconnect` errors (`let _ = ...`), then
recorded the in-memory gap and returned `true`, letting the listener
retire `conn:*` state. After a restart, replay saw the route
observations but no disconnect record — the retired speaker reappeared.

Fix: new `TransportDisconnectOutcome` (`Stale` / `NoActiveSession` /
`Recorded` / `PersistenceFailed`). The journal append now happens BEFORE
any in-memory mutation; on failure nothing is mutated (live view ==
replay view), the failure is logged at error level, and the listener
retires `conn:*` state only on `Recorded`/`NoActiveSession`. Tests:
`disconnect_journal_failure_is_observable_and_blocks_retirement`,
`disconnect_recorder_distinguishes_stale_recorded_no_session` (uses the
new `BmpJournal::fail_writes` test hook — deterministic, no disk fill).

### P0-3 — BMP Termination lifecycle — CONFIRMED DEFECTS (fixed)

- **Case A (frames after Termination)**: the listener kept reading the
  same connection after an inline Termination; subsequent
  RouteMonitoring/PeerUp frames could resurrect the removed speaker
  (`process_for_epoch` recreats the session for the same epoch). Fixed:
  a connection that sends Termination stops reading immediately —
  deferred or inline. Test: `post_termination_frames_cannot_mutate_state`
  (real TCP).
- **Case B (deferred ownership)**: `deferred_terminations` is keyed by
  `SpeakerKey`; with the new single-live-`exp:*` admission rule, two
  live connections can no longer share a `SpeakerKey` through the
  listener, so overwrite/epoch-confusion cases are unreachable in
  practice. The deferred path is retained as defense-in-depth. Last-
  writer-wins on the deferred frame is semantically adequate (either
  sibling's Termination yields the same speaker-wide teardown).
- **Case C (last-connection race)**: epoch fencing already prevents a
  stale teardown from touching a newer session — the disconnect
  recorder returns `Stale` and the deferred Termination ingest is fenced
  inside `process_for_epoch` (the raw frame is still journaled, and
  replay applies the same fencing). Covered by
  `stale_epoch_messages_cannot_mutate_newer_session` and the
  `Stale`/`NoActiveSession` outcome tests.
- **Case D (clean termination completeness)**: CONFIRMED DEFECT — after
  a clean Termination + TCP EOF the listener journaled a meaningless
  `bmp_tcp_eof` transport gap for an already-removed speaker, pinning
  global completeness to `"gap"` forever (live AND replay, since replay
  applies disconnect records unconditionally). Fixed via
  `NoActiveSession`: a scoped disconnect for a speaker that no longer
  exists records nothing. Test:
  `clean_termination_records_no_transport_gap` (asserts no gap live and
  after replay).

### P1-1 — Binding revocation while connections remain active — DESIGN LIMITATION (documented)

Bindings are re-read from SQLite on every new BMP connection
(`trusted_bmp_bindings()` per `resolve_bmp_exporter` call), so revocation
is visible to NEW connections immediately. An already-admitted `exp:*`
connection keeps its scope, `device_id` attribution and mutation rights
until it disconnects/reconnects. A bounded correction (periodic
re-resolution of live speakers plus connection teardown on revocation)
requires a live-connection registry and speaker-scope migration that is
beyond this review's safe-change envelope. Documented mitigation:
revoke-then-restart the routing service (or disable the BMP listener)
to force re-resolution; detection via the management service's binding
audit trail (`created_by`/`updated_by`/`updated_at_ns` on
`exporter_bindings`). Per-message SQLite checks were rejected as
unnecessary cost for a P1.

### P1-2 — Exporter partition identifiers — DEFECT (encoding) + DOC FIX

- **Namespace encoding**: `exp:{scope}:{canonical}` was ambiguous —
  `(scope="a:b", canonical="c")` and `(scope="a", canonical="b:c")`
  serialized identically. Fixed: both components are now
  percent-encoded (`[A-Za-z0-9._-]` pass through, everything else
  `%XX`), so typical ids keep the readable form while separators can
  never collide. Default-scope `exp:{id}` journal compatibility is
  preserved for all previously valid ids. Test:
  `exporter_instance_encoding_is_unambiguous`.
- **FNV partitioning**: CONFIRMED — `SpeakerKey`/`PeerKey` carry
  `exporter_instance_id(scope)`, a 64-bit FNV-1a hash, NOT the scope
  string; distinct strings with a hash collision would share
  authoritative state. Scope strings are server-generated
  (`conn:{uuid}`, percent-encoded `exp:*`) or operator-provisioned
  canonical ids, so an attacker cannot choose them; residual risk is a
  hash collision or a malicious identity store. Correctness does not
  require a key redesign today (same trust boundary as sequential
  same-claim impersonation); the misleading doc comments claiming the
  authoritative key is `(speaker_addr, exporter_instance)` were
  corrected in `crates/rbmp-core/src/bmp/types.rs`.

## Other review checks

- `migration.rs`: `exporter_instance` for migrated legacy events changed
  `"legacy-default"` → `"default"`. Note: `"default"` maps to speaker
  instance 0 (legacy identity); `"legacy-default"` hashed to a
  nonzero instance — the change ALIGNS migrated records with the
  canonical legacy scope. Pre-change migrated event journals referencing
  `"legacy-default"` would key differently; acceptable because the
  value was only introduced by this PR's migration path and no
  production journal contains it.
- Read-only identity DB: `SqliteStore::open_file_readonly` performs no
  migration/WAL changes; older-schema DBs fail closed to `conn:*`
  isolation. Verified by `identity_store_is_opened_read_only`.
- Journal ordering: disconnect append still serialized under
  `ingestion_lock`; replay fencing of stale epochs verified.
- Memory retention: `conn:*` retirement now also prunes decode
  contexts when the speaker was already removed by Termination.
- Unintended changes: diff reviewed — all changes are within the
  BN-021c scope (routing service, rbmp-core/rib/store keys, lab
  acceptance, tests, docs).

## Tests executed

- `cargo test --test bn021c_exporter_isolation`: 17/17 pass
  (12 pre-existing + 5 new/updated: same-claim isolation, post-
  termination frames, clean-termination gap, journal-failure outcomes,
  scope encoding).
- Full regression list and timings: see "Verification" in the final
  report / `docs/build-times/runs.csv`.

## Live NAT results

Re-executed post-hardening on 2026-11-11 (lab rebuilt via guarded
`BN021C_RESET=1 up.sh`; prior runtime archived, not deleted). Full
sequence in `LIVE-RESULTS.md`:

- Baseline isolation (distinct claims behind one SNAT source): PASS.
- Concurrent same-claim (Scenario F scenario): **FIXED** — impostor
  isolated to `conn:*` with attribution preserved; `exp:exp-frr-a`
  state untouched. Lab-verified.
- Sequential same-claim: **CONFIRMED STILL OPEN** — after the legit
  exporter disconnects, an identical-claim connection is admitted to
  `exp:exp-frr-a` with `dev-frr-a` attribution and injects routes.
  Residual routes persist as last-known under BN-005 gap semantics.
- Pre-review `api/`/`replay/` JSON under the 10-Nov evidence dir
  remains valid for published commit `d19394c`.

## Trust boundary (what proves exporter ownership today)

- Transport endpoint (TCP peer IP) — evidence only, NAT-translated.
- BMP Initiation sysName/sysDescr — self-reported claim, PROVISIONAL.
- Trusted binding match (scope+protocol+ pinned evidence fields) —
  operator-provisioned, but all matchable BMP fields are self-reported
  or share the translated transport IP, so identical claims are
  indistinguishable.
- `conn:{uuid}` scope — proves only "this TCP connection", never
  exporter identity.
- NOT proven anywhere: that two connections belong to the same physical
  exporter, or that a claimant is the bound device. Requires
  authenticated ingress (BN-021f).

## Remaining acceptance gates

- BN-021c P0 production attribution gate: **OPEN** — sequential
  same-claim impersonation and claim-based device attribution remain
  possible; requires authenticated ingress.
- BN-021d (NetFlow/IPFIX identity): must not extend the claim-based
  attribution model until the BMP boundary is settled.
- BN-021e/f: adapter wiring, production acceptance, alias reconciliation.

## Merge recommendation

**Outcome B — PARTIAL CAPABILITY — MERGE REQUIRES EXPLICIT ACCEPTANCE
OF LIMITATIONS.**

Rationale: the concurrent-merge P0 defect is fixed and regression-
covered; live/replay durability (P0-2) and Termination lifecycle
(P0-3) defects are fixed; partitioning, replay, BN-005 and BN-021b
contracts are preserved. The residual same-claim limitation is
contained (concurrent merge impossible; sequential merge requires an
active impersonation window and yields only claim-consistent
attribution) but NOT eliminated — production attribution claims must
remain gated under BN-021f. This is not Outcome A (an essential
isolation invariant is only partially met) and not Outcome C (no
reproducible P0 defect remains unmitigated in the implemented scope).

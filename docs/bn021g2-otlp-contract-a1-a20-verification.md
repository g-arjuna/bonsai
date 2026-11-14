# BN-021g-2 — A1–A20 contract verification matrix

Verification of `docs/bn021g2-otlp-verified-source-contract.md` against the
existing codebase, performed 2026-11-14 on branch
`docs/bn021g2-otlp-contract` @ `eeecf5f` (base `origin/main` @ `e071cc7`).

This is a **documentation-contract review**, not runtime feature
acceptance. The contract is explicitly design-only ("no protocol
expansion in this milestone"); every A-item is therefore a *normative
future* acceptance criterion unless noted. "Consistent" means the case is
internally coherent, testable as written, and anchored to mechanisms
that exist or are precisely specified.

## Architectural anchors verified in code

| Anchor | Location | Status |
|---|---|---|
| Provisional OTLP identity, `identity_status:"provisional"`, `authority:"unverified"`, `device_id:null` hard-coded | `src/streaming/otlp.rs` `OtlpSource::identity_json` | Present, matches §1 |
| Peer-keyed source id `otlpsrc/{collector_id}/http/{ip}:{port}`; X-Forwarded-For/payload never consulted | `src/streaming/otlp.rs` `OtlpSource::http` + doc comment | Present, matches §1 |
| Provisional app id `otlpapp/{source_key}/{service}` (per-connection scope, no canonical Application) | `src/streaming/otlp.rs` `provisional_app_id` | Present, matches §1 |
| Ingestion budgets: 32 resource groups / 32 scope groups / 1024 items per scope / 4096 per request / 256 attrs scanned / 512-char strings | `src/streaming/otlp.rs` `MAX_*` consts | Present, matches §3 cardinality |
| `sane_timestamp_ns` bounded-timestamp sanitization | `src/streaming/otlp.rs` | Present, matches §3 replay row |
| `VerifiedIngressPrincipal` minted only from transport attestation; `transport_attested` is `#[serde(skip)]` and Deserialize forces `false` | `src/telemetry_identity.rs`, `src/collector_auth.rs:232` (`require_attested`) | Present — A13's serde guarantee already holds |
| `exporter_bindings` + `match_ingress_credential` capability binding, fingerprint `sha256:<hex>` | `src/telemetry_identity.rs`, `src/http_server/identity.rs` | Present, matches §2.2/§2.3 |
| Epoch-stamped authorization cache, `AUTHZ_CACHE_TTL` = 5 s, invalidated on binding mutation | `src/collector_auth.rs` (~line 97–110) | Present, matches §3 revocation row (≤5 s window) |
| Fail-closed authz-store behavior pattern (deny on store failure) | `src/collector_auth.rs`, g-1 suites | Present as pattern; A10 applies it to the new listener |

## Normative identity chain check (§2.2)

Certificate fingerprint → trusted credential binding → stable producer
identity → producer-local application identity. Verified consistent:

- Fingerprint is explicitly a *credential-binding key*, never an identity
  key — rotation is a re-binding event (A16).
- App ids keyed on stable producer identity only; two producers sharing
  `service.name` stay distinct (A7, A18).
- Payload-asserted roles/identities are claims only (A6, A19).
- Credentials are evidence, not permanent producer identifiers —
  rotation preserves identity; revoked/superseded credentials rejected
  per policy (A16, A17).

## A1–A20 matrix

| # | Verdict | Notes |
|---|---|---|
| A1 | CONSISTENT (normative) | Requires the verified listener + projection, not yet implemented. Id format `otlpapp+/{producer_identity}/{discriminator}` is new and distinct from provisional `otlpapp/` — no collision with existing keys. |
| A2 | CONSISTENT (normative) | Follows from producer-identity keying; port/NAT irrelevant since peer address is never an identity input on the verified path. |
| A3 | CONSISTENT (normative) | Capability check reuses `match_ingress_credential`; rejection before payload parse matches §3 failure modes. |
| A4 | CONSISTENT (normative) | ≤5 s epoch window is implemented in g-1 (`AUTHZ_CACHE_TTL`); contract requires documenting the exact max revocation window at implementation — open parameter, not inconsistency. |
| A5 | CONSISTENT (normative) | "Ambiguous binding → reject" is listed in §3 failure modes; requires implementation to define ambiguity detection (duplicate principal bindings). |
| A6 | CONSISTENT (normative) | Directly restates §2.1 rule 3; provisional writers already demonstrate the claim-only pattern. |
| A7 | CONSISTENT (normative) | Follows from §2.2 scoping; testable once projection exists. |
| A8 | CONSISTENT (normative) | Restates §2.1 rules 1–2; consistent with existing provisional upstream handling. |
| A9 | CONSISTENT (normative) | All cited budgets verified present in `otlp.rs`; contract correctly requires they apply unchanged on the authenticated listener. |
| A10 | CONSISTENT (normative) | Matches existing fail-closed authz pattern; "never degrade to provisional-authoritative" is the correct polarity. |
| A11 | CONSISTENT (normative) | §2.3 explicitly forbids silently upgrading the plaintext listener; verified current behavior (`identity_status` hard-coded provisional). |
| A12 | CONSISTENT (normative) | Bindings/graph events are durable in GraphStore; restart-persistence is testable as written. |
| A13 | VERIFIED ALREADY | `transport_attested` is `#[serde(skip)]`; custom `Deserialize` sets it `false`; `require_attested` rejects unattested principals (`collector_auth.rs:232`). The regression test A13 asks for pins existing behavior. |
| A14 | CONSISTENT — OPEN PARAMETER | Cap value "proposed 64" is explicitly deferred ("finalize at implementation"); A14 says "> cap" without fixing the number. Not an inconsistency, but the cap must be fixed before A14 can be evaluated literally. |
| A15 | CONSISTENT (normative) | Requires real TLS loopback; consistent with g-1 harness style (rcgen synthetic CA). |
| A16 | CONSISTENT (normative) | Core of the four-identity model; fingerprint re-binding preserves producer + app ids. |
| A17 | CONSISTENT (normative) | Old fingerprint unbound/revoked → handshake/authz reject; "no oracle beyond connect-level" matches §3. |
| A18 | CONSISTENT (normative) | Scope field already applies to bindings; app-id namespacing per (scope, producer) is coherent. |
| A19 | CONSISTENT (normative) | Payload role claims can never satisfy capability checks — restates §2.1/§2.2; consistent with `match_ingress_credential` being fingerprint-bound, not payload-bound. |
| A20 | CONSISTENT — OPEN PARAMETER | Dedup key specified; "bounded retention horizon" is not numerically fixed — must be bounded at implementation (same class of open parameter as A14's cap and A4's window). |

## Findings

1. **No blocking inconsistencies.** The contract is internally coherent
   and every architectural claim checked against the code holds.
2. **Three open numeric parameters** deferred to implementation, all
   explicitly flagged as such in the contract: app-discriminator cap
   (proposed 64, A14), max revocation window documentation (A4, bounded
   by the existing ≤5 s epoch cache), dedup retention horizon (A20).
   These must be fixed in the implementation PR before the corresponding
   A-items can be literally evaluated — recorded here so they are not
   silently assumed.
3. **A13 is the only item whose security property already exists in
   code** (`transport_attested` serde guarantee); it is correctly framed
   as a regression test, not new functionality.
4. **Non-goals are respected by the matrix:** no gRPC, no canonical
   Device/Application creation, no network-impact inference, no
   distributed revocation — none of the A-items require them.
5. **Classification:** this document is a normative future contract.
   Nothing in A1–A20 asserts implemented runtime behavior; runtime
   acceptance requires the g-2 implementation plus executed L2/L3
   evidence, not this review.

## Disposition

PR #18 contract: **technically consistent — READY for merge as a design
contract**, subject to the three open parameters being fixed at g-2
implementation time. Not runtime feature acceptance.

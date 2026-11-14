# BN-021g-2 — Verified OTLP original-source projection contract

Status: implementation contract (design only — no protocol expansion in
this milestone). Scope: the **first** verified projection only. Derived
from `src/streaming/otlp.rs`, `src/telemetry_identity.rs`,
`src/collector_auth.rs` and the BN-021g-1 trust foundation
(`tests/bn021g_collector_trust_foundation.rs`).

## 1. Problem statement

Today every OTLP observation is provisional by construction:

- `OtlpSource::http` keys identity on the observed TCP peer
  (`otlpsrc/{collector_id}/http/{ip}:{port}`); `X-Forwarded-For` and all
  payload fields are explicitly untrusted.
- `identity_json()` hard-codes `identity_status: "provisional"`,
  `authority: "unverified"`, `device_id: null`.
- Application identity is `otlpapp/{source_key}/{service}` — scoped to
  the connection, never merged across senders, never canonical.
- Ingestion budgets bound body/groups/items/strings; non-finite metrics
  are dropped; drops are metric'd.

BN-021g-1 already established the pattern to reuse: transport-authenticated
principal (`VerifiedIngressPrincipal`, mTLS leaf fingerprint
`sha256:<hex>`), capability-scoped authorization, epoch-invalidated
decision cache, and classified canonical-vs-provisional authority.

## 2. Trust boundary decisions

### 2.1 Authenticated forwarding collector vs authenticated original source

Two distinct principals must never be conflated:

- **Collector principal** — the entity that completed the transport
  handshake (mTLS client cert or equivalent). It is the *only* thing
  the transport attests.
- **Original source** — the entity that produced the telemetry (an
  application host, an OTel SDK endpoint, a device). The transport
  cannot attest this unless the original source *is* the TLS peer.

Rules:

1. A verified projection may attribute evidence to the **authenticated
   producer's own application evidence** — i.e. the TLS peer itself.
2. A forwarding collector may carry *provenance claims* about upstream
   sources, but those claims stay `provisional`/`claimed_*` evidence
   unless a separate, explicitly authorized attestation mechanism is
   designed (out of scope for g-2).
3. `peer.address`, `net.peer.*`, `service.name`, `host.name`,
   `resource.attributes`, span `peer.*` attributes are **self-reported
   claims**. They may label evidence; they may never establish ownership,
   canonical `Device`/`Application` identity, or `APP_IMPACTED_BY_NETWORK`
   style relationships.
4. No inference of arbitrary application hosting, remote-peer ownership,
   or network-impact edges from OTLP attributes — same prohibition as
   the current provisional writers.

### 2.2 First verified projection (narrow)

The first authoritative projection is deliberately minimal:

- **Scope:** direct authenticated OTLP producer → spans/metrics whose
  `resource`/`scope` attributes describe *that producer's own*
  application (e.g. `service.name` of the producing service).
- **Identity — four separate concepts, never conflated:**
  1. *Credential fingerprint* (`sha256:<hex>` of the presented client
     cert) — the transport-authenticated key. Rotation produces a NEW
     fingerprint; it is a re-binding event, not a new producer.
  2. *Stable bound producer identity* — the durable principal record the
     fingerprint is bound to in `exporter_bindings` (e.g. binding id +
     authorized capability set). Credential rotation re-binds the SAME
     producer identity to the new fingerprint; it must never mint a
     duplicate producer.
  3. *Producer-local application identity* —
     `otlpapp+/{producer_identity}/{app_discriminator}` where
     `app_discriminator` is a validated resource attribute (bounded
     length, charset-restricted). Scoped to the bound producer; two
     producers using the same `service.name` yield two distinct ids.
  4. *Authorized application ownership* — an operator/trusted-inventory
     binding that MAY later map a producer to a managed
     device/application record. The first verified slice does NOT create
     or assert this; it remains a separate, explicit authorization
     decision.

  The normative identity chain is: **certificate fingerprint → trusted
  credential binding → stable producer identity → producer-local
  application identity**. Application ids are keyed on the stable
  producer identity only; the fingerprint is a credential-binding key,
  never an identity key. Credential rotation re-binds the same producer
  and therefore preserves both producer and application identity; a
  payload-asserted producer role can never substitute for the
  transport-attested binding.
- **Ownership:** the verified projection asserts "producer P produced
  application-evidence E at time T" — nothing more. It does not assert
  that P *hosts* the application in an inventory sense unless a trusted
  binding maps the credential to a managed device/application record.
- **Verified evidence semantics:** direct authenticated producer evidence
  is NOT the same as forwarded collector attestation. Machine-readable
  authority states (exact enum names fixed at implementation):
  - `verified_direct` — the TLS peer itself produced the evidence.
  - `claimed_*` — self-reported attributes (hostname, peer address,
    service labels) preserved as claims only.
  - `provisional` / `unverified` — everything unattested; an
    unattested request is never upgraded, and a forwarding collector's
    upstream claims never become `verified_direct`.

### 2.3 Transport options (implementation choice deferred to g-2 PR)

- Preferred: terminate OTLP/gRPC or OTLP/HTTP+TLS on a listener that
  reuses the BN-021f/021g-1 mTLS handshake → `VerifiedIngressPrincipal`
  via `TlsConnectInfo`. HTTP+TLS with client-cert verification is
  sufficient for JSON OTLP; gRPC OTLP is optional expansion.
- The existing plaintext `run_otlp_receiver` stays provisional-only;
  it must not be silently upgraded. A separate authenticated listener
  (or a TLS config gate) is required so operators opt in explicitly.
- Capability model: reuse `exporter_bindings` /
  `match_ingress_credential` with a new narrow capability
  (e.g. `otlp-producer`) rather than inventing a parallel ACL.

## 3. Required semantics

| Concern | Contract |
|---|---|
| Spoofed `service.name`/`host.name`/`peer.*` | Treated as claims only; may appear as `claimed_*` evidence. Cannot select canonical identity, cannot create `Device`/`Application` nodes, cannot satisfy ownership checks. |
| Forwarded `X-Forwarded-For`/PROXY headers | Never consulted for identity (already true; must remain true on the TLS listener — `TlsConnectInfo` peer only). |
| Credential revocation | Binding revocation/disable must deny *new* connections immediately; in-flight streams revalidate per message or per bounded epoch (reuse the ≤5 s epoch-stamped cache semantics from g-1 and document the max revocation window). |
| Replay | Sequence/epoch fencing where the protocol provides it; duplicate deliveries are idempotent at the projection layer (dedup key includes principal + app id + span/metric identity). Out-of-order timestamps bounded by `sane_timestamp_ns`; no wall-clock fabrication. |
| Cardinality | Verified path inherits existing budgets (body 2 MiB, ≤32 resource groups, ≤1024 items/scope, ≤4096/request, ≤256 attrs scanned, ≤512-char strings). App-discriminator cardinality bounded per principal (proposed cap, e.g. 64 distinct app ids/principal — finalize at implementation). |
| Retention | Verified and provisional evidence share the parquet archiver + graph-event durability path; verified projections are retained under the same graph retention policy. No separate unbounded store. |
| Failure modes | TLS handshake failure, untrusted/expired cert, unbound fingerprint, revoked binding, ambiguous binding → reject before parsing payload (no oracle via error differentiation beyond connect-level). Authz-store failure → fail closed (deny), never provisional-upgrade. |
| Multi-tenancy | Scope (`scope` field / site / tenant) still applies; a verified principal in scope A cannot write verified projections into scope B. |
| Resource budgets | Verified listener bounds: max concurrent TLS sessions (proposed 64/listener — finalize at implementation); request parse budget inherits the existing body/group/item/string caps; per-producer app-discriminator cardinality cap (proposed 64 distinct app ids/principal); replay dedup window bounded per producer (dedup key = producer identity + app id + span/metric identity, retained for a bounded horizon, not unbounded); graph write amplification capped (verified path shares the existing graph-event budgets — no second unbounded writer); retention identical to provisional evidence; revocation revalidation honors the ≤5 s epoch-stamped decision-cache window documented in g-1. |
| Acceptance harness | Runs under the local containerized CI (`scripts/ci/run-local.sh`, `docker/ci/Dockerfile`) and the canonical `scripts/ci/gates.sh` tiers — synthetic CA + client certs (rcgen, already a dev-dep), isolated loopback producers only, real temporary GraphStore instances. No production telemetry sources or credentials. New TLS-loopback cases join the L2/L3 manifest explicitly; they are never counted as L4 lab acceptance. |

## 4. Acceptance matrix (BN-021g-2 exit criteria)

Real TLS loopback tests (synthetic CA + client certs, real GraphStore —
no mocks for persistence), mirroring the g-1 harness style:

| # | Case | Expected |
|---|------|----------|
| A1 | Valid mTLS producer posts `/v1/traces` with `service.name=payments` | Span event written with verified principal + stable `otlpapp+/{producer_identity}/payments`-class id keyed on the **stable bound producer identity** (never the credential fingerprint); `identity_status` verified; no canonical `Device` fabricated |
| A2 | Same producer reconnects from a different source port / behind NAT | Same application identity (producer-identity-keyed, not peer-keyed, not fingerprint-keyed) |
| A3 | Producer presents valid cert not bound to an `otlp-producer` capability | Rejected at handshake/authz; zero graph writes |
| A4 | Binding revoked mid-stream | In-flight stream denied within documented epoch window; subsequent messages rejected |
| A5 | Two credentials bound to ambiguous/duplicate principals | Fail closed (no arbitrary pick) |
| A6 | Payload claims `peer.address`, `host.name`, `net.peer.ip` of a victim device | Claims preserved as evidence only; no ownership/impact edges; victim device state untouched |
| A7 | Same `service.name` sent by two different authenticated producers | Two distinct verified app ids; no merge |
| A8 | Forwarding collector sends spans describing an upstream app it does not own | Upstream identity stays provisional/claimed; only collector's own attested evidence may be verified (per §2.1) |
| A9 | Malformed/oversized OTLP body on authenticated listener | Existing budgets apply; drops metric'd; connection not privileged by auth success |
| A10 | Authz store unavailable during request | Deny (503-class); never degrade to provisional-authoritative |
| A11 | Plaintext (unauthenticated) listener receives identical payload | Still provisional/unverified — auth on one listener never upgrades the other |
| A12 | Restart persistence | Verified app ids/events survive restart; revocation state durable |
| A13 | JWT/API-key authn on northbound API cannot mint `VerifiedIngressPrincipal` | Deserialized/replayed principals carry `transport_attested=false` and are rejected (existing serde guarantee — regression test) |
| A14 | Cardinality abuse: producer registers > cap distinct app discriminators | Excess rejected/dropped with metric; no unbounded id space |
| A15 | Real TLS acceptance | End-to-end over actual TLS (not synthetic in-process channel): handshake → verified write → API read-back shows verified status |
| A16 | Credential rotation: bound producer presents a NEW client cert/fingerprint for the same stable producer identity | Same stable producer identity and same `otlpapp+/…` application ids; rotation is a re-binding event that never mints a duplicate producer or app id; prior fingerprint's in-flight window closes per the documented epoch semantics |
| A17 | Old credential replayed after rotation/revocation | Rejected at handshake/authz (fingerprint no longer bound or binding disabled); zero graph writes; error path does not reveal binding internals |
| A18 | Cross-scope collision: two producers in different scopes use identical `service.name`/discriminator | App ids remain distinct per (scope, producer identity); no cross-scope merge, lookup or metric aggregation |
| A19 | Forwarding collector holds a valid collector credential and payload attributes assert `role=original_producer` / upstream producer identity | Payload-asserted role is a claim only; collector's own evidence may verify under its collector capability, upstream identities stay `claimed_*`/`provisional`; a collector credential alone can never satisfy the `otlp-producer` capability check |
| A20 | Duplicate/replayed span and metric deliveries (same principal + app id + span/metric identity, incl. out-of-order within `sane_timestamp_ns`) | Projection is idempotent: no duplicate graph events or parquet rows; dedup key = stable producer identity + app id + span/metric identity over a bounded retention horizon |

## 5. Explicit non-goals for g-2

- OTLP/gRPC receiver (HTTP+TLS suffices for the first projection).
- Canonical `Application`/`Device` node creation from OTLP claims.
- `APP_IMPACTED_BY_NETWORK` or any network-impact inference.
- Distributed/multi-node revocation fan-out (BN-021g-6).
- Changes to collector→core gRPC trust (BN-021g-1, complete).

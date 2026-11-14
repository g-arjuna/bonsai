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
- **Identity:** stable application identity derived from the
  authenticated principal + an explicit, bounded producer-chosen app
  discriminator, e.g.
  `otlpapp+/{credential_fingerprint}/{app_discriminator}` where
  `app_discriminator` is a validated resource attribute (bounded length,
  charset-restricted). The credential fingerprint — not the IP:port —
  is the stable owner key, so reconnects/NAT rebinding do not fork
  identity, and credential rotation is an explicit re-binding event.
- **Ownership:** the verified projection asserts "principal P produced
  application-evidence E at time T" — nothing more. It does not assert
  that P *hosts* the application in an inventory sense unless a trusted
  binding maps the credential to a managed device/application record.
- **Authoritative vs provisional:** events written under a verified
  principal carry `identity_status: "verified"` /
  `authority: "collector-attested"` (exact enum names TBD at
  implementation); everything else remains `provisional` /
  `unverified`. Unknown stays unknown — an unattested request is never
  upgraded.

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

## 4. Acceptance matrix (BN-021g-2 exit criteria)

Real TLS loopback tests (synthetic CA + client certs, real GraphStore —
no mocks for persistence), mirroring the g-1 harness style:

| # | Case | Expected |
|---|------|----------|
| A1 | Valid mTLS producer posts `/v1/traces` with `service.name=payments` | Span event written with verified principal + stable `otlpapp+/sha256:…/payments`-class id; `identity_status` verified; no canonical `Device` fabricated |
| A2 | Same producer reconnects from a different source port / behind NAT | Same application identity (fingerprint-keyed, not peer-keyed) |
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

## 5. Explicit non-goals for g-2

- OTLP/gRPC receiver (HTTP+TLS suffices for the first projection).
- Canonical `Application`/`Device` node creation from OTLP claims.
- `APP_IMPACTED_BY_NETWORK` or any network-impact inference.
- Distributed/multi-node revocation fan-out (BN-021g-6).
- Changes to collector→core gRPC trust (BN-021g-1, complete).

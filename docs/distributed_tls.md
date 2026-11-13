# Distributed Collector/Core TLS

T2-3 secures the collector-to-core `TelemetryIngest` channel with mutual TLS.
It is disabled by default for local single-process work. Enable it only for
`runtime.mode = "core"` or `runtime.mode = "collector"` distributed runs.

## CA Layout

Use one lab CA for the distributed Bonsai control plane:

- `ca.pem` signs both core and collector certificates.
- The core presents a server certificate whose SAN contains the name collectors
  use in `runtime.tls.server_name`.
- Each collector presents a client certificate signed by the same CA.
- Private keys stay local to the process that presents them and must not be
  committed.

## Generate Lab Certificates

From the repo root:

```powershell
New-Item -ItemType Directory -Force -Path config\tls

openssl genrsa -out config\tls\ca-key.pem 4096
openssl req -x509 -new -nodes -key config\tls\ca-key.pem -sha256 -days 3650 `
  -subj "/CN=bonsai-lab-ca" -out config\tls\ca.pem

openssl genrsa -out config\tls\core-key.pem 2048
openssl req -new -key config\tls\core-key.pem -subj "/CN=bonsai-core.local" `
  -out config\tls\core.csr
Set-Content config\tls\core.ext "subjectAltName=DNS:bonsai-core.local,IP:127.0.0.1"
openssl x509 -req -in config\tls\core.csr -CA config\tls\ca.pem `
  -CAkey config\tls\ca-key.pem -CAcreateserial -out config\tls\core.pem `
  -days 825 -sha256 -extfile config\tls\core.ext

openssl genrsa -out config\tls\collector-key.pem 2048
openssl req -new -key config\tls\collector-key.pem -subj "/CN=bonsai-collector-1" `
  -out config\tls\collector.csr
Set-Content config\tls\collector.ext "extendedKeyUsage=clientAuth"
openssl x509 -req -in config\tls\collector.csr -CA config\tls\ca.pem `
  -CAkey config\tls\ca-key.pem -CAcreateserial -out config\tls\collector.pem `
  -days 825 -sha256 -extfile config\tls\collector.ext
```

If the collector runs in WSL and connects to a Windows core through a different
host/IP, add that DNS name or IP to `core.ext` before signing the core cert.

## Core Config

```toml
[runtime]
mode = "core"

[runtime.tls]
enabled = true
ca_cert = "config/tls/ca.pem"
cert = "config/tls/core.pem"
key = "config/tls/core-key.pem"
```

The core uses `ca_cert` as the client trust root. Collectors without a
certificate signed by that CA are rejected during the TLS handshake.

## Collector Config

```toml
[runtime]
mode = "collector"
collector_id = "lab-wsl"
core_ingest_endpoint = "https://127.0.0.1:50051"

[runtime.tls]
enabled = true
ca_cert = "config/tls/ca.pem"
cert = "config/tls/collector.pem"
key = "config/tls/collector-key.pem"
server_name = "bonsai-core.local"
```

Collectors use `ca_cert` to verify the core certificate and present `cert`/`key`
as their client identity. `server_name` must match a SAN on the core
certificate; it may differ from the host portion of `core_ingest_endpoint`.

## Validation

1. Start the core with the core config.
2. Start the collector with the collector config; it should connect and replay
   any queued records.
3. Remove or rename the collector cert/key and restart the collector; the TLS
   handshake should fail before any telemetry is accepted.
4. Sign a collector certificate with another CA and restart; the core should
   reject it during handshake.

## BN-021g: credential→collector authorization (added 12 November 2026)

When `runtime.tls.enabled = true`, the core additionally requires every
mTLS client certificate to be pinned to an authorized binding — a
certificate chaining to the lab CA is necessary but NOT sufficient:

- Compute the credential fingerprint: `sha256:<hex>` of the DER-encoded
  client certificate leaf (same convention as BN-021f BMP ingress).
- Provision a canonical device for the collector entity, then create an
  exporter binding via `POST /api/identity/bindings` (admin):
  - `protocol`: `"collector"` for the ingest plane
    (`TelemetryIngest`, `DetectionIngest`, `Heartbeat`,
    `RegisterCollector`), `"core-admin"` for device/site/credential
    mutation, `query` and remediation writes, `"sidecar"` for sidecar
    RPCs. Read RPCs accept any active pinned binding.
  - `exporter_id`: the collector's `collector_id` (ingest plane) or any
    label (capability planes).
  - `match_ingress_credential`: the fingerprint above.
- The claimed `collector_id` in each ingest message must equal the
  bound `exporter_id` — certificate A cannot claim collector B.
- Authorization decisions are cached ≤5s **stamped with a
  process-local authorization epoch**: every `exporter_bindings`
  upsert/revoke bumps the epoch, so within one core process revoking
  the binding (`POST /api/identity/bindings/{id}/revoke`) is visible
  on the next authorization check — not after the TTL. The TTL only
  bounds staleness for changes made outside the process (e.g. direct
  DB edits on another node in a multi-node deployment). Rotation =
  update `match_ingress_credential` on the same binding (sequential
  re-pin; old fingerprint loses authority on the next check).
- Plaintext mode (`runtime.tls.enabled = false`) remains the explicit
  legacy development path with no authorization.

## BN-021g-1 hardening: authorization beyond transport (added 12 November 2026)

Transport authentication is not unrestricted authority. Every RPC is now
gated by an explicit capability, and ingest is authorized per
observation, not per connection.

### Authentication policy (`runtime.auth_mode`)

- `required` (default): a request without a transport-attested TLS
  client principal is rejected on every privileged RPC. Plaintext
  listeners cannot serve authorized RPCs.
- `development-compat`: explicit opt-in for isolated local development;
  requests without a principal keep legacy behavior. The server logs a
  loud warning at startup. Never use outside a trusted loopback lab.
- Any other value fails config parsing (fail closed).

### RPC capability matrix

| RPC class | Required capability |
|---|---|
| `TelemetryIngest`, `Heartbeat`, `RegisterCollector` stream | `collector` binding whose `exporter_id` == claimed `collector_id` |
| `DetectionIngest` | `detection-ingest` binding + per-message collector grant + target/event verification |
| `Query`, device/site/credential mutations, detection/remediation writes | `core-admin` binding |
| `RegisterSidecar`, `SidecarHeartbeat` | `sidecar` binding |
| `GetTopology`, `ListSites`, `StreamEvents`, global `GetDevices`/`GetInterfaces`/`GetBgpNeighbors`/`ListManagedDevices` | `operator-read` binding |
| Device-scoped `GetDevices`/`GetInterfaces`/`GetBgpNeighbors`/`ListManagedDevices` | `collector` binding; result limited to devices assigned to that collector |
| `GetLeader` | any active credential-pinned binding |

A credential bound only for `collector`, `detection-ingest`,
`core-admin`, `sidecar`, `operator-read`, or an exporter protocol
(BMP/OTLP/NetFlow/sFlow/syslog/SNMP) gains exactly its listed
capability — no implicit read or admin access. Exporter-protocol
bindings do NOT appear in this matrix: they authorize original-source
producers, not core RPCs.

### Observation authorization (G1 + H1 classified authority)

`TelemetryIngest` decodes each record, **classifies** it via
`TelemetryUpdate::classify()` → `TelemetryEvent::authority()`, and only
then authorizes — the path prefix alone never grants authority:

- **`CanonicalDevice`** observations (YANG paths *and* canonical
  writers reachable through `streaming/` paths — BMP peer-state /
  route-monitoring / initiation / statistics, BGP-LS node/link/
  policy, interface summaries, config-state leaves) require the
  `target` to be a registry device that is `enabled` and assigned
  (`collector_id`) to the authenticated collector. Cross-collector,
  unassigned, and unknown targets are dropped per message.
- **`Provisional`** observations must carry a recognized `*src/`
  source key (`flowsrc/`, `sflowsrc/`, `otlpsrc/`, `syslogsrc/`,
  `snmpsrc/`, `streaming/`, `signals/` provisional classes) — never a
  canonical device address. The collector namespace segment is
  rewritten to the authenticated `collector_id`, so a collector cannot
  file evidence under another collector's namespace; the original
  producer claim is preserved inside the payload as unverified
  evidence.
- **`Ignored`** observations (unclassified paths) pass through with no
  graph effect.
- Producer-supplied `vendor`/`hostname`/`role`/`site` are replaced by
  authoritative registry inventory before publication on the canonical
  path.

Serialized BMP/BGP-LS payloads arriving through generic collector
ingestion therefore cannot acquire the authority of the original
authenticated BMP/BGP-LS receivers — they are gated by the same
assigned-device rule, and direct receiver behavior is unchanged.

**Trust contract:** the registry assignment authorizes the collector's
acquisition process to report on that configured device. It is NOT
cryptographic proof that the bytes came from that device — a
compromised collector can still forge observations for its assigned
devices. Original-source authentication remains BN-021g-2+ work.

### Detection ingest authority (G2 + H3/H4)

`DetectionIngest` requires the separate `detection-ingest` capability
(not merely `collector`), revalidated **per message** on the open
stream — revoking only that binding stops detection authoring on the
same stream while ordinary collector telemetry continues (bounded by
the authz epoch/decision cache). Per-message verification:

- `features_json` and `source_types_json` must parse; malformed
  payloads are permanently rejected.
- Remote ingest **requires positive provenance**: `source_event_ids`
  (plus `state_change_event_id`) must be non-empty, every referenced
  event must exist, belong to the authorized canonical device, carry
  server-owned non-provisional `source_type` provenance, and not be
  conflicting/stale. Evidence-free remote detections are denied —
  `StateChangeEvent.device_address` alone is never proof of canonical
  identity, and authority is not inferred from JSON field absence.
- The device must be assigned to the collector.
- Stored `source_types` is marked with `remote_detection_ingest` so
  remote-submitted detections are distinguishable from locally
  authored (core-admin) detections downstream.
- Remote submissions cannot self-authorize remediation — the proto's
  `auto_remediate`/`remediation_action` fields are ignored on this
  path, and graph-write failures abort the stream as retryable errors
  rather than counting as rejections.
- Local detection-engine writes are unaffected.

### Active streams and session fencing (G4)

- `RegisterCollector`'s assignment stream revalidates the binding
  before every delivery and on a bounded interval while idle; a revoked
  credential stops receiving credential-bearing assignments (device
  usernames/passwords) within ~5s.
- `StreamEvents` revalidates `operator-read` periodically; idle streams
  are fenced the same way.
- Registration is generation-fenced: a reconnecting collector gets a new
  session generation; an old stream closing cannot unregister or steal
  the newer session, and stale generations cannot push updates.
- **Credential delivery caveat:** revocation stops FUTURE deliveries
  but cannot recall usernames/passwords already sent. Rotating a
  collector's authorization should be paired with rotating the device
  credentials it received.

### Revocation, rotation, replay

- Authorization decisions are cached ≤5s (4096-entry bounded cache,
  fail-safe clear on overflow) **stamped with a process-local
  authorization epoch**: every `exporter_bindings` upsert/revoke bumps
  the epoch and invalidates all cached decisions immediately. Within
  one core process, revocation is therefore visible on the next
  authorization check — per-message ingest checks and per-delivery
  stream checks — not after a TTL. The 5s TTL only bounds staleness for
  changes made outside the process (direct DB edits on another node).
- Idle streams revalidate on `STREAM_REVALIDATE_INTERVAL` (= 5s), so a
  revoked credential on an idle assignment/event stream is fenced
  within ~5s plus one poll phase; an actively delivering stream is
  fenced on the next delivery. Backpressured streams cannot extend
  this: revalidation runs on the same select loop as delivery.
- Assignment sessions are generation-fenced AND cleanup-owned: a drop
  guard captured by the response stream unregisters the session on
  normal end, client disconnect, or cancellation — including a stream
  dropped before its first poll. `unregister_collector` checks and
  removes the current generation and performs all runtime/registry
  cleanup under one `active_collectors` lock, so stale cleanup cannot
  mark a newer session disconnected, clear its assignments, or deliver
  stale assignment state.
- Rotation = re-pin `match_ingress_credential` on the same binding.
- Principals exist only in TLS request extensions of a live
  connection; serialized/replayed records cannot mint authority.
- Ambiguous bindings (multiple active rows matching
  protocol+collector+fingerprint across scopes) fail closed — for
  ingest AND for collector-scoped reads. `collector_ids_for_credential`
  no longer collapses identities with `SELECT DISTINCT`; the same
  trusted-binding ambiguity rules apply to both planes.
- **Global collector identity uniqueness (R4):** for active
  `protocol="collector"` bindings the `(exporter_id)` collector
  identity and the `match_ingress_credential` credential are each
  globally unique — enforced at provisioning time in
  `upsert_exporter_binding` (schema v5 partial unique indexes
  `idx_exporter_bindings_collector_identity` /
  `idx_exporter_bindings_collector_credential`) and re-checked at
  authorization time, where pre-existing ambiguous rows fail closed
  for every claimant. One credential cannot be provisioned for two
  collector identities; one collector identity cannot be claimed by
  two credentials — including across different scopes. Rotation
  updates the same logical binding (same `binding_id`). Note:
  canonical graph entities remain globally address-keyed — collector
  scoping limits which devices are returned/written, it is not
  multi-tenant isolation.

### Queue acknowledgement contract (H2)

The collector disk queue (`queue.dat`/`queue.ack`) acknowledges by
byte offset only after the core's `TelemetryIngestResponse` arrives —
and the core consumes the entire client stream before responding, so
a response means every record in the batch was processed:

- `accepted` + `rejected` == submitted count; the whole batch is
  retired. Rejected records (authorization denial, malformed payload,
  protocol major skew) are permanently refused — never replayed — so
  a poison record cannot block later legitimate telemetry.
- The collector validates the response **before** acking: the
  response `protocol_version` must not be major-skewed, and
  `accepted + rejected` must exactly equal the submitted count using
  checked arithmetic. An inconsistent, impossible or
  protocol-incompatible response is a protocol violation — the
  collector writes no acknowledgement and retains the whole batch
  for replay (loud failure, never silent record loss).
- Protocol version is verified on **every** record core-side, not
  just the first of a stream: equal = compatible, non-equal
  same-major = minor skew (logged, processed), major skew =
  permanent per-record rejection.
- RPC error / connection loss → no ack → whole batch retained for
  at-least-once replay. Accepted means published to the in-memory bus,
  not a durable graph commit; exactly-once is not claimed.

### Provisioning

Capability bindings are provisioned via `POST /api/identity/bindings`
(admin) with `protocol` in {`collector`, `detection-ingest`,
`core-admin`, `sidecar`, `operator-read`} and
`match_ingress_credential` = `sha256:<hex>` of the DER leaf. Collector
bindings additionally need `exporter_id` = the collector's
`collector_id`. Migrating a plaintext dev deployment: enable
`runtime.tls`, set `runtime.auth_mode = "required"` (the default),
provision bindings, and restart — there is no silent downgrade.

Checked-in config migration status:

- `docker/configs/{standalone,lab-dc,lab-sp,cloud-dc}.toml` (and the
  gitignored local `bonsai.toml`) run plaintext loopback/containerlab
  development and now carry an explicit
  `runtime.auth_mode = "development-compat"` — same legacy
  pass-through as before, but visibly opt-in rather than implied.
- `docker/configs/{core,collector-1,collector-2}.toml` are the
  distributed mTLS example: `runtime.tls` is enabled and
  `auth_mode` defaults to `required`, so the core demands a verified
  client certificate on every RPC. **Before this deployment passes
  traffic, an operator must provision `collector` bindings** (one per
  collector cert fingerprint + `collector_id`) via
  `POST /api/identity/bindings` — until then all collector RPCs fail
  closed by design. The docker lab does not yet automate this
  provisioning step; treat it as a required migration action, not a
  regression.

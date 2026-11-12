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
- Authorization decisions are cached ≤5s; revoking the binding
  (`POST /api/identity/bindings/{id}/revoke`) stops privileged ingest
  within that window. Rotation = update `match_ingress_credential`
  on the same binding (sequential re-pin; old fingerprint loses
  authority immediately after cache expiry).
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

### Observation authorization (G1)

`TelemetryIngest` authorizes each update before it reaches the event
bus:

- Provisional paths (`streaming/`, `signals/`) pass through as
  provisional evidence — they can never mint canonical device state.
- Canonical paths require the `target` to be a registry device that is
  `enabled` and assigned (`collector_id`) to the authenticated
  collector. Cross-collector, unassigned, and unknown targets are
  dropped per message.
- Producer-supplied `vendor`/`hostname`/`role`/`site` are replaced by
  authoritative registry inventory before publication.
- Path relabeling cannot bypass this: a flow/OTLP payload renamed to a
  canonical YANG path still hits the assigned-device check, and the
  provisional graph writers ignore canonical claims by design.

**Trust contract:** the registry assignment authorizes the collector's
acquisition process to report on that configured device. It is NOT
cryptographic proof that the bytes came from that device — a
compromised collector can still forge observations for its assigned
devices. Original-source authentication remains BN-021g-2+ work.

### Detection ingest authority (G2)

`DetectionIngest` requires the separate `detection-ingest` capability
(not merely `collector`), plus per-message verification: the device
must be assigned to the collector, every `source_event_ids` entry must
exist, belong to that same canonical device, and not be provisional
(`identity_status:"provisional"` / `authority:"unverified"`). Remote
submissions cannot self-authorize remediation — the proto's
`auto_remediate`/`remediation_action` fields are ignored on this path.
Local detection-engine writes are unaffected.

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
  fail-safe clear on overflow). Revocation and reassignment take effect
  within that window; measured, not claimed constant-time.
- Rotation = re-pin `match_ingress_credential` on the same binding.
- Principals exist only in TLS request extensions of a live
  connection; serialized/replayed records cannot mint authority.
- Ambiguous bindings (multiple active rows matching
  protocol+collector+fingerprint across scopes) fail closed.

### Provisioning

Capability bindings are provisioned via `POST /api/identity/bindings`
(admin) with `protocol` in {`collector`, `detection-ingest`,
`core-admin`, `sidecar`, `operator-read`} and
`match_ingress_credential` = `sha256:<hex>` of the DER leaf. Collector
bindings additionally need `exporter_id` = the collector's
`collector_id`. Migrating a plaintext dev deployment: enable
`runtime.tls`, set `runtime.auth_mode = "required"` (the default),
provision bindings, and restart — there is no silent downgrade.

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

# BN-021f — Live NAT acceptance results (2026-11-11)

Lab: `lab/acceptance/bn021c-nat/up.sh` (BN021C_RESET=1 rebuild), image
`bn021c-routing:local` built from the BN-021f branch binary. All BMP
traffic is mutual TLS: FRR → `bmp_tls_proxy.py` (per-exporter client
cert, verifies Bonsai server cert) → iptables SNAT → Bonsai mTLS
listener. Every exporter shares translated source `10.240.0.200`.

Identity DB (schema v4): bindings pin `match_ingress_credential` =
SHA-256 of each client cert DER. frr-a→`exp-frr-a`/`dev-frr-a`,
frr-b→`exp-frr-b`/`dev-frr-b`; frr-c and frr-imp have valid certs but
NO binding.

Raw captures: `api/speakers-post-restore.json`,
`api/routes-post-restore.json`, `service-logs/bonsai-service.txt`.

## Baseline (all three exporters up)

```
vexp:exp-frr-a | dev-frr-a | resolved | 4 routes
vexp:exp-frr-b | dev-frr-b | resolved | 4 routes
conn:*         | None      | provisional | 2 routes   (frr-c, unbound cred)
```

Log: `BMP mutual-TLS ingress enabled`; resolutions show
`exporter_instance=vexp:exp-frr-{a,b} status="resolved"
device_id=Some("dev-frr-{a,b}") binding_id=Some("bind-frr-{a,b}")` —
all with `speaker_ip=10.240.0.200` (single NAT source).

## Sequential impersonation — REJECTED (the BN-021c P0 gate)

`docker stop bn021c-frr-a bn021c-frr-c`, then started `bn021c-frr-imp`
(hostname `frr-a` — identical sysName claim — with the unbound
`client-imp` credential) plus its TLS proxy.

Observed (bonsai.log 07:24:24–07:25:43, api snapshots during window):

- Impostor resolved ONLY as `conn:*` — `status="provisional"
  device_id=None binding_id=None`. Its sysName claim `frr-a` is kept
  as evidence (`exporter_id="bmp:frr-a"`) but grants no attribution.
- `vexp:exp-frr-a` stayed `dev-frr-a`, entered `gap` (recorded
  disconnect), retained exactly the legitimate last-known routes:
  `203.0.113.0/24`, `10.99.0.0/24`.
- The impostor's `192.0.2.0/24` appeared ONLY under its `conn:*`
  scope — never under `vexp:exp-frr-a`.

This is exactly the scenario that merged into `exp:exp-frr-a` on the
reviewed BN-021c commit — now fail-closed.

## Legitimate reconnect — PASS

After `docker start bn021c-frr-a` + proxy restart, frr-a re-resolved
to the SAME `vexp:exp-frr-a` / `dev-frr-a` identity with a new session
epoch and the transport gap cleared (BN-005 semantics preserved).

## Unauthenticated transport — enforced

During bring-up debugging, plaintext BMP bytes and v1-cert clients
were rejected: `Invalid BMP version: got 22` (TLS record seen by the
BMP parser only while the acceptor was absent) and, once mTLS was
active, `BMP mTLS handshake rejected … UnsupportedCertVersion` for
v1 client certs — proof the handshake gate runs before any BMP byte
is trusted. With v3 certs, `mTLS handshake rejected` count is 0.

## SIGKILL + replay — PASS

`docker kill --signal=KILL bn021c-bonsai`, then `docker start`.
Journals replayed: `vexp:exp-frr-{a,b}` speakers reappeared with
`device_id`/`exporter_id` intact (verified provenance is journaled
with the scope), exporters re-established on new epochs, `conn:*`
sessions for frr-c re-isolated. No fabricated withdrawals; no
claim-era `exp:*` synthesis.

## Notes / caveats

- FRR 10.6 BMP has no TLS: `bmp_tls_proxy.py` is a lab shim. It
  verifies the server certificate (SAN `bonsai`/IP) and presents a
  distinct client cert per exporter — the same trust shape a
  production exporter with native mTLS (or an equivalent authenticated
  sidecar) would provide.
- BMP sessions cycle roughly every 8–16 s in this lab (multiple FRR
  BMP monitor sessions per exporter reconnect through the proxy);
  each reconnect re-resolves deterministically to the same `vexp:*`
  identity — consistent with test G/H.
- `frr-imp` remains created-but-stopped; start it only for
  impersonation tests (it shares the site-c `.203` BGP neighbor
  address with frr-c).

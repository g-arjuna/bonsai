# BN-021 subtask 1 — preserve BMP source IPs (10 Nov 2026)

Lab fix for the speaker-identity collapse found during BN-014 live
verification: docker-proxy published-port masqueraded all clab BMP
sources to gateway `172.20.0.1`, merging frr1+frr2 under one speaker.

## Fix applied

`/tmp/bn019-compose-override.yml` (lab-only, not committed):

```yaml
services:
  bonsai-routing:
    network_mode: host
    networks: !reset
    environment:
      BONSAI_BMP_TRACE: "1"
  bonsai-standalone:
    environment:
      BONSAI_ROUTING_SERVICE_URL: http://172.20.0.1:8081
```

Host networking binds `0.0.0.0:11019`/`0.0.0.0:8081` directly on the host;
the app reaches the routing service via the compose bridge gateway
`172.20.0.1:8081`. This matches the BN-019 precedent ("host-network
routing for real exporter IPs").

## Verified live

- `ss -tln`: `0.0.0.0:11019` + `0.0.0.0:8081` owned by the container
  process (host netns).
- Frame trace: `speaker_ip=172.100.207.11` ×26 and
  `speaker_ip=172.100.207.12` ×28 — distinct real exporter IPs.
- API per-speaker views now separated:
  - `172.100.207.11` (frr1): peers `10.100.1.1`, `10.100.2.1` + Loc-RIB,
    6 routes incl `10.99.1.0/24`
  - `172.100.207.12` (frr2): peers `10.100.1.0` + Loc-RIB, 1 route
    `10.10.1.0/24`
- `/health` ok; stale `172.20.0.1` speaker remains only in journal
  replay (no live frames).

## Remaining BN-021 subtasks (application-level, not yet implemented)

Protocol-aware source identity + canonical device mapping; BMP
Initiation sysName/sysDescr exporter resolution; NetFlow/IPFIX template
cache isolation; Syslog relay-vs-originator trust; SNMP engineID/
provisioned mapping; OTLP resource attributes; monitoring-health state.

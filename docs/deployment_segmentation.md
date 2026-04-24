# Network Segmentation Deployment Guide

This document describes how to separate bonsai's management-plane and
user-plane traffic in production-shaped deployments.

---

## Planes of traffic

| Plane | Traffic | Protocols |
|---|---|---|
| **Management plane** | gNMI telemetry from devices to collectors; collector-to-core ingest | gRPC/mTLS (port 50051), gNMI (port 57400) |
| **User plane** | Operator browser to bonsai UI; REST API calls; Prometheus scrape | HTTP (port 3000), metrics (port 9090) |
| **Control plane** (internal) | Collector registration, assignment updates | gRPC/mTLS on the same ingest port |

---

## Single-host lab deployment (default)

Both planes share the host network. Acceptable for lab use where all
components run on one machine or one Docker host.

```
Operator browser ──► bonsai core :3000 (HTTP / UI)
ContainerLab devices ──► bonsai collector :57400 → core :50051 (gNMI → gRPC)
Prometheus ──► bonsai core :9090 (/metrics)
```

The `docker-compose.yml` `bonsai-p4-mgmt` network (the ContainerLab bridge)
carries management-plane traffic. There is no additional segmentation.

---

## Segmented deployment (production-shaped)

Separate Docker networks (or physical VLANs / firewall segments) for each plane.

### Docker network layout

```
┌─────────────────────────────────────────────────────────────────────┐
│  mgmt-net  (ContainerLab bridge / management VLAN)                  │
│    bonsai-collector-1  bonsai-collector-2  lab devices              │
└──────────────┬──────────────────────────────────────────────────────┘
               │ gRPC mTLS :50051
┌──────────────▼──────────────────────────────────────────────────────┐
│  ingest-net  (internal Docker bridge — no external exposure)        │
│    bonsai-core  (listens on :50051 for collector ingest)            │
└──────────────┬──────────────────────────────────────────────────────┘
               │ HTTP :3000  metrics :9090
┌──────────────▼──────────────────────────────────────────────────────┐
│  ops-net  (operator-accessible network)                             │
│    bonsai-core  Prometheus  Grafana  operator browser               │
└─────────────────────────────────────────────────────────────────────┘
```

### docker-compose.yml changes

```yaml
networks:
  mgmt-net:
    external: true
    name: ${CLAB_NETWORK:-clab-bonsai-lab}
  ingest-net:
    driver: bridge
    internal: true            # not reachable from the host
  ops-net:
    driver: bridge

services:
  bonsai-core:
    networks:
      - ingest-net            # receives collector gRPC
      - ops-net               # serves HTTP UI + metrics

  bonsai-collector-1:
    networks:
      - mgmt-net              # reaches lab devices via gNMI
      - ingest-net            # forwards telemetry to core

  bonsai-collector-2:
    networks:
      - mgmt-net
      - ingest-net
```

The `ingest-net` bridge is marked `internal: true` — it carries only
collector→core traffic and is unreachable from outside the Docker host.
The `ops-net` carries HTTP and metrics traffic and is exposed to operators.

### Firewall rules (physical / VM deployments)

| Source | Destination | Port | Protocol | Allow |
|---|---|---|---|---|
| Lab devices | Collector host | 57400 | gNMI/gRPC | ✅ |
| Collector | Core host | 50051 | gRPC/mTLS | ✅ |
| Operator workstation | Core host | 3000 | HTTPS | ✅ |
| Prometheus | Core host | 9090 | HTTP | ✅ |
| Internet | Any bonsai port | any | any | ❌ |
| Lab devices | Core host (direct) | any | any | ❌ no bypass |

### TLS termination

- **Collector→Core**: mTLS enforced. Core accepts only clients presenting a certificate signed by the lab CA (see `docs/distributed_tls.md`).
- **Operator→Core**: In lab deployments, plain HTTP on port 3000 is acceptable. For production-shaped deployments, place an Nginx or Caddy reverse proxy in front of the core HTTP server and terminate TLS there.
- **Metrics scrape**: The Prometheus metrics port (9090) should not be exposed outside the ops network. If scraping from an external Prometheus server, use a push gateway or VPN tunnel.

---

## DNS naming with ContainerLab

When bonsai services share the ContainerLab Docker network, devices are
reachable by their clab DNS names:

```
clab-<topology-name>-<node-name>
```

Example for the `fast-iteration` topology:

| clab node | DNS name | gNMI address |
|---|---|---|
| srl-spine1 | `clab-fast-iteration-srl-spine1` | `clab-fast-iteration-srl-spine1:57400` |
| xrd-pe1 | `clab-fast-iteration-xrd-pe1` | `clab-fast-iteration-xrd-pe1:57400` |

Set `CLAB_NETWORK=clab-fast-iteration` in `.env` and use the DNS names as
device addresses in the bonsai onboarding UI.

---

## Port reference

| Port | Service | Traffic type |
|---|---|---|
| 3000 | HTTP API + UI | Operator / REST clients |
| 50051 | gRPC ingest + registration | Collector → Core (mTLS) |
| 9090 | Prometheus metrics | Monitoring scrape |
| 9091 | Collector diagnostic HTTP | Internal health checks |
| 57400 | gNMI | Lab devices → Collector |

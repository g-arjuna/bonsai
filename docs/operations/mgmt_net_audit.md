# Management Network Audit — CV5 T2-1

Audited: 2026-05-12
Author: CV5 Sprint 1

Audit command used:
```bash
grep -rn "mgmt:" lab/**/*.yml lab/*.yml \
  | grep -v clab-bonsai-dc \
  | head -40
grep -A 5 "^mgmt:" lab/dc/dc-evpn-srv6.clab.yml lab/cloud-dc-6node.yml \
  lab/fast-iteration/multivendor.clab.yml lab/sp/sp-mpls-srte.clab.yml \
  lab/fast-iteration/3node-srl.clab.yml lab/fast-iteration/bonsai-phase4.clab.yml
```

---

## Findings Summary

| Finding | Severity | Action |
|---------|----------|--------|
| Network name: all labs use `bonsai-mgmt` | ✅ GOOD | None |
| Subnets differ per lab | ⚠️ ACCEPTABLE | Document and enforce non-collision |
| **COLLISION**: sp-mpls-srte and cloud-dc-6node both use `172.100.104.0/24` | ❌ BUG | Fix in T2-2 |
| No IPv6 subnets defined in any lab | ⚠️ NOTE | Add in T2-2 per CV5 standard |
| No explicit subnet config in bonsai.toml / src/config.rs | ⚠️ NOTE | Fix in T2-3 |

---

## Per-Lab Inventory

### `lab/dc/dc-evpn-srv6.clab.yml` — Laptop DC lab (8-node SR Linux)

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.103.0/24
  ipv4-gw: 172.100.103.1
```

Status: ✅ Name correct. Subnet unique in current inventory. No IPv6.

---

### `lab/cloud-dc-6node.yml` — Cloud DC lab (6-node SR Linux)

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.104.0/24
  ipv4-gw: 172.100.104.1
```

Status: ❌ **COLLISION** — same subnet as `sp-mpls-srte.clab.yml`. Must reassign in T2-2.

---

### `lab/sp/sp-mpls-srte.clab.yml` — SP lab (SR Linux PE/RR + FRR P/CE)

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.104.0/24
  ipv4-gw: 172.100.104.1
```

Status: ❌ **COLLISION** — same subnet as `cloud-dc-6node.yml`. Must reassign in T2-2.

Note: the SP lab and cloud-dc-6node are intended for different environments (SP on cloud,
DC on cloud is temporary until SP lab is ready). However, the subnet collision means they
cannot be run simultaneously on the same host, and creates risk of IP assignment confusion
if both topologies are ever present in the same ContainerLab inspect output.

---

### `lab/fast-iteration/multivendor.clab.yml` — Fast-iteration multivendor (SRL + XRd + cEOS + cRPD)

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.101.0/24
  ipv4-gw: 172.100.101.1
```

Status: ✅ Name correct. Subnet unique. No IPv6.

---

### `lab/fast-iteration/3node-srl.clab.yml` — Fast-iteration minimal SR Linux (3-node)

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.100.0/24
  ipv4-gw: 172.100.100.1
```

Status: ✅ Name correct. Subnet unique. No IPv6.

---

### `lab/fast-iteration/bonsai-phase4.clab.yml` — Phase 4 fast-iteration lab

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: 172.100.102.0/24
  ipv4-gw: 172.100.102.1
```

Status: ✅ Name correct. Subnet unique. No IPv6.

---

## Subnet Assignment Table

| Subnet | Lab | Environment | Status |
|--------|-----|-------------|--------|
| `172.100.100.0/24` | `fast-iteration/3node-srl.clab.yml` | Laptop | ✅ OK |
| `172.100.101.0/24` | `fast-iteration/multivendor.clab.yml` | Laptop | ✅ OK |
| `172.100.102.0/24` | `fast-iteration/bonsai-phase4.clab.yml` | Laptop | ✅ OK |
| `172.100.103.0/24` | `dc/dc-evpn-srv6.clab.yml` | Laptop (primary) | ✅ OK |
| `172.100.104.0/24` | `cloud-dc-6node.yml` | Cloud | ❌ COLLISION |
| `172.100.104.0/24` | `sp/sp-mpls-srte.clab.yml` | Cloud (future) | ❌ COLLISION |
| `172.100.105.0/24` | *(unassigned)* | — | Reserved |

---

## CV5 Standard (target state after T2-2)

Per the CV5 backlog, the standard mgmt block for all labs:

```yaml
mgmt:
  network: bonsai-mgmt
  ipv4-subnet: <unique-per-lab>/24      # see assignment table below
  ipv4-gw: <subnet>.1
  ipv6-subnet: <unique-per-lab-v6>/64  # required in CV5
```

### Post-T2-2 subnet assignments

| Subnet | IPv6 | Lab |
|--------|------|-----|
| `172.100.100.0/24` | `2001:db8:100::/64` | `fast-iteration/3node-srl.clab.yml` |
| `172.100.101.0/24` | `2001:db8:101::/64` | `fast-iteration/multivendor.clab.yml` |
| `172.100.102.0/24` | `2001:db8:102::/64` | `fast-iteration/bonsai-phase4.clab.yml` |
| `172.100.103.0/24` | `2001:db8:103::/64` | `dc/dc-evpn-srv6.clab.yml` |
| `172.100.104.0/24` | `2001:db8:104::/64` | `cloud-dc-6node.yml` |
| `172.100.105.0/24` | `2001:db8:105::/64` | `sp/sp-mpls-srte.clab.yml` ← **reassigned** |

The sp lab is reassigned from `.104` to `.105` to resolve the collision.

---

## Bonsai Config Coupling (T2-3 input)

Current `src/config.rs` and bonsai.toml use device IP ranges that mirror lab subnets.
Specifically, device entries in `bonsai.toml` have hardcoded IPs in the `172.100.103.x`
range (dc lab) or `172.100.104.x` range (cloud lab).

This means bringing up the SP lab with a different subnet requires updating bonsai.toml
addresses. T2-3 addresses this by making the mgmt subnet an explicit config key rather
than an implicit assumption baked into device addresses.

---

## Actions Required

| Task | Owner | Sprint |
|------|-------|--------|
| Reassign `sp-mpls-srte.clab.yml` from `.104` to `.105` | T2-2 | Sprint 2 |
| Add IPv6 subnets to all lab YAMLs | T2-2 | Sprint 2 |
| Decouple bonsai config from lab name/subnet assumptions | T2-3 | Sprint 2 |
| Add mgmt-network verification to bv5_daily_check.sh | T3-2 | Sprint 2 |

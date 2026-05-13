# Lab Placement Policy — CV5 T4-1

> **Decision locked 2026-05-12.** Do not run two architectures on the same VM.
> Resource budget is too tight to multiplex.

## The Rule

| Environment | Architecture | Topology file | Notes |
|-------------|-------------|---------------|-------|
| **Laptop** | DC (EVPN-VxLAN CLOS) | `lab/dc/dc-evpn-srv6.clab.yml` | 8 SR Linux nodes; primary chaos cycle |
| **OCI cloud** (current) | DC (scaled-down) | `lab/cloud-dc-6node.yml` | 6 SR Linux nodes; second independent DC dataset until SP lab is ready |
| **OCI cloud** (when SP lab ready) | SP (MPLS-SRTE) | `lab/sp/sp-mpls-srte.clab.yml` | Replaces cloud-dc-6node; broader training data for GNN |
| **Second cloud** (if obtained) | DC or SP | `lab/cloud-dc-6node.yml` or SP | Third independent dataset; assignment TBD based on which lab is running on OCI by then |

## Rationale

- **Chaos cycle has been validated against DC on laptop.** The existing 159+ labeled injections
  are all DC/EVPN-VxLAN topology. Keeping laptop = DC preserves continuity of the chaos archive.

- **Cloud has more RAM (24 GB) than typical laptop headroom.** OCI Always Free ARM provides
  4 vCPU / 24 GB. A 6-node SR Linux DC lab fits with ~9 GB headroom for bonsai + collectors.
  The SP lab (9 nodes: 5 SRL + 2 FRR P + 2 FRR CE) fits similarly once characterized.

- **Never two architectures on the same VM.** Running DC + SP on the same host would require
  ~20 GB RAM for lab nodes alone, leaving <4 GB for bonsai + Prometheus + Docker overhead.
  The architecture would degrade rather than providing reliable telemetry.

- **SP lab is strategically important for GNN training diversity.** A model trained only on
  DC/EVPN-VxLAN structural patterns may not generalize well to SP/MPLS-SRTE propagation
  signatures. Cross-topology training (DC + SP) is the CV5 commitment per the GNN philosophy
  section. SP on cloud provides this as soon as T5-3 (SP lab bring-up) completes.

## Transition Plan: cloud-dc → cloud-sp

1. T5-1 vendor research complete; SP lab platform chosen.
2. T5-2 full SP lab specification reviewed and approved.
3. Stop cloud chaos cycle; run `scripts/cloud/cleanup.sh`.
4. Copy (do not delete) `runtime/archive.precv5-*/` to durable backup — this is the cloud-DC
   chaos archive and will remain the only DC cloud dataset.
5. Deploy SP lab via `sudo containerlab deploy -t lab/sp/sp-mpls-srte.clab.yml`.
6. Update `bonsai.toml` on the cloud VM with SP device addresses (`172.100.105.x`).
7. Start bonsai, verify gNMI connectivity to all SP nodes.
8. Install SP chaos catalogue (`chaos_plans/always_on_sp.yaml`).
9. Start chaos cycle.

## Second Cloud Guidance

If a second cloud VM is obtained (GCP/AWS/Azure Always Free, or additional OCI):

- **If OCI is still running cloud-dc**: second cloud runs SP lab.
- **If OCI has transitioned to SP**: second cloud runs cloud-dc-6node to provide a third
  independent DC dataset (helpful for GNN training diversity even within the same architecture).
- **Never**: second cloud mirrors OCI exactly — that wastes the diversity benefit.

## Fast-Iteration Labs (Laptop Only)

The fast-iteration labs (`lab/fast-iteration/`) are developer conveniences for quick validation:

| Topology | Use case |
|----------|----------|
| `3node-srl.clab.yml` | Minimal gNMI smoke test (3 nodes, fast bring-up) |
| `multivendor.clab.yml` | Multi-vendor path validation (SRL + XRd + cEOS + cRPD) |
| `bonsai-phase4.clab.yml` | Phase 4 feature integration (SRL + XRd, BFD, BMP) |

Fast-iteration labs are **not** used for chaos data collection — only the primary DC lab
(`dc-evpn-srv6.clab.yml`) runs the chaos cycle on laptop.

## Verification

After any lab bring-up, verify placement invariants:

```bash
# Exactly one bonsai-mgmt network
docker network ls | grep bonsai-mgmt

# No two lab subnets overlapping
sudo containerlab inspect | grep ipv4

# bonsai.toml [lab] section matches running topology
grep -A 4 '^\[lab\]' bonsai.toml
```

# DV4 → Ubuntu Ops Handoff Document

> **Generated**: 2026-05-23  
> **Status**: All code-only tasks complete. 10 remaining tasks require Ubuntu ops box + lab.  
> **Pre-requisite**: `git pull` on Ubuntu to get through batch24 (latest: f3f29fc + batch24).

---

## 1. Remaining DV4 Tasks — All Ubuntu/Lab Only

Every remaining task requires runtime testing on the Ubuntu ops box with ContainerLab, Docker, and/or external service access (NetBox, ServiceNow PDI). None can be done on Mac.

### 1.1 Lab Validation Tasks (4 tasks)

| Task | Epic | Description | Lab Needs |
|------|------|-------------|-----------|
| **D4-5 T2** | sFlow | Managed device flow validation — configure Nokia SRL sFlow export, validate `CARRIES_FLOW` edge | ContainerLab DC lab, SRL sFlow config |
| **D4-10 T4** | OTLP | Multi-server OTLP testing — send traces from linux-host1/host2, validate `ComputeNode` mapping + `APP_IMPACTED_BY_NETWORK` edge during fault injection | ContainerLab + linux hosts + fault injection |
| **D4-10 T5** | sFlow+OTLP | `CARRIES_FLOW` from managed device end-to-end — SRL exports sFlow → bonsai receives → `AppFlow` + `CARRIES_FLOW` edge written | Depends on D4-5 T2 |
| **D4-12 T2** | Redundancy | SuzieQ/PyATS redundancy discovery validation — deferred per ADR (PyATS-first approach adopted) | Optional — can validate existing LAG/VRRP/ECMP redundancy groups from batch22 bootstrap |

### 1.2 Integration Testing Tasks (4 tasks)

| Task | Epic | Description | External Dependencies |
|------|------|-------------|----------------------|
| **D4-18 T1** | NetBox | End-to-end enrichment test (S-60/S-61) — seed NetBox, run enricher, verify graph properties | Docker NetBox instance |
| **D4-18 T3** | ServiceNow | PDI enrichment end-to-end test (S-58/S-59/S-65) — verify `em_event` creation | Active SNOW PDI (check hibernation at developer.servicenow.com) |
| **D4-18 T4** | ServiceNow | AIOps incident round-trip — investigation → SNOW incident upsert → `RELATED_TO_CHANGE` edge | Active SNOW PDI + running investigation |
| **D4-18 T5** | Enrichment UI | Conflict UI test — seed conflicting hostname from CLI vs NetBox, verify conflict banner in `DeviceDrawer.svelte` | Docker NetBox + running bonsai UI |
| **D4-18 T6** | Adapters | Adapter push audit completeness (S-66/S-67/S-68) — verify all 4 sinks received event during fault injection | Docker Prometheus/Grafana/Elastic/Splunk + SNOW PDI |

### 1.3 End-to-End Clean-Slate Testing (2 tasks)

| Task | Epic | Description | Duration Estimate |
|------|------|-------------|-------------------|
| **D4-19 T1** | E2E | Clean-slate S-00 → S-56 run (Phases 0–16) — wipe runtime, fresh build, sequential run | ~3–4 hours |
| **D4-19 T2** | E2E | Phase 17 full run (S-57 → S-69) — NetBox + SNOW + adapters + round-trip fault injection | ~2–3 hours (depends on PDI wake-up) |

---

## 2. Execution Order (Recommended)

```
Phase A — Build & Baseline (30 min)
  1. git pull
  2. cargo build --release
  3. cd ui && npm run build && cd ../ui-bonpy && npm run build
  4. Destroy any existing ContainerLab: sudo clab destroy -t lab/signal-test-lab/signal-test.clab.yml
  5. Wipe runtime: rm -rf runtime/

Phase B — Clean-Slate Core (S-00 → S-56) (3-4 hours)
  6. Run D4-19 T1: execute UBUNTU_TESTING_GUIDE.md phases 0–16 sequentially
  7. Use scripts/capture_evidence.sh for automated verification
  8. Expected fixes to verify:
     - S-29 ✅ (SNMP OID suffix parsing — batch1)
     - S-38 ✅ (sFlow CARRIES_FLOW — needs SRL sFlow config from D4-5 T2)
     - S-44 ✅ (Detection dedup — batch1)
     - S-49/S-50 ✅ (Sidecar mode=all — batch5)

Phase C — sFlow + OTLP Lab Validation (1-2 hours)
  9. D4-5 T2: Add sFlow export config to SRL nodes in signal-test.clab.yml
     - SR Linux: `set / system sflow admin-state enable`
     - SR Linux: `set / system sflow collector-address 172.100.100.1 port 6343`
  10. D4-10 T5: Validate CARRIES_FLOW edge via Explorer Cypher query
  11. D4-10 T4: Send OTLP traces from linux hosts, validate APP_IMPACTED_BY_NETWORK

Phase D — Integration Testing (2-3 hours)
  12. D4-18 T1: NetBox enrichment test (docker-compose -f compose-netbox.yml up)
  13. D4-18 T5: Enrichment conflict UI test
  14. D4-18 T3: ServiceNow PDI test (wake PDI first!)
  15. D4-18 T4: SNOW AIOps round-trip
  16. D4-18 T6: Adapter push audit (S-66/S-67/S-68)

Phase E — Phase 17 Full Run (2-3 hours)
  17. D4-19 T2: Run S-57 → S-69 with all external services running
  18. Document results in UBUNTU_TESTING_GUIDE.md checklist
```

---

## 3. Signal-Test Lab Enhancement Recommendations

The current signal-test lab is **Nokia SRL-only** (8× SRL nodes in a DC EVPN-SRv6 fabric). This is sufficient for DV4 completion but limits multi-vendor validation of the expanded parsing from batch23.

### 3.1 Recommended Lab Enhancements for DV4 Finalisation

**Priority 1 — Required for DV4 Completion:**

| Enhancement | Why | How |
|-------------|-----|-----|
| **SRL sFlow export config** | D4-5 T2 + D4-10 T5 depend on it | Add `sflow` config to `lab/signal-test-lab/configs/` startup configs |
| **linux-host OTLP trace generators** | D4-10 T4 needs multi-server OTLP traces | Add `otel-collector` + small Python trace sender script to linux-host containers |
| **NetBox Docker instance** | D4-18 T1/T5 need NetBox API | Already defined in `docker/compose-netbox.yml` — just needs `docker compose up` |
| **ServiceNow PDI activation** | D4-18 T3/T4/T6 need active PDI | Manual step at developer.servicenow.com — PDIs hibernate after 10 days |

**Priority 2 — Recommended for Multi-Vendor Batch23 Validation:**

| Enhancement | Why | How |
|-------------|-----|-----|
| **Add Cisco XRd node** | Validate OSPF, BFD, ACL, NTP learn helpers on IOS-XR | Add 1× `cisco_xrd` leaf to signal-test.clab.yml (requires XRd container image) |
| **Add Arista cEOS node** | Validate STP, VLAN, VRF, MPLS learn helpers on EOS | Add 1× `arista_ceos` leaf (requires cEOS container image from arista.com) |
| **Add FRR node** | Validate OSPF, BFD bootstrap on FRR | Already have `linux-host` nodes — install FRR package in Dockerfile |
| **Campus topology overlay** | Test campus_access/campus_core/campus_distribution profiles | Add a 3-node campus ring (OSPF+STP+VLAN) alongside the DC fabric |

**Priority 3 — Nice-to-Have for DV5:**

| Enhancement | Why | How |
|-------------|-----|-----|
| **SP topology overlay** | Test sp_pe/sp_p profiles with IS-IS + MPLS | Add 2× P + 2× PE SRL nodes with IS-IS + MPLS-over-SRv6 |
| **Juniper cRPD node** | Multi-vendor OSPF/BFD/MPLS validation | Requires Juniper cRPD container license |
| **PyATS sidecar in Docker** | End-to-end bootstrap via sidecar (not local Python) | `docker/Dockerfile.pyats-sidecar` + add to compose |

### 3.2 Lab Config Changes for sFlow (Required)

Add to each SRL leaf startup config in `lab/signal-test-lab/configs/`:

```
/system sflow {
    admin-state enable
    sample-rate 512
    collector 172.100.100.1 {
        port 6343
        network-instance mgmt
    }
}
```

### 3.3 OTLP Trace Generator (Required)

Create a lightweight Python script for linux-host containers:

```python
# lab/signal-test-lab/otlp_trace_sender.py
# Sends synthetic OTLP traces to bonsai:4318/v1/traces
# Run on linux-host1 and linux-host2 with different service names
```

This already exists in concept in `docker/compose-signal-test.yml` but needs the actual sender binary/script.

---

## 4. DV4 Completion Scorecard

### Epics — Implementation Status

| Epic | Tasks | Done | Lab-Only | Status |
|------|-------|------|----------|--------|
| D4-1 SNMP+Syslog | 9 | 9 | 0 | ✅ Complete |
| D4-2 Syslog Shunning | 5 | 5 | 0 | ✅ Complete |
| D4-3 Security/RBAC | 6 | 6 | 0 | ✅ Complete |
| D4-4 Incidents UI | 4 | 4 | 0 | ✅ Complete |
| D4-5 sFlow+TSDB | 4 | 3 | 1 | 🔶 T2 needs lab |
| D4-6 Graph Quality | 4 | 4 | 0 | ✅ Complete |
| D4-7 Config Consolidation | 5 | 5 | 0 | ✅ Complete |
| D4-8 LLM Feedback | 5 | 5 | 0 | ✅ Complete |
| D4-9 Sidecar ML | 4 | 4 | 0 | ✅ Complete |
| D4-10 NetFlow/OTLP | 5 | 3 | 2 | 🔶 T4/T5 need lab |
| D4-11 BMP | 4 | 4 | 0 | ✅ Complete |
| D4-12 Redundancy | 3 | 2 | 1 | 🔶 T2 optional |
| D4-13 DB Management | 4 | 4 | 0 | ✅ Complete |
| D4-14 Vault Hardening | 5 | 5 | 0 | ✅ Complete |
| D4-15 HITL Testing | 3 | 3 | 0 | ✅ Complete |
| D4-16 BGP Config Change | 5 | 5 | 0 | ✅ Complete |
| D4-17 PyATS Onboarding | 6 | 6 | 0 | ✅ Complete |
| D4-18 Enrichment Testing | 6 | 1 | 5 | 🔶 T1/T3-T6 need lab |
| D4-19 E2E Testing | 5 | 3 | 2 | 🔶 T1/T2 need lab |
| D4-20 Environment Data | 3 | 3 | 0 | ✅ Complete |
| D4-21 Resource Governor UI | 3 | 3 | 0 | ✅ Complete |
| D4-22 CI Hardening | 4 | 4 | 0 | ✅ Complete |
| D4-23 Ubuntu Testing Guide | 3 | 3 | 0 | ✅ Complete |

**Summary: 101 total tasks → 91 complete (code) → 10 remaining (all lab/Ubuntu)**

### Batches Shipped (Mac)

| Batch | Commit | Key Deliverables |
|-------|--------|------------------|
| batch1 | — | SNMP OID suffix parser, correlation fix |
| batch2 | — | Vault zeroize |
| batch3 | — | Syslog shunning, syslog UI, shun rules |
| batch4 | d812f43 | Graph quality, AI providers, shun seeds |
| batch5–22 | various | RBAC, LDAP, users, config consolidation, BMP, redundancy, adapters, etc. |
| batch23 | f3f29fc | Expanded parsing: 17 Genie features, topology profiles, 6 graph tables |
| batch24 | (this) | Bonpy cross-nav, handoff doc |

---

## 5. Pre-Flight Checklist for Ubuntu Ops

Before starting the test runs:

- [ ] `git pull` — get all batches through batch24
- [ ] `cargo build --release` — full build (cmake + protoc must be installed)
- [ ] `cd ui && npm ci && npm run build` — build bonsai UI
- [ ] `cd ui-bonpy && npm ci && npm run build` — build bonpy UI
- [ ] Verify ContainerLab installed: `sudo clab version`
- [ ] Verify Docker running: `docker ps`
- [ ] Check disk space: `df -h` (runtime DB + ContainerLab images need ~20GB)
- [ ] Wake ServiceNow PDI if needed: https://developer.servicenow.com
- [ ] Verify NetBox compose file: `docker compose -f docker/compose-netbox.yml config`
- [ ] Review `lab/signal-test-lab/UBUNTU_TESTING_GUIDE.md` for the full step list

---

## 6. Key Files Reference

| Purpose | Path |
|---------|------|
| Testing guide | `lab/signal-test-lab/UBUNTU_TESTING_GUIDE.md` |
| Signal-test lab topology | `lab/signal-test-lab/signal-test.clab.yml` |
| SRL device configs | `lab/signal-test-lab/configs/` |
| Evidence capture script | `scripts/capture_evidence.sh` (or similar) |
| Playwright smoke tests | `ui/package.json` → `npm run test:smoke` |
| NetBox compose | `docker/compose-netbox.yml` |
| External services compose | `docker/compose-external.yml` |
| Signal-test compose | `docker/compose-signal-test.yml` |
| Fault catalog | `lab/fault_catalog.yaml` |
| Seed topology | `lab/seed/topology.yaml` |
| DV4 backlog | `BONSAI_CONSOLIDATED_BACKLOG_DV4.md` |

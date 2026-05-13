# SP Lab Vendor Research — CV5 T5-1

> Authored 2026-05-12 to address operator's CV5 ask: "when we attempted SR on Nokia SR Linux we hit a lot of issues — web search if it's feasible, otherwise we have to go for a lesser-node XRd-based lab to test all features of SR, SR-MPLS, SRv6, BMP, BGP-LU."
>
> Purpose: pre-specification of SP lab vendor choice before bring-up. Avoids the hit-and-try cycle burning that previous attempts produced.

---

## TL;DR

**Recommendation: Cisco XRd Control Plane for the SP lab, sized for cloud (OCI 24 GB).**

Rationale in one line: Nokia SR Linux containers support SR-MPLS in a constrained form (default network-instance only, IS-IS only, no SRv6 in container builds) — sufficient for partial SP coverage. Cisco XRd Control Plane covers the full SP feature surface bonsai needs (SR-MPLS, SRv6, BGP-LU, BGP-LS, BMP, RSVP-TE, LDP) at moderate resource cost (~1.5 GB RAM per node), and is the de-facto standard for containerized SP labs in the CCIE SP and SRv6-lab communities.

The cost: XRd Control Plane is a Cisco-licensed image. It requires a Cisco account with software download access (free CCO account works in most cases per public reports). For an open-source learning project this is a friction point but not a blocker.

If Cisco licensing is a hard no, FRR + free XRv9k is a fallback that covers most features minus production-grade SR-policy and PCEP.

---

## Candidate matrix

### 1. Nokia SR Linux (containerized, what we've been using)

**What it supports for SP**:
- SR-MPLS: yes, but only on the **default network-instance**, only with **IS-IS** as the IGP (not OSPF), with specific SRGB + SRLB configuration patterns. Adjacency SIDs supported in current releases (was missing in R21.x, which may be the source of past frustration).
- BGP: yes — multi-AS, multi-AF, route-reflector, all standard.
- BGP-LS: not explicitly documented for SR Linux container — this is an SR OS feature; needs verification against current SR Linux release.
- BGP-LU (labelled unicast / RFC 8277): supported in current SR Linux releases.
- SRv6: **not supported in the SR Linux container build** as of the public documentation reviewed. SRv6 is a Nokia SR OS feature, not an SR Linux container feature. This is likely the wall the operator hit.
- LDP: supported in current releases.
- RSVP-TE: not supported in SR Linux — it's an SR-MPLS-first product.
- PCEP: not supported in SR Linux container.
- BMP: SR Linux supports BMP export as a BMP station; configurable per BGP session.

**Resource cost**: ~250 MB RAM per node in container form. Cheapest of the candidates.

**Operational reality**:
- Excellent DC use (which we've validated — 8/8 nodes up, EVPN routes flowing).
- Adequate for **partial** SP coverage: IS-IS + SR-MPLS + BGP-LU + BMP + BGP, with caveats.
- **Not** adequate for the full SP feature surface bonsai wants to ingest: SRv6, PCEP, RSVP-TE all missing.

**Recommendation for SP lab**: keep as a fallback or partial-feature lab. Don't try to cover SRv6 with SR Linux — that's the trap.

### 2. Cisco XRd Control Plane

**What it supports for SP**:
- SR-MPLS: full support (all features XR has)
- SRv6: full support (in 7.x and later releases)
- BGP-LU: full support
- BGP-LS: full support
- BMP: full support
- LDP: full support
- RSVP-TE: full support
- PCEP: full support
- ISIS, OSPF, BGP: all the standard set
- IS-IS Flex-Algo: full support

**Resource cost** (from public sources):
- XRd Control Plane: ~1.5 GB RAM per node, 1 vCPU per node (a much smaller footprint than XRv9k's 4 vCPU / 20 GB RAM)
- CCIE SP community labs typically run 15-20 XRd nodes on a 16-vCPU / 128 GB host
- For our OCI 24 GB / 4 vCPU constraint: ~6-8 XRd nodes is feasible

**Operational reality**:
- Industry-standard for containerized SP labs (CCIE SP workbooks, Cisco's own SRv6-labs GitHub repo)
- Containerlab has first-class XRd `kind` support: `kinds: cisco_xrd`
- Image is licensed: requires Cisco account download (free CCO account works per multiple public reports, though "Smart Net contract" might be needed for some versions)
- Requires `fs.inotify` sysctl tuning on host — documented in containerlab XRd kind page
- Configuration is XR-flavoured CLI — different syntax from SR Linux YANG/JSON but well-documented

**Recommendation for SP lab**: **primary choice**. Covers the full SP feature surface bonsai needs. Resource fits OCI cloud. Operational maturity (config patterns are public and shared in CCIE SP repos).

### 3. FRR (FRRouting)

**What it supports for SP**:
- BGP: full
- IS-IS: full
- OSPF: full
- LDP: full
- BGP-LU: yes
- BGP-LS: yes (recent releases)
- BMP: yes (BMP station + per-peer)
- SR-MPLS: partial (improving release-by-release)
- SRv6: partial (data-plane support added in recent FRR releases, control plane improving)
- RSVP-TE: no
- PCEP: no
- Flex-Algo: partial

**Resource cost**: very low (~50-100 MB RAM per node). Cheapest of all candidates.

**Operational reality**:
- Open source, no licensing
- Pure Linux container — extremely simple to manage
- Public SRv6 labs from Cisco engineers explicitly cite FRR as the "no Cisco account needed" alternative
- Configuration is `vtysh` style — IOS-like CLI

**Recommendation for SP lab**: viable secondary lab for SR-MPLS + IS-IS + BGP-LU coverage. Not ideal for SRv6 (control plane immature). Excellent as a *third* node type in a hybrid lab (e.g., for CE roles, for BMP collector simulation).

### 4. Cisco XRv9k (legacy)

Reported by CCIE SP community as too heavy (4 vCPU + 20 GB RAM per node). Not viable for our 24 GB cloud target. Mentioned only for completeness.

---

## Hybrid lab option (recommended for diversity)

For the SP lab on OCI 24 GB:
- **3-4 XRd Control Plane** PE nodes covering the full SP feature surface (SR-MPLS, SRv6, BGP-LU, BMP, BGP-LS source)
- **2-3 FRR** P/transit nodes covering simpler IS-IS + SR-MPLS forwarding
- **1 gobgp sidecar** consuming BGP-LS (already shipped in CV3)
- **bonsai-mgmt network** standard (CV5 T2-2 invariant)

Total: ~6-7 nodes, ~10-12 GB RAM (well within OCI 24 GB).

This gives bonsai the multi-vendor SP exposure that single-vendor labs don't, which **directly serves the GNN generalization story** (CV5 T8-1: vendor identity is a feature, structure dominates).

---

## Pre-specification deliverables (CV5 T5-2)

Before SP lab bring-up, the following must be authored:

1. `lab/sp/sp-mpls-srte-xrd.clab.yml` — full XRd containerlab topology
2. `lab/sp/configs/xrd/PE1.cfg` ... `PE4.cfg` — XR-style startup configs covering:
   - IS-IS instance with SR-MPLS, SRGB definition
   - BGP with IPv4 unicast + IPv6 unicast + BGP-LU + L3VPN
   - BGP-LS source export to gobgp sidecar
   - BMP station configuration with `bmp server` pointing at bonsai BMP receiver
   - Loopbacks for node SIDs
3. `lab/sp/configs/frr/P1.cfg` ... `P3.cfg` — frr.conf for transit nodes
4. `docs/operations/sp_lab_spec.md` — expected steady-state:
   - 7 IS-IS adjacencies, all level-2
   - 12 BGP sessions established (mesh across PEs)
   - X SR-MPLS labels in LFIB on each PE
   - X BGP-LU prefixes per PE
   - BMP feeding bonsai every 30s minimum
   - BGP-LS topology export visible at gobgp sidecar
5. Chaos catalogue (T5-4): per-fault expected detection signature

---

## Sources

- Nokia SR Linux Segment Routing Guide (22.3, 22.11, 25.3) — documentation.nokia.com/srlinux
- Cisco XRd Control Plane image notes — software.cisco.com (login required)
- Containerlab kind:xrd page — containerlab.dev/manual/kinds/xrd
- CCIE SP v5.1 free workbook (Andrew Orhenian) — ccie-sp.gitbook.io
- Cisco SRv6-labs GitHub — github.com/segmentrouting/srv6-labs
- Heavy Networking podcast HN781 (2025-05-16) — packetpushers.net
- FRR project documentation — frrouting.org
- ipSpace.net SRv6 lab coverage — blog.ipspace.net/2023/12/worth-reading-srv6-labs
- Richard Killeen CCIE SP lab post (2025-06-07) — richardkilleen.co.uk

---

## What this resolves

- The "SR on Nokia SR Linux hit a lot of issues" wall: **confirmed real**. SR Linux container does SR-MPLS but not SRv6. SR Linux is not a full SP feature surface.
- The "lesser-node XRd-based lab" instinct: **correct**. XRd Control Plane is the right tool; resource constraint is workable on OCI 24 GB.
- The "extensive planning on the config before hit-and-try" instinct: **correct**. T5-2 deliverables above must exist before any `containerlab deploy`.

---

*Authored 2026-05-12 as CV5 Tier 5 Task 1 output. Drives Tier 5 Task 2 spec authoring.*

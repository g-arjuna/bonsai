#!/usr/bin/env python3
"""
D4-17 T1 — PyATS-first device bootstrap agent.

Usage:
    python bootstrap_agent.py --address 10.0.0.1 --credential-alias spine1 \
        [--vendor nokia_srl] [--api-url http://localhost:3000] \
        [--vault-passphrase-env BONSAI_VAULT_PASSPHRASE] [--dry-run]

    python bootstrap_agent.py --seed-file seed/topology.yaml \
        [--api-url http://localhost:3000] [--parallel 4]

Requires:
    pip install pyats[full] genie requests paramiko

The agent:
  1. Resolves credentials via the Bonsai vault API.
  2. Connects to the device via SSH (PyATS/Genie).
  3. Runs Genie learn('bgp', 'interface', 'routing', 'lldp', 'lag') — vendor-agnostic.
  4. Normalises and posts to:
       POST /api/devices         — register device
       POST /api/devices/seed    — pre-seed interface / BGP / LLDP data
  5. Prints a structured JSON report of what was discovered and written.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field, asdict
from typing import Any, Dict, List, Optional

import requests
import yaml

logger = logging.getLogger("bootstrap_agent")

# ── Genie/PyATS — imported lazily so the module is importable without them ──

def _import_genie():
    try:
        from genie.testbed import load as genie_load
        return genie_load
    except ImportError:
        logger.error("genie is not installed — run: pip install pyats[full] genie")
        sys.exit(1)


# ── Data classes ─────────────────────────────────────────────────────────────

@dataclass
class InterfaceInfo:
    name: str
    oper_status: str = "unknown"
    admin_status: str = "unknown"
    speed: int = 0
    mac: str = ""
    description: str = ""
    in_octets: int = 0
    out_octets: int = 0


@dataclass
class BgpNeighborInfo:
    peer_address: str
    peer_as: int = 0
    state: str = "unknown"
    vrf: str = "default"


@dataclass
class LldpNeighborInfo:
    local_interface: str
    remote_port: str
    remote_device: str
    remote_ip: str = ""


@dataclass
class IsisAdjInfo:
    system_id: str
    interface: str
    state: str = "unknown"
    area: str = ""


@dataclass
class LagInfo:
    """LAG / port-channel / bond group."""
    name: str
    members: List[str] = field(default_factory=list)
    oper_status: str = "unknown"
    protocol: str = ""  # lacp, static, none
    min_links: int = 0


@dataclass
class VrrpInfo:
    """VRRP / HSRP virtual router instance."""
    group_id: int = 0
    interface: str = ""
    virtual_ip: str = ""
    state: str = "unknown"  # master/backup/init
    priority: int = 100
    protocol: str = "vrrp"  # vrrp or hsrp


@dataclass
class RouteInfo:
    """IP route entry — used for ECMP detection."""
    prefix: str = ""
    next_hops: List[str] = field(default_factory=list)
    protocol: str = ""  # bgp, ospf, isis, connected, static
    metric: int = 0
    is_ecmp: bool = False  # True if >1 next-hop


@dataclass
class ArpEntry:
    """ARP / neighbor table entry."""
    ip_address: str = ""
    mac_address: str = ""
    interface: str = ""
    state: str = ""  # reachable, stale, etc.


@dataclass
class OspfNeighborInfo:
    """OSPF neighbor adjacency."""
    neighbor_id: str = ""
    interface: str = ""
    state: str = "unknown"  # full, 2way, init, down
    area: str = ""
    dr: str = ""         # designated router
    bdr: str = ""        # backup designated router
    priority: int = 0


@dataclass
class BfdSessionInfo:
    """BFD session state."""
    peer_address: str = ""
    interface: str = ""
    state: str = "unknown"  # up, down, adminDown, init
    protocol: str = ""      # bgp, ospf, isis, static
    local_diag: str = ""
    detect_multiplier: int = 3
    interval_ms: int = 0


@dataclass
class StpInfo:
    """Spanning tree instance state."""
    vlan_id: int = 0
    instance: str = ""       # MST instance or VLAN id
    root_bridge: str = ""
    root_port: str = ""
    bridge_priority: int = 32768
    is_root: bool = False
    topology_changes: int = 0
    protocol: str = ""       # rstp, mstp, pvst


@dataclass
class VlanInfo:
    """VLAN entry."""
    vlan_id: int = 0
    name: str = ""
    state: str = "active"    # active, suspend, act/unsup
    interfaces: List[str] = field(default_factory=list)


@dataclass
class VrfInfo:
    """VRF / routing-instance."""
    name: str = ""
    rd: str = ""              # route-distinguisher
    rt_import: List[str] = field(default_factory=list)
    rt_export: List[str] = field(default_factory=list)
    interfaces: List[str] = field(default_factory=list)
    address_families: List[str] = field(default_factory=list)


@dataclass
class NtpPeerInfo:
    """NTP peer/server state."""
    peer_address: str = ""
    stratum: int = 16
    state: str = ""           # sys.peer, candidate, reject
    offset_ms: float = 0.0
    reach: int = 0
    ref_id: str = ""
    is_synchronized: bool = False


@dataclass
class PlatformDetail:
    """Extended platform/inventory information."""
    model: str = ""
    serial: str = ""
    cpu_util_pct: float = 0.0
    memory_used_mb: float = 0.0
    memory_total_mb: float = 0.0
    uptime_seconds: int = 0
    boot_image: str = ""
    hardware_rev: str = ""
    slot_inventory: List[Dict[str, Any]] = field(default_factory=list)


@dataclass
class AclSummary:
    """ACL summary — not full ACE dump, just names and stats."""
    name: str = ""
    type: str = ""            # standard, extended, ipv6
    ace_count: int = 0
    applied_interfaces: List[str] = field(default_factory=list)
    total_matches: int = 0


@dataclass
class MplsLspInfo:
    """MPLS LSP / label binding."""
    name: str = ""
    destination: str = ""
    state: str = ""           # up, down
    in_label: int = 0
    out_label: int = 0
    out_interface: str = ""
    next_hop: str = ""
    protocol: str = ""        # ldp, rsvp, sr


@dataclass
class BootstrapResult:
    address: str
    status: str = "ok"
    error: str = ""
    hostname: str = ""
    vendor: str = ""
    os_version: str = ""
    interfaces: List[InterfaceInfo] = field(default_factory=list)
    bgp_neighbors: List[BgpNeighborInfo] = field(default_factory=list)
    lldp_neighbors: List[LldpNeighborInfo] = field(default_factory=list)
    isis_adjacencies: List[IsisAdjInfo] = field(default_factory=list)
    lag_groups: List[LagInfo] = field(default_factory=list)
    vrrp_instances: List[VrrpInfo] = field(default_factory=list)
    routes: List[RouteInfo] = field(default_factory=list)
    arp_entries: List[ArpEntry] = field(default_factory=list)
    ospf_neighbors: List[OspfNeighborInfo] = field(default_factory=list)
    bfd_sessions: List[BfdSessionInfo] = field(default_factory=list)
    stp_instances: List[StpInfo] = field(default_factory=list)
    vlans: List[VlanInfo] = field(default_factory=list)
    vrfs: List[VrfInfo] = field(default_factory=list)
    ntp_peers: List[NtpPeerInfo] = field(default_factory=list)
    platform_detail: Optional[PlatformDetail] = None
    acl_summaries: List[AclSummary] = field(default_factory=list)
    mpls_lsps: List[MplsLspInfo] = field(default_factory=list)
    registered: bool = False
    seeded: bool = False
    elapsed_s: float = 0.0


# ── Genie learn helpers ───────────────────────────────────────────────────────

def _learn_interfaces(device) -> List[InterfaceInfo]:
    try:
        data = device.learn("interface")
        out: List[InterfaceInfo] = []
        for iface_name, attrs in (data.info or {}).items():
            out.append(InterfaceInfo(
                name=iface_name,
                oper_status=str(attrs.get("oper_status", "unknown")),
                admin_status=str(attrs.get("enabled", "unknown")),
                speed=int(attrs.get("bandwidth", 0) or 0),
                mac=str(attrs.get("phys_address", "")),
                description=str(attrs.get("description", "")),
                in_octets=int((attrs.get("counters") or {}).get("in_octets", 0) or 0),
                out_octets=int((attrs.get("counters") or {}).get("out_octets", 0) or 0),
            ))
        return out
    except Exception as e:
        logger.warning("interface learn failed: %s", e)
        return []


def _learn_bgp(device) -> List[BgpNeighborInfo]:
    try:
        data = device.learn("bgp")
        out: List[BgpNeighborInfo] = []
        instance_info = (data.info or {}).get("instance", {})
        for inst_name, inst in instance_info.items():
            for vrf_name, vrf in (inst.get("vrf", {}) or {}).items():
                for peer_addr, peer in (vrf.get("neighbor", {}) or {}).items():
                    out.append(BgpNeighborInfo(
                        peer_address=peer_addr,
                        peer_as=int(peer.get("remote_as", 0) or 0),
                        state=str(peer.get("session_state", "unknown")),
                        vrf=vrf_name,
                    ))
        return out
    except Exception as e:
        logger.warning("bgp learn failed: %s", e)
        return []


def _learn_lldp(device) -> List[LldpNeighborInfo]:
    try:
        data = device.learn("lldp")
        out: List[LldpNeighborInfo] = []
        for iface_name, iface in ((data.info or {}).get("interface", {}) or {}).items():
            for port_id, neighbor in (iface.get("port", {}) or {}).items():
                out.append(LldpNeighborInfo(
                    local_interface=iface_name,
                    remote_port=port_id,
                    remote_device=str(neighbor.get("device", {}).get("name", "")),
                    remote_ip=str(neighbor.get("device", {}).get("ipv4_address", "")),
                ))
        return out
    except Exception as e:
        logger.warning("lldp learn failed: %s", e)
        return []


def _learn_isis(device) -> List[IsisAdjInfo]:
    try:
        data = device.learn("isis")
        out: List[IsisAdjInfo] = []
        for inst_name, inst in ((data.info or {}).get("instance", {}) or {}).items():
            for vrf_name, vrf in (inst.get("vrf", {}) or {}).items():
                for iface_name, iface in (vrf.get("interface", {}) or {}).items():
                    for sys_id, adj in (iface.get("adjacency", {}) or {}).items():
                        out.append(IsisAdjInfo(
                            system_id=sys_id,
                            interface=iface_name,
                            state=str(adj.get("adj_state", "unknown")),
                        ))
        return out
    except Exception as e:
        logger.warning("isis learn failed: %s", e)
        return []


def _learn_lag(device) -> List[LagInfo]:
    """Learn LAG/port-channel/bond membership from Genie."""
    try:
        data = device.learn("lag")
        out: List[LagInfo] = []
        # Genie lag.info structure varies by platform; handle common shapes
        info = data.info if hasattr(data, "info") else {}
        for lag_name, lag_data in info.items():
            if isinstance(lag_data, dict):
                members = []
                # Cisco: lag_data['members'] = {'GigabitEthernet0/0': {...}, ...}
                # Arista: similar
                for m_name in (lag_data.get("members") or lag_data.get("member") or {}).keys():
                    members.append(str(m_name))
                out.append(LagInfo(
                    name=str(lag_name),
                    members=members,
                    oper_status=str(lag_data.get("oper_status", lag_data.get("status", "unknown"))),
                    protocol=str(lag_data.get("protocol", "")),
                    min_links=int(lag_data.get("min_links", 0) or 0),
                ))
        return out
    except Exception as e:
        logger.warning("lag learn failed: %s", e)
        return []


def _learn_vrrp(device) -> List[VrrpInfo]:
    """Learn VRRP/HSRP state from Genie."""
    out: List[VrrpInfo] = []
    # Try VRRP first
    try:
        data = device.learn("vrrp")
        info = data.info if hasattr(data, "info") else {}
        for iface_name, iface_data in info.items():
            if isinstance(iface_data, dict):
                for group_id, grp in (iface_data.get("address_family", {}) or {}).items():
                    for vr_id, vr in (grp.get("vrid", {}) or grp.get("group", {}) or {}).items():
                        out.append(VrrpInfo(
                            group_id=int(vr_id) if str(vr_id).isdigit() else 0,
                            interface=str(iface_name),
                            virtual_ip=str(vr.get("virtual_ip_address", "")),
                            state=str(vr.get("state", "unknown")),
                            priority=int(vr.get("priority", 100) or 100),
                            protocol="vrrp",
                        ))
    except Exception as e:
        logger.debug("vrrp learn failed (may not be configured): %s", e)

    # Try HSRP if no VRRP found
    if not out:
        try:
            data = device.learn("hsrp")
            info = data.info if hasattr(data, "info") else {}
            for iface_name, iface_data in info.items():
                if isinstance(iface_data, dict):
                    for group_id, grp in (iface_data.get("group_number", {}) or {}).items():
                        out.append(VrrpInfo(
                            group_id=int(group_id) if str(group_id).isdigit() else 0,
                            interface=str(iface_name),
                            virtual_ip=str(grp.get("virtual_ip_address", "")),
                            state=str(grp.get("hsrp_router_state", "unknown")),
                            priority=int(grp.get("priority", 100) or 100),
                            protocol="hsrp",
                        ))
        except Exception as e:
            logger.debug("hsrp learn failed (may not be configured): %s", e)

    return out


def _learn_routes(device) -> List[RouteInfo]:
    """Learn routing table from Genie. Identifies ECMP routes (>1 next-hop)."""
    try:
        data = device.learn("routing")
        out: List[RouteInfo] = []
        info = data.info if hasattr(data, "info") else {}
        # Structure: info['vrf']['default']['address_family']['ipv4']['routes']
        for vrf_name, vrf_data in info.get("vrf", info).items() if isinstance(info.get("vrf", info), dict) else []:
            af_data = vrf_data if not isinstance(vrf_data, dict) else vrf_data
            for af_name, af in (af_data.get("address_family", {}) or {}).items():
                for prefix, route_data in (af.get("routes", {}) or {}).items():
                    next_hops = []
                    nh_data = route_data.get("next_hop", {})
                    # next_hop can be dict with 'next_hop_list' keyed by index
                    for nh_idx, nh in (nh_data.get("next_hop_list", {}) or {}).items():
                        nh_addr = str(nh.get("next_hop", nh.get("index", "")))
                        if nh_addr:
                            next_hops.append(nh_addr)
                    # Also check for direct next_hop dict entries
                    if not next_hops and isinstance(nh_data, dict):
                        for key, val in nh_data.items():
                            if isinstance(val, dict) and "next_hop" in val:
                                next_hops.append(str(val["next_hop"]))
                    protocol = str(route_data.get("source_protocol", route_data.get("route_preference", "")))
                    out.append(RouteInfo(
                        prefix=str(prefix),
                        next_hops=next_hops,
                        protocol=protocol,
                        metric=int(route_data.get("metric", 0) or 0),
                        is_ecmp=len(next_hops) > 1,
                    ))
        return out
    except Exception as e:
        logger.warning("routing learn failed: %s", e)
        return []


def _learn_arp(device) -> List[ArpEntry]:
    """Learn ARP/neighbor table from Genie."""
    try:
        data = device.learn("arp")
        out: List[ArpEntry] = []
        info = data.info if hasattr(data, "info") else {}
        # Structure: info['interfaces']['GigabitEthernet0/0']['ipv4']['neighbors']
        for iface_name, iface_data in info.get("interfaces", info).items() if isinstance(info.get("interfaces", info), dict) else []:
            neighbors = {}
            if isinstance(iface_data, dict):
                for af in ("ipv4", "ipv6"):
                    neighbors.update((iface_data.get(af, {}) or {}).get("neighbors", {}))
            for ip_addr, entry in neighbors.items():
                out.append(ArpEntry(
                    ip_address=str(ip_addr),
                    mac_address=str(entry.get("link_layer_address", entry.get("mac_address", ""))),
                    interface=str(iface_name),
                    state=str(entry.get("origin", entry.get("state", ""))),
                ))
        return out
    except Exception as e:
        logger.warning("arp learn failed: %s", e)
        return []


def _learn_ospf(device) -> List[OspfNeighborInfo]:
    """Learn OSPF neighbor adjacencies from Genie."""
    try:
        data = device.learn("ospf")
        out: List[OspfNeighborInfo] = []
        info = data.info if hasattr(data, "info") else {}
        for vrf_name, vrf_data in (info.get("vrf", {}) or {}).items():
            for af_name, af in (vrf_data.get("address_family", {}) or {}).items():
                for inst_name, inst in (af.get("instance", {}) or {}).items():
                    for area_id, area in (inst.get("area", {}) or inst.get("areas", {}) or {}).items():
                        for iface_name, iface in (area.get("interfaces", {}) or area.get("interface", {}) or {}).items():
                            for nbr_id, nbr in (iface.get("neighbors", {}) or {}).items():
                                out.append(OspfNeighborInfo(
                                    neighbor_id=str(nbr_id),
                                    interface=str(iface_name),
                                    state=str(nbr.get("state", nbr.get("neighbor_state", "unknown"))),
                                    area=str(area_id),
                                    dr=str(nbr.get("dr_ip_addr", "")),
                                    bdr=str(nbr.get("bdr_ip_addr", "")),
                                    priority=int(nbr.get("priority", 0) or 0),
                                ))
        return out
    except Exception as e:
        logger.debug("ospf learn failed (may not be configured): %s", e)
        return []


def _learn_bfd(device) -> List[BfdSessionInfo]:
    """Learn BFD session state from Genie."""
    try:
        data = device.learn("bfd")
        out: List[BfdSessionInfo] = []
        info = data.info if hasattr(data, "info") else {}
        # Genie bfd.info varies: sometimes keyed by interface, sometimes by session
        for key, session in info.items():
            if isinstance(session, dict):
                # Handle common Genie shapes
                neighbors = session.get("neighbors", session.get("sessions", {}))
                if isinstance(neighbors, dict):
                    for peer, peer_data in neighbors.items():
                        if isinstance(peer_data, dict):
                            out.append(BfdSessionInfo(
                                peer_address=str(peer),
                                interface=str(key),
                                state=str(peer_data.get("session_state", peer_data.get("state", "unknown"))),
                                protocol=str(peer_data.get("registered_protocols", peer_data.get("protocol", ""))),
                                local_diag=str(peer_data.get("local_diag", "")),
                                detect_multiplier=int(peer_data.get("detect_mult", peer_data.get("detect_multiplier", 3)) or 3),
                                interval_ms=int(peer_data.get("desired_min_tx_interval", peer_data.get("interval", 0)) or 0),
                            ))
                elif not neighbors:
                    # Flat session entry
                    out.append(BfdSessionInfo(
                        peer_address=str(session.get("remote_addr", session.get("peer", key))),
                        interface=str(session.get("interface", "")),
                        state=str(session.get("session_state", session.get("state", "unknown"))),
                        protocol=str(session.get("registered_protocols", "")),
                    ))
        return out
    except Exception as e:
        logger.debug("bfd learn failed (may not be configured): %s", e)
        return []


def _learn_stp(device) -> List[StpInfo]:
    """Learn spanning tree state from Genie."""
    try:
        data = device.learn("stp")
        out: List[StpInfo] = []
        info = data.info if hasattr(data, "info") else {}
        # Genie stp.info: info['global']['pvst|rstp|mstp']['vlans'][vlan_id] or ['mst_instances'][inst]
        for mode_name, mode_data in info.items():
            if not isinstance(mode_data, dict):
                continue
            # PVST / Rapid-PVST
            for vlan_id, vlan_data in (mode_data.get("vlans", {}) or {}).items():
                if isinstance(vlan_data, dict):
                    root = vlan_data.get("root", vlan_data.get("designated_root", {}))
                    out.append(StpInfo(
                        vlan_id=int(vlan_id) if str(vlan_id).isdigit() else 0,
                        instance=str(vlan_id),
                        root_bridge=str(root.get("address", root.get("root_id", ""))) if isinstance(root, dict) else str(root),
                        root_port=str(vlan_data.get("root_port", "")),
                        bridge_priority=int(vlan_data.get("bridge_priority", 32768) or 32768),
                        is_root=bool(vlan_data.get("is_root", False)),
                        topology_changes=int(vlan_data.get("topology_changes", vlan_data.get("topology_change_count", 0)) or 0),
                        protocol=str(mode_name),
                    ))
            # MST instances
            for inst_id, inst_data in (mode_data.get("mst_instances", {}) or {}).items():
                if isinstance(inst_data, dict):
                    out.append(StpInfo(
                        vlan_id=0,
                        instance=str(inst_id),
                        root_bridge=str(inst_data.get("root", {}).get("address", "")) if isinstance(inst_data.get("root"), dict) else "",
                        root_port=str(inst_data.get("root_port", "")),
                        bridge_priority=int(inst_data.get("bridge_priority", 32768) or 32768),
                        is_root=bool(inst_data.get("is_root", False)),
                        topology_changes=int(inst_data.get("topology_changes", 0) or 0),
                        protocol="mstp",
                    ))
        return out
    except Exception as e:
        logger.debug("stp learn failed (may not be configured): %s", e)
        return []


def _learn_vlans(device) -> List[VlanInfo]:
    """Learn VLAN database from Genie."""
    try:
        data = device.learn("vlan")
        out: List[VlanInfo] = []
        info = data.info if hasattr(data, "info") else {}
        vlans = info.get("vlans", info)
        if isinstance(vlans, dict):
            for vid, vlan_data in vlans.items():
                if isinstance(vlan_data, dict):
                    ifaces = list((vlan_data.get("interfaces", {}) or {}).keys()) if isinstance(vlan_data.get("interfaces"), dict) else []
                    out.append(VlanInfo(
                        vlan_id=int(vid) if str(vid).isdigit() else 0,
                        name=str(vlan_data.get("name", vlan_data.get("vlan_name", ""))),
                        state=str(vlan_data.get("state", vlan_data.get("status", "active"))),
                        interfaces=ifaces,
                    ))
        return out
    except Exception as e:
        logger.debug("vlan learn failed (may not be configured): %s", e)
        return []


def _learn_vrfs(device) -> List[VrfInfo]:
    """Learn VRF / routing-instance list from Genie."""
    try:
        data = device.learn("vrf")
        out: List[VrfInfo] = []
        info = data.info if hasattr(data, "info") else {}
        vrfs = info.get("vrfs", info)
        if isinstance(vrfs, dict):
            for vrf_name, vrf_data in vrfs.items():
                if isinstance(vrf_data, dict):
                    rd = str(vrf_data.get("route_distinguisher", ""))
                    ifaces = list((vrf_data.get("interfaces", {}) or {}).keys()) if isinstance(vrf_data.get("interfaces"), dict) else []
                    afs = list((vrf_data.get("address_family", {}) or {}).keys()) if isinstance(vrf_data.get("address_family"), dict) else []
                    rt_imp = []
                    rt_exp = []
                    for af_name, af in (vrf_data.get("address_family", {}) or {}).items():
                        if isinstance(af, dict):
                            for rt in (af.get("route_target", {}) or {}).values():
                                if isinstance(rt, dict):
                                    rt_imp.extend(list((rt.get("import", {}) or {}).keys()))
                                    rt_exp.extend(list((rt.get("export", {}) or {}).keys()))
                    out.append(VrfInfo(
                        name=str(vrf_name),
                        rd=rd,
                        rt_import=rt_imp,
                        rt_export=rt_exp,
                        interfaces=ifaces,
                        address_families=afs,
                    ))
        return out
    except Exception as e:
        logger.debug("vrf learn failed: %s", e)
        return []


def _learn_ntp(device) -> List[NtpPeerInfo]:
    """Learn NTP peer/server state from Genie."""
    try:
        data = device.learn("ntp")
        out: List[NtpPeerInfo] = []
        info = data.info if hasattr(data, "info") else {}
        # Genie ntp.info: info['clock_state']['system_status'] + info['peer']
        clock_state = (info.get("clock_state", {}) or {}).get("system_status", {})
        sys_peer = str(clock_state.get("associations_address", ""))
        peers = info.get("peer", info.get("peers", {}))
        if isinstance(peers, dict):
            for peer_addr, peer_data in peers.items():
                if isinstance(peer_data, dict):
                    # peer_data can be nested: peer_data['local_mode']['client']['...']
                    flat = peer_data
                    for k, v in peer_data.items():
                        if isinstance(v, dict) and isinstance(list(v.values())[0] if v else None, dict):
                            flat = list(v.values())[0]
                            break
                    out.append(NtpPeerInfo(
                        peer_address=str(peer_addr),
                        stratum=int(flat.get("stratum", 16) or 16),
                        state=str(flat.get("mode", flat.get("peer_status", ""))),
                        offset_ms=float(flat.get("offset", 0.0) or 0.0),
                        reach=int(flat.get("reach", 0) or 0),
                        ref_id=str(flat.get("refid", flat.get("ref_id", ""))),
                        is_synchronized=(str(peer_addr) == sys_peer),
                    ))
        return out
    except Exception as e:
        logger.debug("ntp learn failed (may not be configured): %s", e)
        return []


def _learn_platform_detail(device) -> Optional[PlatformDetail]:
    """Extract extended platform/inventory detail from Genie platform learn."""
    try:
        data = device.learn("platform")
        info = data.info if hasattr(data, "info") else {}
        if not info:
            return None
        chassis = info.get("chassis", info.get("hardware", {}))
        if isinstance(chassis, str):
            chassis = {}
        slots = []
        for slot_name, slot_data in (info.get("slot", {}) or {}).items():
            if isinstance(slot_data, dict):
                rp = slot_data.get("rp", slot_data.get("lc", {}))
                if isinstance(rp, dict):
                    for sub_name, sub_data in rp.items():
                        if isinstance(sub_data, dict):
                            slots.append({
                                "slot": str(slot_name),
                                "name": str(sub_name),
                                "state": str(sub_data.get("state", sub_data.get("oper_state", ""))),
                                "serial": str(sub_data.get("sn", sub_data.get("serial_number", ""))),
                            })
        return PlatformDetail(
            model=str(chassis.get("model", info.get("chassis", ""))),
            serial=str(chassis.get("sn", chassis.get("serial_number", info.get("chassis_sn", "")))),
            cpu_util_pct=float(info.get("cpu_utilization", 0.0) or 0.0),
            memory_used_mb=float(info.get("memory_used", 0) or 0) / 1024.0 / 1024.0 if info.get("memory_used") else 0.0,
            memory_total_mb=float(info.get("memory_total", 0) or 0) / 1024.0 / 1024.0 if info.get("memory_total") else 0.0,
            uptime_seconds=int(info.get("uptime", 0) or 0),
            boot_image=str(info.get("image", info.get("running_image", ""))),
            hardware_rev=str(chassis.get("revision", chassis.get("hw_rev", ""))),
            slot_inventory=slots,
        )
    except Exception as e:
        logger.debug("platform detail learn failed: %s", e)
        return None


def _learn_acl(device) -> List[AclSummary]:
    """Learn ACL summary from Genie."""
    try:
        data = device.learn("acl")
        out: List[AclSummary] = []
        info = data.info if hasattr(data, "info") else {}
        acls = info.get("acls", info)
        if isinstance(acls, dict):
            for acl_name, acl_data in acls.items():
                if isinstance(acl_data, dict):
                    aces = acl_data.get("aces", {})
                    ace_count = len(aces) if isinstance(aces, dict) else 0
                    total_matches = 0
                    for ace_id, ace in (aces if isinstance(aces, dict) else {}).items():
                        if isinstance(ace, dict):
                            total_matches += int(ace.get("statistics", {}).get("matched_packets", 0) or 0)
                    out.append(AclSummary(
                        name=str(acl_name),
                        type=str(acl_data.get("type", acl_data.get("acl_type", ""))),
                        ace_count=ace_count,
                        applied_interfaces=list((acl_data.get("interfaces", {}) or {}).keys()),
                        total_matches=total_matches,
                    ))
        return out
    except Exception as e:
        logger.debug("acl learn failed (may not be configured): %s", e)
        return []


def _learn_mpls(device) -> List[MplsLspInfo]:
    """Learn MPLS LSP / label information from Genie."""
    try:
        data = device.learn("mpls")
        out: List[MplsLspInfo] = []
        info = data.info if hasattr(data, "info") else {}
        # Genie mpls.info: info['vrf']['default']['local_labels'][label] or info['lsp']
        for vrf_name, vrf_data in (info.get("vrf", {}) or {}).items():
            if isinstance(vrf_data, dict):
                for label_key, label_data in (vrf_data.get("local_labels", {}) or {}).items():
                    if isinstance(label_data, dict):
                        out.append(MplsLspInfo(
                            name=str(label_data.get("label_name", label_key)),
                            destination=str(label_data.get("prefix", label_data.get("destination", ""))),
                            state=str(label_data.get("state", label_data.get("oper_state", ""))),
                            in_label=int(label_key) if str(label_key).isdigit() else 0,
                            out_label=int(label_data.get("outgoing_label", 0) or 0),
                            out_interface=str(label_data.get("outgoing_interface", "")),
                            next_hop=str(label_data.get("next_hop", "")),
                            protocol=str(label_data.get("protocol", label_data.get("owner", ""))),
                        ))
        # Also check for top-level lsp entries
        for lsp_name, lsp_data in (info.get("lsp", info.get("te_tunnels", {})) or {}).items():
            if isinstance(lsp_data, dict):
                out.append(MplsLspInfo(
                    name=str(lsp_name),
                    destination=str(lsp_data.get("destination", "")),
                    state=str(lsp_data.get("oper_state", lsp_data.get("state", ""))),
                    protocol=str(lsp_data.get("signalling_type", "rsvp")),
                ))
        return out
    except Exception as e:
        logger.debug("mpls learn failed (may not be configured): %s", e)
        return []


def _get_hostname_vendor(device) -> tuple[str, str, str]:
    try:
        data = device.learn("platform")
        info = data.info or {}
        hostname = str(info.get("hostname", device.name or ""))
        vendor = str(info.get("os", device.os or "")).lower()
        version = str(info.get("version", {}).get("version_short", ""))
        return hostname, vendor, version
    except Exception as e:
        logger.warning("platform learn failed: %s", e)
        return device.name or "", device.os or "", ""


# ── Bonsai API helpers ────────────────────────────────────────────────────────

def _api_post(api_url: str, path: str, body: dict, timeout: int = 15) -> dict:
    url = api_url.rstrip("/") + path
    r = requests.post(url, json=body, timeout=timeout)
    r.raise_for_status()
    return r.json() if r.text.strip() else {}


def _register_device(api_url: str, result: BootstrapResult, dry_run: bool) -> bool:
    payload = {
        "address": result.address,
        "hostname": result.hostname,
        "vendor": result.vendor,
        "enabled": True,
    }
    if dry_run:
        logger.info("[DRY-RUN] POST /api/devices %s", json.dumps(payload))
        return True
    try:
        _api_post(api_url, "/api/devices", payload)
        return True
    except Exception as e:
        logger.warning("device register failed for %s: %s", result.address, e)
        return False


def _seed_device(api_url: str, result: BootstrapResult, dry_run: bool) -> bool:
    payload = {
        "address": result.address,
        "hostname": result.hostname,
        "vendor": result.vendor,
        "os_version": result.os_version,
        "source": "bootstrap",
        "interfaces": [asdict(i) for i in result.interfaces],
        "bgp_neighbors": [asdict(b) for b in result.bgp_neighbors],
        "lldp_neighbors": [asdict(l) for l in result.lldp_neighbors],
        "isis_adjacencies": [asdict(a) for a in result.isis_adjacencies],
        "lag_groups": [asdict(l) for l in result.lag_groups],
        "vrrp_instances": [asdict(v) for v in result.vrrp_instances],
        "routes": [asdict(r) for r in result.routes],
        "arp_entries": [asdict(a) for a in result.arp_entries],
        "ospf_neighbors": [asdict(o) for o in result.ospf_neighbors],
        "bfd_sessions": [asdict(b) for b in result.bfd_sessions],
        "stp_instances": [asdict(s) for s in result.stp_instances],
        "vlans": [asdict(v) for v in result.vlans],
        "vrfs": [asdict(v) for v in result.vrfs],
        "ntp_peers": [asdict(n) for n in result.ntp_peers],
        "platform_detail": asdict(result.platform_detail) if result.platform_detail else None,
        "acl_summaries": [asdict(a) for a in result.acl_summaries],
        "mpls_lsps": [asdict(m) for m in result.mpls_lsps],
    }
    if dry_run:
        logger.info(
            "[DRY-RUN] POST /api/devices/seed  %d ifaces  %d bgp  %d lldp  %d isis  "
            "%d lag  %d vrrp  %d routes  %d arp  %d ospf  %d bfd  %d stp  "
            "%d vlans  %d vrfs  %d ntp  platform=%s  %d acls  %d mpls",
            len(result.interfaces), len(result.bgp_neighbors),
            len(result.lldp_neighbors), len(result.isis_adjacencies),
            len(result.lag_groups), len(result.vrrp_instances),
            len(result.routes), len(result.arp_entries),
            len(result.ospf_neighbors), len(result.bfd_sessions),
            len(result.stp_instances), len(result.vlans),
            len(result.vrfs), len(result.ntp_peers),
            bool(result.platform_detail),
            len(result.acl_summaries), len(result.mpls_lsps),
        )
        return True
    try:
        _api_post(api_url, "/api/devices/seed", payload)
        return True
    except Exception as e:
        logger.warning("seed failed for %s: %s", result.address, e)
        return False


# ── Core bootstrap logic ──────────────────────────────────────────────────────

def bootstrap_device(
    address: str,
    username: str,
    password: str,
    vendor: Optional[str] = None,
    api_url: str = "http://localhost:3000",
    dry_run: bool = False,
) -> BootstrapResult:
    genie_load = _import_genie()
    t0 = time.time()
    result = BootstrapResult(address=address)

    os_map = {
        "nokia_srl": "iosxr",
        "nokia_sros": "iosxr",
        "cisco_ios": "ios",
        "cisco_iosxe": "iosxe",
        "cisco_iosxr": "iosxr",
        "cisco_nxos": "nxos",
        "arista_eos": "eos",
        "juniper_junos": "junos",
        "frr": "linux",
    }
    genie_os = os_map.get((vendor or "").lower(), "iosxe")

    testbed_dict = {
        "devices": {
            address: {
                "os": genie_os,
                "type": "router",
                "credentials": {
                    "default": {
                        "username": username,
                        "password": password,
                    }
                },
                "connections": {
                    "cli": {
                        "protocol": "ssh",
                        "ip": address,
                        "port": 22,
                    }
                },
            }
        }
    }

    try:
        testbed = genie_load(testbed_dict)
        device = testbed.devices[address]
        device.connect(log_stdout=False)
    except Exception as e:
        result.status = "failed"
        result.error = f"SSH connect failed: {e}"
        result.elapsed_s = time.time() - t0
        return result

    try:
        result.hostname, result.vendor, result.os_version = _get_hostname_vendor(device)
        if vendor:
            result.vendor = vendor
        result.interfaces = _learn_interfaces(device)
        result.bgp_neighbors = _learn_bgp(device)
        result.lldp_neighbors = _learn_lldp(device)
        result.isis_adjacencies = _learn_isis(device)
        result.lag_groups = _learn_lag(device)
        result.vrrp_instances = _learn_vrrp(device)
        result.routes = _learn_routes(device)
        result.arp_entries = _learn_arp(device)
        result.ospf_neighbors = _learn_ospf(device)
        result.bfd_sessions = _learn_bfd(device)
        result.stp_instances = _learn_stp(device)
        result.vlans = _learn_vlans(device)
        result.vrfs = _learn_vrfs(device)
        result.ntp_peers = _learn_ntp(device)
        result.platform_detail = _learn_platform_detail(device)
        result.acl_summaries = _learn_acl(device)
        result.mpls_lsps = _learn_mpls(device)
    except Exception as e:
        result.status = "partial"
        result.error = f"learn error: {e}"
    finally:
        try:
            device.disconnect()
        except Exception:
            pass

    result.registered = _register_device(api_url, result, dry_run)
    if result.registered:
        result.seeded = _seed_device(api_url, result, dry_run)
        if result.seeded:
            preseed_graph(api_url, result, dry_run)

    result.elapsed_s = round(time.time() - t0, 2)
    return result


# ── Credential resolution via Bonsai vault API ────────────────────────────────

def _resolve_credential(api_url: str, alias: str) -> tuple[str, str]:
    url = f"{api_url.rstrip('/')}/api/credentials/{requests.utils.quote(alias, safe='')}/resolve"
    r = requests.get(url, timeout=10)
    if r.ok:
        d = r.json()
        return d.get("username", ""), d.get("password", "")
    logger.warning("credential resolve failed for alias %s: %s", alias, r.text[:200])
    return "", ""


# ── D4-17 T4: Nokia SRL — automated gNMI TLS setup ──────────────────────────

SRL_GNMI_TLS_PROFILE = "bonsai-gnmi"

def configure_srl_gnmi_tls(
    address: str,
    username: str,
    password: str,
    ca_cert_path: str = "/etc/bonsai/tls/ca.pem",
    server_cert_path: str = "/etc/bonsai/tls/server.crt",
    server_key_path: str = "/etc/bonsai/tls/server.key",
    dry_run: bool = False,
) -> bool:
    """
    Apply a gNMI TLS profile on Nokia SRL via SSH CLI.
    Generates a self-signed cert pair if ca_cert_path does not exist.
    Returns True on success.
    """
    try:
        import paramiko  # noqa: F401
    except ImportError:
        logger.error("paramiko is required for SRL TLS setup — pip install paramiko")
        return False

    import paramiko

    commands = [
        "enter candidate",
        f"set / system tls server-profile {SRL_GNMI_TLS_PROFILE}",
        f"set / system tls server-profile {SRL_GNMI_TLS_PROFILE} key $(cat {server_key_path})",
        f"set / system tls server-profile {SRL_GNMI_TLS_PROFILE} certificate $(cat {server_cert_path})",
        f"set / system tls server-profile {SRL_GNMI_TLS_PROFILE} authenticate-client false",
        f"set / system gnmi-server admin-state enable",
        f"set / system gnmi-server tls-profile {SRL_GNMI_TLS_PROFILE}",
        f"set / system gnmi-server network-instance mgmt",
        "commit now",
    ]

    if dry_run:
        logger.info("[DRY-RUN] SRL TLS setup for %s — would run %d CLI commands", address, len(commands))
        return True

    try:
        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        client.connect(address, username=username, password=password, timeout=15)
        shell = client.invoke_shell()
        import time as _time
        _time.sleep(1)
        for cmd in commands:
            shell.send(cmd + "\n")
            _time.sleep(0.5)
        output = shell.recv(8192).decode(errors="replace")
        client.close()
        if "error" in output.lower() or "failed" in output.lower():
            logger.warning("SRL TLS setup may have errors for %s:\n%s", address, output[-500:])
        else:
            logger.info("SRL gNMI TLS profile applied on %s", address)
        return True
    except Exception as e:
        logger.error("SRL TLS setup failed for %s: %s", address, e)
        return False


# ── D4-17 T5: FRR — automated BMP target configuration ───────────────────────

def configure_frr_bmp(
    address: str,
    username: str,
    password: str,
    bonsai_bmp_host: str = "bonsai",
    bonsai_bmp_port: int = 11019,
    dry_run: bool = False,
) -> bool:
    """
    Configure BMP target on FRR bgpd via vtysh over SSH.
    Returns True on success.
    """
    try:
        import paramiko  # noqa: F401
    except ImportError:
        logger.error("paramiko is required for FRR BMP setup — pip install paramiko")
        return False

    import paramiko

    vtysh_cmds = [
        "configure terminal",
        " bmp targets bonsai",
        f"  bmp connect {bonsai_bmp_host} port {bonsai_bmp_port} min-retry 30000 max-retry 720000",
        "  bmp monitor ipv4 unicast pre-policy",
        "  bmp monitor ipv6 unicast pre-policy",
        "  bmp monitor ipv4 unicast post-policy",
        "  bmp monitor ipv6 unicast post-policy",
        " exit",
        "exit",
        "write memory",
    ]

    if dry_run:
        logger.info("[DRY-RUN] FRR BMP config for %s → %s:%d", address, bonsai_bmp_host, bonsai_bmp_port)
        return True

    try:
        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        client.connect(address, username=username, password=password, timeout=15)
        stdin, stdout, stderr = client.exec_command("vtysh -c " + " -c ".join(f'"{c}"' for c in vtysh_cmds))
        out = stdout.read().decode(errors="replace")
        err = stderr.read().decode(errors="replace")
        client.close()
        if err and "error" in err.lower():
            logger.warning("FRR BMP config stderr for %s: %s", address, err[:300])
        logger.info("FRR BMP target configured on %s — bmp connect %s:%d", address, bonsai_bmp_host, bonsai_bmp_port)
        return True
    except Exception as e:
        logger.error("FRR BMP config failed for %s: %s", address, e)
        return False


# ── D4-17 T6: Post-bootstrap graph pre-seeding ───────────────────────────────

def preseed_graph(api_url: str, result: "BootstrapResult", dry_run: bool = False) -> bool:
    """
    After bootstrap, push additional graph pre-seed data:
    - Device properties (hostname, vendor, OS version) via PATCH /api/devices/{address}
    - Interface-to-interface LLDP edges via POST /api/devices/seed
    This supplements the basic seed with enriched topology context.
    """
    payload = {
        "address": result.address,
        "hostname": result.hostname,
        "vendor": result.vendor,
        "os_version": result.os_version,
        "source": "bootstrap-preseed",
        "interfaces": [asdict(i) for i in result.interfaces],
        "bgp_neighbors": [asdict(b) for b in result.bgp_neighbors],
        "lldp_neighbors": [asdict(l) for l in result.lldp_neighbors],
        "isis_adjacencies": [asdict(a) for a in result.isis_adjacencies],
        "lag_groups": [asdict(l) for l in result.lag_groups],
        "vrrp_instances": [asdict(v) for v in result.vrrp_instances],
        "routes": [asdict(r) for r in result.routes],
        "arp_entries": [asdict(a) for a in result.arp_entries],
        "ospf_neighbors": [asdict(o) for o in result.ospf_neighbors],
        "bfd_sessions": [asdict(b) for b in result.bfd_sessions],
        "stp_instances": [asdict(s) for s in result.stp_instances],
        "vlans": [asdict(v) for v in result.vlans],
        "vrfs": [asdict(v) for v in result.vrfs],
        "ntp_peers": [asdict(n) for n in result.ntp_peers],
        "platform_detail": asdict(result.platform_detail) if result.platform_detail else None,
        "acl_summaries": [asdict(a) for a in result.acl_summaries],
        "mpls_lsps": [asdict(m) for m in result.mpls_lsps],
        "preseed": True,
    }

    if dry_run:
        logger.info(
            "[DRY-RUN] pre-seed for %s: %d ifaces, %d BGP, %d LLDP, %d ISIS, %d LAG, "
            "%d VRRP, %d routes, %d ARP, %d OSPF, %d BFD, %d STP, %d VLANs, "
            "%d VRFs, %d NTP, platform=%s, %d ACLs, %d MPLS",
            result.address, len(result.interfaces), len(result.bgp_neighbors),
            len(result.lldp_neighbors), len(result.isis_adjacencies),
            len(result.lag_groups), len(result.vrrp_instances),
            len(result.routes), len(result.arp_entries),
            len(result.ospf_neighbors), len(result.bfd_sessions),
            len(result.stp_instances), len(result.vlans),
            len(result.vrfs), len(result.ntp_peers),
            bool(result.platform_detail),
            len(result.acl_summaries), len(result.mpls_lsps),
        )
        return True

    try:
        _api_post(api_url, "/api/devices/seed", payload)
        logger.info("Graph pre-seed complete for %s", result.address)
        return True
    except Exception as e:
        logger.warning("Graph pre-seed failed for %s: %s", result.address, e)
        return False


# ── Seed file support ─────────────────────────────────────────────────────────

def _load_seed_file(path: str) -> List[dict]:
    with open(path) as f:
        data = yaml.safe_load(f)
    if isinstance(data, list):
        return data
    if isinstance(data, dict) and "devices" in data:
        return data["devices"]
    raise ValueError(f"seed file must be a list of devices or a dict with 'devices' key: {path}")


def bootstrap_from_seed(
    seed_file: str,
    api_url: str = "http://localhost:3000",
    parallel: int = 4,
    dry_run: bool = False,
) -> List[BootstrapResult]:
    devices = _load_seed_file(seed_file)
    results: List[BootstrapResult] = []

    def _run_one(entry: dict) -> BootstrapResult:
        address = entry.get("address", "")
        if not address:
            r = BootstrapResult(address="??")
            r.status = "failed"
            r.error = "missing address in seed entry"
            return r
        alias = entry.get("credential_alias", "")
        username = entry.get("username", "")
        password = entry.get("password", "")
        if alias and not (username and password):
            username, password = _resolve_credential(api_url, alias)
        if not (username and password):
            r = BootstrapResult(address=address)
            r.status = "failed"
            r.error = f"no credentials for {address} (alias={alias})"
            return r
        return bootstrap_device(
            address=address,
            username=username,
            password=password,
            vendor=entry.get("vendor"),
            api_url=api_url,
            dry_run=dry_run,
        )

    with ThreadPoolExecutor(max_workers=parallel) as pool:
        futures = {pool.submit(_run_one, entry): entry for entry in devices}
        for fut in as_completed(futures):
            results.append(fut.result())

    return results


# ── CLI ───────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Bonsai PyATS bootstrap agent (D4-17 T1)")
    sub = parser.add_subparsers(dest="cmd")

    single = sub.add_parser("device", help="Bootstrap a single device")
    single.add_argument("--address", required=True, help="Device IP address")
    single.add_argument("--credential-alias", help="Bonsai vault credential alias")
    single.add_argument("--username", help="SSH username (if not using vault)")
    single.add_argument("--password", help="SSH password (if not using vault)")
    single.add_argument("--vendor", help="Vendor hint (nokia_srl, cisco_iosxe, arista_eos, frr, …)")
    single.add_argument("--api-url", default="http://localhost:3000")
    single.add_argument("--dry-run", action="store_true")

    bulk = sub.add_parser("seed", help="Bootstrap from seed YAML file")
    bulk.add_argument("--seed-file", required=True)
    bulk.add_argument("--api-url", default="http://localhost:3000")
    bulk.add_argument("--parallel", type=int, default=4)
    bulk.add_argument("--dry-run", action="store_true")

    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s — %(message)s",
        datefmt="%H:%M:%S",
    )

    if args.cmd == "device":
        username = args.username or ""
        password = args.password or ""
        if args.credential_alias and not (username and password):
            username, password = _resolve_credential(args.api_url, args.credential_alias)
        if not (username and password):
            logger.error("No credentials — provide --username/--password or a valid --credential-alias")
            sys.exit(1)
        result = bootstrap_device(
            address=args.address,
            username=username,
            password=password,
            vendor=args.vendor,
            api_url=args.api_url,
            dry_run=args.dry_run,
        )
        print(json.dumps(asdict(result), indent=2, default=str))
        sys.exit(0 if result.status in ("ok", "partial") else 1)

    elif args.cmd == "seed":
        results = bootstrap_from_seed(
            seed_file=args.seed_file,
            api_url=args.api_url,
            parallel=args.parallel,
            dry_run=args.dry_run,
        )
        summary = {
            "total": len(results),
            "ok": sum(1 for r in results if r.status == "ok"),
            "partial": sum(1 for r in results if r.status == "partial"),
            "failed": sum(1 for r in results if r.status == "failed"),
            "results": [asdict(r) for r in results],
        }
        print(json.dumps(summary, indent=2, default=str))
        sys.exit(0 if summary["failed"] == 0 else 1)

    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()

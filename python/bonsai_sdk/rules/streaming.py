"""Modern streaming-protocol anomaly detection rules."""
from __future__ import annotations

import ipaddress
import json
import time
from typing import TYPE_CHECKING, Optional

from ..detection import Detector, Features
from ..window import WindowRegistry

if TYPE_CHECKING:
    from ..client import BonsaiClient

_ROUTE_FLAP_REGISTRY = WindowRegistry(window_seconds=300)
_ROUTE_FLAP_THRESHOLD = 3
_SRLG_RISK_COOLDOWN_SECS = 300
_ACTIVE_SR_POLICY_STATES = {"up", "active", "installed", "ready"}


def _parse_detail(event) -> dict:
    try:
        return json.loads(event.detail_json or "{}")
    except (json.JSONDecodeError, AttributeError):
        return {}


def _global_prefix(prefix: str) -> bool:
    try:
        network = ipaddress.ip_network(prefix, strict=False)
    except ValueError:
        return False
    return network.is_global


def _private_asn(asn: int) -> bool:
    return 64512 <= asn <= 65534 or 4200000000 <= asn <= 4294967294


def _route_entries(detail: dict) -> list[dict]:
    entries = detail.get("route_entries", [])
    return entries if isinstance(entries, list) else []


class RouteFlapDetected(Detector):
    rule_id = "route_flap_detected"
    severity = "warn"
    recurrence_indicators = [
        "Count route_flap_detected DetectionEvents for this device/peer/prefix in last 1h",
        "Check BgpNeighbor session state for the flapping peer: MATCH (n:BgpNeighbor {peer_address: $peer}) RETURN n.session_state",
        "Check for route_leak_detected co-firing on same device within same time window",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bmp_route_change":
            return None
        detail = _parse_detail(event)
        peer = detail.get("peer_address", "")
        for route in _route_entries(detail):
            prefix = route.get("prefix", "")
            prefix_len = route.get("prefix_len", "")
            action = route.get("action", "")
            if not prefix or not action:
                continue
            key = f"{event.device_address}:{peer}:{prefix}/{prefix_len}:{action}"
            window = _ROUTE_FLAP_REGISTRY.get(key)
            window.record(event.occurred_at_ns, action)
            count = window.count()
            if count < _ROUTE_FLAP_THRESHOLD:
                continue
            detail["trigger_prefix"] = f"{prefix}/{prefix_len}"
            detail["trigger_action"] = action
            detail["recent_flap_count"] = count
            return Features.from_event(event, detail)
        return None

    def detect(self, features: Features) -> Optional[str]:
        count = features.detail.get("recent_flap_count", 0)
        if count >= _ROUTE_FLAP_THRESHOLD:
            return (
                f"Route {features.detail.get('trigger_prefix', 'unknown')} on "
                f"{features.device_address} changed {count} times in 5 minutes via BMP"
            )
        return None


class UnexpectedAsPath(Detector):
    rule_id = "unexpected_as_path"
    severity = "warn"
    recurrence_indicators = [
        "Compare AS path in features_json against historical BMP RouteMonitoring entries for same prefix",
        "Check bgp_session_down/flap co-fires for the peer announcing this route",
        "Review config-history for BGP policy changes: GET /api/devices/{address}/config-history",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bmp_route_change":
            return None
        detail = _parse_detail(event)
        for route in _route_entries(detail):
            as_path = route.get("as_path", [])
            if not isinstance(as_path, list) or len(as_path) < 2:
                continue
            if len(as_path) != len(set(as_path)):
                detail["trigger_route"] = route
                return Features.from_event(event, detail)
        return None

    def detect(self, features: Features) -> Optional[str]:
        route = features.detail.get("trigger_route", {})
        prefix = route.get("prefix", "unknown")
        prefix_len = route.get("prefix_len", "")
        as_path = route.get("as_path", [])
        return (
            f"Unexpected AS path for {prefix}/{prefix_len} on {features.device_address}: "
            f"repeated ASN sequence {as_path}"
        )


class RouteLeakDetected(Detector):
    rule_id = "route_leak_detected"
    severity = "critical"
    recurrence_indicators = [
        "Count route_leak_detected DetectionEvents for this device in last 24h — persistence indicates misconfigured BGP policy",
        "Check if same private ASN appears in other leaked routes on this device within this detection window",
        "Verify AS path against known legitimate paths via BGP-LS topology: MATCH (l:BgpLsLink {device_address: $dev}) RETURN l",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "bmp_route_change":
            return None
        detail = _parse_detail(event)
        for route in _route_entries(detail):
            if route.get("action") != "announce":
                continue
            prefix = route.get("prefix", "")
            prefix_len = route.get("prefix_len", "")
            full_prefix = f"{prefix}/{prefix_len}"
            as_path = route.get("as_path", [])
            if not _global_prefix(full_prefix) or len(as_path) < 3:
                continue
            middle = as_path[1:-1]
            if any(_private_asn(int(asn)) for asn in middle):
                detail["trigger_route"] = route
                return Features.from_event(event, detail)
        return None

    def detect(self, features: Features) -> Optional[str]:
        route = features.detail.get("trigger_route", {})
        prefix = route.get("prefix", "unknown")
        prefix_len = route.get("prefix_len", "")
        as_path = route.get("as_path", [])
        return (
            f"Possible route leak for {prefix}/{prefix_len} on {features.device_address}: "
            f"private ASN observed inside propagated AS_PATH {as_path}"
        )


class SrPolicyDegraded(Detector):
    rule_id = "sr_policy_degraded"
    severity = "warn"
    recurrence_indicators = [
        "MATCH (p:SrPolicy {device_address: $dev, name: $name}) RETURN p.status — expect active/up when healthy",
        "Check BgpLsLink state for links along this SR policy's candidate paths",
        "Check IS-IS/OSPF adjacency state for intermediate nodes via DetectionEvent history",
    ]

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        if event.event_type != "sr_policy_change":
            return None
        detail = _parse_detail(event)
        new_status = str(detail.get("status", "")).lower()
        if new_status in _ACTIVE_SR_POLICY_STATES:
            return None
        return Features.from_event(event, detail)

    def detect(self, features: Features) -> Optional[str]:
        name = features.detail.get("name", "unknown-policy")
        endpoint = features.detail.get("endpoint", "unknown-endpoint")
        status = features.detail.get("status", "unknown")
        return (
            f"SR policy {name} toward {endpoint} on {features.device_address} "
            f"is no longer active (status={status})"
        )


class SrlgRiskDetected(Detector):
    rule_id = "srlg_risk_detected"
    severity = "warn"
    recurrence_indicators = [
        "MATCH (l:BgpLsLink) WHERE l.srlgs_json CONTAINS $srlg RETURN l.local_router_id, l.remote_router_id — enumerate all links sharing this SRLG",
        "Check for interface_down on any link in this SRLG within the last detection window",
        "Review topology for diversity: are there paths not sharing this SRLG? GET /api/path?src=...&dst=...",
    ]

    _last_fired: dict[str, float] = {}

    def extract_features(self, event, client: "BonsaiClient") -> Optional[Features]:
        return None

    def detect(self, features: Features) -> Optional[str]:
        return None

    @classmethod
    def evaluate_graph(cls, client: "BonsaiClient") -> list[tuple[str, str]]:
        rows = client.query(
            "MATCH (l:BgpLsLink) "
            "RETURN l.device_address, l.local_router_id, l.remote_router_id, l.srlgs_json"
        )
        srlg_index: dict[tuple[str, int], list[str]] = {}
        for row in rows:
            device = row[0] if len(row) > 0 else ""
            local = row[1] if len(row) > 1 else ""
            remote = row[2] if len(row) > 2 else ""
            raw_srlgs = row[3] if len(row) > 3 else "[]"
            try:
                srlgs = json.loads(raw_srlgs or "[]")
            except json.JSONDecodeError:
                srlgs = []
            if not device or not isinstance(srlgs, list):
                continue
            link_name = f"{local}->{remote}"
            for srlg in srlgs:
                try:
                    key = (device, int(srlg))
                except (TypeError, ValueError):
                    continue
                srlg_index.setdefault(key, []).append(link_name)

        results: list[tuple[str, str]] = []
        now = time.time()
        for (device, srlg), links in srlg_index.items():
            unique_links = sorted(set(links))
            if len(unique_links) < 2:
                continue
            dedup_key = f"{device}:{srlg}:{','.join(unique_links)}"
            last = cls._last_fired.get(dedup_key, 0.0)
            if now - last < _SRLG_RISK_COOLDOWN_SECS:
                continue
            cls._last_fired[dedup_key] = now
            results.append(
                (
                    device,
                    f"SRLG {srlg} is shared by multiple BGP-LS links on {device}: "
                    f"{', '.join(unique_links)}",
                )
            )
        return results


STREAMING_RULES: list[Detector] = [
    RouteFlapDetected(),
    UnexpectedAsPath(),
    RouteLeakDetected(),
    SrPolicyDegraded(),
    SrlgRiskDetected(),
]

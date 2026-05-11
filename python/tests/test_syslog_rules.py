from __future__ import annotations

import json
import time
from types import SimpleNamespace
from unittest.mock import MagicMock

from bonsai_sdk.rules.syslog import (
    MultiSourceCorrelation,
    OrphanInterfaceMention,
    SyslogAuthFailureCluster,
    SyslogBpduGuardActivation,
    SyslogGnmiDisagreement,
    SyslogHardwareError,
    SyslogLicenseExpiry,
    SyslogProtocolError,
    SyslogStpTopologyChange,
    SyslogSoftwareCrash,
)


def _event(device: str, event_type: str, message: str, ts: int) -> SimpleNamespace:
    return SimpleNamespace(
        device_address=device,
        event_type=event_type,
        detail_json=json.dumps({"message": message, "category": event_type.removeprefix("syslog_")}),
        occurred_at_ns=ts,
        state_change_event_id=f"{device}-{ts}",
    )


def _fact_event(device: str, event_type: str, detail: dict, ts: int) -> SimpleNamespace:
    return SimpleNamespace(
        device_address=device,
        event_type=event_type,
        detail_json=json.dumps(detail),
        occurred_at_ns=ts,
        state_change_event_id=f"{device}-{ts}",
    )


def test_syslog_auth_failure_cluster_fires_on_third_event():
    client = MagicMock()
    rule = SyslogAuthFailureCluster()
    base_ts = time.time_ns()

    assert rule.extract_features(_event("leaf-auth-a", "syslog_auth", "Failed password for admin", base_ts), client) is None
    assert rule.extract_features(_event("leaf-auth-a", "syslog_auth", "Failed password for admin", base_ts + 1), client) is None
    features = rule.extract_features(_event("leaf-auth-a", "syslog_auth", "Failed password for admin", base_ts + 2), client)

    assert features is not None
    assert features.recent_flap_count >= 3
    assert "auth failures reached" in rule.detect(features)


def test_syslog_hardware_error_matches_failure_keywords():
    client = MagicMock()
    rule = SyslogHardwareError()
    features = rule.extract_features(
        _event("leaf-hw-a", "syslog_hardware", "PSU failure alarm asserted", 100),
        client,
    )
    assert features is not None
    assert "PSU failure" in rule.detect(features)


def test_syslog_software_crash_matches_restart_keywords():
    client = MagicMock()
    rule = SyslogSoftwareCrash()
    features = rule.extract_features(
        _event("leaf-sw-a", "syslog_software", "routing process restarted after crash", 100),
        client,
    )
    assert features is not None
    assert "Software crash" in rule.detect(features)


def test_syslog_license_expiry_matches_warning_keywords():
    client = MagicMock()
    rule = SyslogLicenseExpiry()
    features = rule.extract_features(
        _event("leaf-lic-a", "syslog_license", "license will expire in 7 days", 100),
        client,
    )
    assert features is not None
    assert "License warning" in rule.detect(features)


def test_syslog_protocol_error_matches_down_keywords():
    client = MagicMock()
    rule = SyslogProtocolError()
    features = rule.extract_features(
        _event("leaf-proto-a", "syslog_protocol", "BFD session down on ethernet-1/1", 100),
        client,
    )
    assert features is not None
    assert "Protocol syslog" in rule.detect(features)


def test_syslog_bpduguard_activation_matches_keyword():
    client = MagicMock()
    rule = SyslogBpduGuardActivation()
    features = rule.extract_features(
        _event("leaf-stp-a", "syslog_protocol", "STP BPDUGuard placed ethernet-1/1 into errdisable", 100),
        client,
    )
    assert features is not None
    assert "BPDUGuard syslog" in rule.detect(features)


def test_syslog_stp_topology_change_matches_keyword():
    client = MagicMock()
    rule = SyslogStpTopologyChange()
    features = rule.extract_features(
        _event("leaf-stp-b", "syslog_protocol", "spanning-tree topology change detected on vlan 10", 100),
        client,
    )
    assert features is not None
    assert "topology change" in rule.detect(features).lower()


def test_syslog_gnmi_disagreement_fires_for_bgp_mismatch():
    client = MagicMock()
    rule = SyslogGnmiDisagreement()
    features = rule.extract_features(
        _fact_event(
            "leaf-bgp-a",
            "syslog_fact_joined",
            {
                "fact_type": "bgp_neighbor",
                "message": "BGP neighbor 10.1.0.1 down",
                "fields": {"peer_address": "10.1.0.1", "new_state": "down"},
                "join": {
                    "status": "joined",
                    "kind": "bgp_neighbor",
                    "graph_state": {"peer_address": "10.1.0.1", "session_state": "established"},
                },
            },
            100,
        ),
        client,
    )
    assert features is not None
    assert "disagree on BGP state" in rule.detect(features)


def test_orphan_interface_mention_fires_for_unresolved_interface_fact():
    client = MagicMock()
    rule = OrphanInterfaceMention()
    features = rule.extract_features(
        _fact_event(
            "leaf-if-a",
            "syslog_fact_orphan",
            {
                "fact_type": "interface_state",
                "message": "Interface ethernet-1/99 changed state to down",
                "fields": {"if_name": "ethernet-1/99", "new_state": "down"},
                "join": {"status": "orphan", "kind": "interface", "reason": "no_interface_match"},
            },
            100,
        ),
        client,
    )
    assert features is not None
    assert "unmanaged or unresolved interface ethernet-1/99" in rule.detect(features)


def test_multi_source_correlation_fires_for_joined_interface_fact():
    client = MagicMock()
    rule = MultiSourceCorrelation()
    features = rule.extract_features(
        _fact_event(
            "leaf-if-b",
            "syslog_fact_joined",
            {
                "fact_type": "interface_state",
                "message": "Interface ethernet-1/1 changed state to down",
                "fields": {"if_name": "ethernet-1/1", "new_state": "down"},
                "join": {
                    "status": "joined",
                    "kind": "interface",
                    "graph_state": {"if_name": "ethernet-1/1", "in_errors": 3, "out_errors": 1},
                },
            },
            100,
        ),
        client,
    )
    assert features is not None
    assert "resolved syslog interface mention ethernet-1/1" in rule.detect(features)

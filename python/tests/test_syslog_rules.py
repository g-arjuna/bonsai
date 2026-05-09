from __future__ import annotations

import json
import time
from types import SimpleNamespace
from unittest.mock import MagicMock

from bonsai_sdk.rules.syslog import (
    SyslogAuthFailureCluster,
    SyslogBpduGuardActivation,
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

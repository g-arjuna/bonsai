from __future__ import annotations

import json
import time
from types import SimpleNamespace
from unittest.mock import MagicMock

from bonsai_sdk.rules.snmp import (
    SnmpAuthFailureBurst,
    SnmpColdWarmStart,
    SnmpEnvironmentalThresholdBreach,
    SnmpFruFailure,
)


def _event(device: str, event_type: str, message: str, ts: int) -> SimpleNamespace:
    return SimpleNamespace(
        device_address=device,
        event_type=event_type,
        detail_json=json.dumps({"message": message, "trap_oid": "1.3.6.1.6.3.1.1.5.5"}),
        occurred_at_ns=ts,
        state_change_event_id=f"{device}-{ts}",
    )


def test_snmp_cold_warm_start_matches_both_startup_events():
    client = MagicMock()
    rule = SnmpColdWarmStart()

    cold = rule.extract_features(_event("leaf-a", "snmp_cold_start", "device coldStart trap received", 1), client)
    warm = rule.extract_features(_event("leaf-a", "snmp_warm_start", "device warmStart trap received", 2), client)

    assert cold is not None
    assert warm is not None
    assert "cold start" in rule.detect(cold)
    assert "warm start" in rule.detect(warm)


def test_snmp_auth_failure_burst_fires_on_third_trap():
    client = MagicMock()
    rule = SnmpAuthFailureBurst()
    base_ts = time.time_ns()

    assert rule.extract_features(_event("leaf-auth-a", "snmp_auth_failure", "authenticationFailure trap received", base_ts), client) is None
    assert rule.extract_features(_event("leaf-auth-a", "snmp_auth_failure", "authenticationFailure trap received", base_ts + 1), client) is None
    features = rule.extract_features(_event("leaf-auth-a", "snmp_auth_failure", "authenticationFailure trap received", base_ts + 2), client)

    assert features is not None
    assert features.recent_flap_count >= 3
    assert "authentication failures reached" in rule.detect(features)


def test_snmp_environmental_rule_matches_psu_keywords():
    client = MagicMock()
    rule = SnmpEnvironmentalThresholdBreach()
    features = rule.extract_features(
        _event("leaf-env-a", "snmp_environmental", "PSU failure alarm asserted", 100),
        client,
    )
    assert features is not None
    assert "Environmental SNMP trap" in rule.detect(features)


def test_snmp_fru_rule_matches_module_keywords():
    client = MagicMock()
    rule = SnmpFruFailure()
    features = rule.extract_features(
        _event("leaf-fru-a", "snmp_fru_failure", "linecard module failure detected", 100),
        client,
    )
    assert features is not None
    assert "FRU SNMP trap" in rule.detect(features)

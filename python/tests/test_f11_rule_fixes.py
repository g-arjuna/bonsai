"""Regression tests for F-11: bfd_session_down and interface_down rules not firing.

Root causes fixed:
  BFD  — BfdSessionDown rejected events where old_state="none" (bootstrap case:
          bonsai started after the session was already down).
  Iface — emit_oper_status_event was not calling write_state_change_event, so
          interface_oper_status_change events had no state_change_event_id.
          Also: SRL subinterface oper-state path was not classified.

These tests exercise the Python rule logic in isolation (no gRPC needed).
"""
from __future__ import annotations

import json
import types
import unittest


def _make_event(event_type: str, device_address: str, detail: dict, occurred_at_ns: int = 0):
    e = types.SimpleNamespace()
    e.event_type = event_type
    e.device_address = device_address
    e.detail_json = json.dumps(detail)
    e.occurred_at_ns = occurred_at_ns
    e.state_change_event_id = ""
    return e


class _FakeClient:
    def get_bgp_neighbors(self, _addr):
        return []


_CLIENT = _FakeClient()


class TestBfdSessionDownBootstrap(unittest.TestCase):
    """BFD rule must fire when old_state is 'none' (bootstrap) and new_state is 'down' or 'admin_down'."""

    def setUp(self):
        from bonsai_sdk.rules.bfd import BfdSessionDown
        self.rule = BfdSessionDown()

    def test_up_to_down_fires(self):
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "up",
            "new_state": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "up→down must produce features")
        reason = self.rule.detect(f)
        self.assertIsNotNone(reason)
        self.assertIn("down", reason)

    def test_none_to_down_fires(self):
        """Bootstrap case: bonsai just started, session arrives already-down."""
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "none",
            "new_state": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "none→down must produce features (bootstrap case)")
        reason = self.rule.detect(f)
        self.assertIsNotNone(reason)

    def test_up_to_admin_down_fires(self):
        """SR Linux BFD admin-disable transitions to admin_down, not down."""
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "up",
            "new_state": "admin_down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "up→admin_down must produce features")
        reason = self.rule.detect(f)
        self.assertIsNotNone(reason)
        self.assertIn("admin_down", reason)

    def test_none_to_admin_down_fires(self):
        """Bootstrap: session observed for first time already admin-disabled."""
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "none",
            "new_state": "admin_down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "none→admin_down must produce features")
        reason = self.rule.detect(f)
        self.assertIsNotNone(reason)

    def test_down_to_up_does_not_fire(self):
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "down",
            "new_state": "up",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNone(f, "down→up must not produce features")

    def test_wrong_event_type_ignored(self):
        ev = _make_event("bgp_session_change", "10.0.0.1", {
            "old_state": "none", "new_state": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNone(f)

    def test_none_to_up_does_not_fire(self):
        """Session observed for first time as up — not an anomaly."""
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "if_name": "ethernet-1/1.0",
            "peer": "10.0.0.2",
            "local_address": "10.0.0.1",
            "local_discriminator": "1",
            "old_state": "none",
            "new_state": "up",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNone(f, "none→up must not fire (not anomalous)")


class TestInterfaceDownRule(unittest.TestCase):
    """interface_down rule must fire for interface_oper_status_change events with oper_status=down."""

    def setUp(self):
        from bonsai_sdk.rules.interface import InterfaceDown
        self.rule = InterfaceDown()

    def test_oper_status_down_fires(self):
        ev = _make_event("interface_oper_status_change", "10.0.0.1", {
            "if_name": "ethernet-1/1",
            "old_state": "up",
            "new_state": "down",
            "oper_status": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "oper_status=down must produce features")
        reason = self.rule.detect(f)
        self.assertIsNotNone(reason)
        self.assertIn("ethernet-1/1", reason)

    def test_lower_layer_down_fires(self):
        ev = _make_event("interface_oper_status_change", "10.0.0.1", {
            "if_name": "ethernet-1/2",
            "old_state": "up",
            "new_state": "lower-layer-down",
            "oper_status": "lower-layer-down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f)

    def test_oper_status_up_does_not_fire(self):
        ev = _make_event("interface_oper_status_change", "10.0.0.1", {
            "if_name": "ethernet-1/1",
            "old_state": "down",
            "new_state": "up",
            "oper_status": "up",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNone(f)

    def test_wrong_event_type_ignored(self):
        ev = _make_event("bfd_session_change", "10.0.0.1", {
            "oper_status": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNone(f)

    def test_new_state_key_fallback(self):
        """extract_features_for_event falls back to new_state when oper_status key absent."""
        ev = _make_event("interface_oper_status_change", "10.0.0.1", {
            "if_name": "ethernet-1/3",
            "old_state": "up",
            "new_state": "down",
        })
        f = self.rule.extract_features(ev, _CLIENT)
        self.assertIsNotNone(f, "new_state=down fallback must fire")


if __name__ == "__main__":
    unittest.main()

import importlib.util
import pathlib
import sys
import unittest


def _load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, str(path))
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


BONSAI_ROOT = pathlib.Path(__file__).resolve().parents[1]
RUSTYBMP_ROOT = pathlib.Path("/home/arjuna/rustybmp-b01")


class TestB07VendorRegistry(unittest.TestCase):
    def test_bootstrap_agent_rejects_unknown_vendor_without_cisco_fallback(self):
        bootstrap = _load_module("bootstrap_agent", BONSAI_ROOT / "python" / "bootstrap_agent.py")
        self.assertIsNone(bootstrap.normalize_bootstrap_vendor("unknown"))
        self.assertIsNone(bootstrap.normalize_bootstrap_vendor("vyos"))
        self.assertIsNone(bootstrap.normalize_bootstrap_vendor(""))
        self.assertEqual(bootstrap.normalize_bootstrap_vendor("nokia_srl"), "nokia.srlinux")
        self.assertEqual(bootstrap.normalize_bootstrap_vendor("cisco_xrd"), "cisco.iosxr")
        self.assertEqual(bootstrap.normalize_bootstrap_vendor("arista_ceos"), "arista.eos")
        self.assertEqual(bootstrap.normalize_bootstrap_vendor("juniper_crpd"), "juniper.junos")

        res = bootstrap.bootstrap_device(
            address="edge-a",
            username="admin",
            password="secret",
            vendor="unknown",
            dry_run=True,
        )
        self.assertEqual(res.status, "failed")
        self.assertEqual(res.vendor, "unknown")
        self.assertIn("refusing Cisco IOS-XE fallback", res.error)

    def test_bootstrap_agent_handles_ipv6_and_neutral_hostname_endpoints(self):
        bootstrap = _load_module("bootstrap_agent", BONSAI_ROOT / "python" / "bootstrap_agent.py")
        self.assertEqual(bootstrap._strip_port("10.0.0.1:57400"), "10.0.0.1")
        self.assertEqual(bootstrap._strip_port("[2001:db8::1]:57400"), "2001:db8::1")
        self.assertEqual(bootstrap._strip_port("2001:db8::1"), "2001:db8::1")
        self.assertEqual(bootstrap._strip_port("edge-a:57400"), "edge-a")
        self.assertEqual(bootstrap._strip_port("edge-a"), "edge-a")

        self.assertEqual(bootstrap._ensure_gnmi_address("2001:db8::1", 57400), "[2001:db8::1]:57400")
        self.assertEqual(bootstrap._ensure_gnmi_address("[2001:db8::1]:57400", 57400), "[2001:db8::1]:57400")
        self.assertEqual(bootstrap._ensure_gnmi_address("edge-a", 57400), "edge-a:57400")

    def test_policy_fetcher_rejects_unknown_vendor_and_parses_ipv6_and_hostname(self):
        pf = _load_module("policy_fetcher", RUSTYBMP_ROOT / "bmppy" / "policy_fetcher.py")
        with self.assertRaisesRegex(ValueError, "no default/Cisco fallback is permitted"):
            pf.normalize_vendor("unknown")
        with self.assertRaisesRegex(ValueError, "no default/Cisco fallback is permitted"):
            pf._policy_commands("unknown-nos", "RM-IN", "10.0.0.2", "in")

        self.assertEqual(pf.normalize_vendor("nokia_srl"), "nokia.srlinux")
        self.assertEqual(pf.normalize_vendor("cisco-iosxr"), "cisco.iosxr")
        self.assertEqual(pf.extract_ssh_host("[2001:db8::10]:179"), "2001:db8::10")
        self.assertEqual(pf.extract_ssh_host("2001:db8::10"), "2001:db8::10")
        self.assertEqual(pf.extract_ssh_host("edge-a:57400"), "edge-a")
        self.assertEqual(pf.extract_ssh_host("edge-a"), "edge-a")

        with self.assertRaisesRegex(ValueError, "Invalid policy parameter"):
            pf._policy_commands("frr", "RM-IN; rm -rf /", "10.0.0.2", "in")


if __name__ == "__main__":
    unittest.main()

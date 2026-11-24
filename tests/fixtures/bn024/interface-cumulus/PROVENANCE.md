# Fixture provenance — Cumulus/SONiC interface observation pair

Source: SuzieQ OSS `tests/integration/texttfsm/input/cl_interfaces.txt`
@ pinned revision `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`
(https://github.com/netenglabs/suzieq, Apache-2.0).

Upstream file sha256:
`f1384d1b2a569b84e5b6db95bf0c07e479679a0b58c9209de7c4d0292d6ca28e`

The upstream file concatenates the two command outputs that
`suzieq/config/interfaces.yml` declares for `cumulus`/`sonic`
(`sudo vtysh -c "show int vrf all"; ip -d address show`). This fixture
splits it at the first `ip -d address` stanza boundary (line 161):

- `vtysh-show-int-vrf-all.txt` — lines 1–160 (vtysh output)
- `ip-d-address-show.txt` — lines 161–297 (iproute2 output)

Content is a genuine captured device observation (Cumulus Linux,
timestamps 2018/10/16) redistributed under Apache-2.0 as upstream test
data; per `docs/bn024/provenance/upstream-sources.json` the
`test_fixture_redistribution` class applies. License evidence:
upstream `LICENSE` (Apache-2.0); required notice: retain this file and
the upstream copyright in derivative test fixtures.

Device release is unknown for this capture; upstream declares
`version: all` for the interfaces feature, so applicability is
unaffected — the normalizer records `release_raw: None` accordingly.

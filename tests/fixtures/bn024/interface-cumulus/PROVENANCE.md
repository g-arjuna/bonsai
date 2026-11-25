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
`test_fixture_redistribution` class applies.

## License and attribution

The upstream Apache License 2.0 text is vendored verbatim in this
directory as `LICENSE.apache-2.0` (sha256
`c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`,
identical to upstream `LICENSE` @ the pinned revision; no upstream
NOTICE file exists at that revision). Repository-level attribution is
recorded in the top-level `NOTICE`. The fixture was scanned for
credentials and other sensitive material; none is present.

## Modified files

The two `.txt` files are MODIFIED derivatives of the upstream file:
the only change is the split into two command outputs (verbatim line
ranges, no content edits). Derived file hashes:

- `vtysh-show-int-vrf-all.txt` sha256
  `c60671aee1e0bba7d229821dbc401d7ee4e5b25a2c41fa4b5f137cd3334b1172`
  (upstream lines 1–160)
- `ip-d-address-show.txt` sha256
  `0fad3b5fedeb8e97705fa4ec51aebbf514ddc683ae25cc97f199a19609226277`
  (upstream lines 161–297)

`LICENSE.apache-2.0` is UNMODIFIED upstream content.

Device release is unknown for this capture; upstream declares
`version: all` for the interfaces feature, so applicability is
unaffected — the normalizer records `release_raw: None` accordingly.

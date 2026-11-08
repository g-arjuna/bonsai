# BN-006 bounded live acceptance

**Result:** complete for the bounded support profile below. Evidence was collected
7 November 2026 in dedicated containerlab `bonsai-bn006-live-20261107`. It used
cEOS 4.36.0F and SR Linux 26.3.1, with Bonsai and the actual NOS speakers on the
same isolated lab network. Container image IDs, captures, journals, command logs,
and test outputs are stored beside this report.

## Tested support matrix

| Platform | Tested collector path | Instance / family | Demonstrated behavior | Evidence boundary |
|---|---|---|---|---|
| Arista cEOS 4.36.0F | BMP Loc-RIB and post-policy Adj-RIB-Out; Bonsai routing receiver | default and BLUE (RD `65001:100`); IPv4 unicast | lifecycle, MED/policy change, withdraw/reannounce, EOF gap/reconnect, and software-FIB forward/withdraw probes | virtual cEOS only; no physical ASIC claim; no pre-policy feed tested |
| Nokia SR Linux 26.3.1 | Native read-only SSH CLI collection through operator API and MCP | default instance; IPv4 BGP peer | authenticated collection, ASN and established state, exporter provenance | operator-triggered; no periodic scheduler; no BMP lifecycle claim for this NOS |

## Acceptance demonstrated

- EOS sent BMP Loc-RIB and post-policy Adj-RIB-Out updates for IPv4 unicast in
default and BLUE (RD `65001:100`). Bonsai preserved distinct source, instance,
view and epoch identity. The repeated lifecycle gate verified route arrival,
views/RD isolation, an actual wire MED change affecting configured import policy,
actual withdrawal and reannouncement, and real TCP EOF on EOS process kill with
an explicit gap and no synthetic withdrawals. Recreating only this task lab
produced a later epoch and fresh routes.
- An offline startup-config change changed the route's import-policy result
(local preference 250 to 300) while MED remained 150; BLUE remained separate.
The policy was staged while EOS was stopped; no live router configuration write
was made.
- Independent raw Ethernet/IPv4 probes verified cEOS software-FIB forwarding
and negative forwarding after withdrawing the selected route, in default and
BLUE. Probes checked returned frame MACs, decremented TTL and payload identity.
These are virtual cEOS forwarding results, not physical ASIC or hardware results.
- A live read-only SR Linux 26.3.1 SSH collection entered the operator API and
MCP path. The test checked authorization, the collected peer ASN and established
state, and collector provenance.
- A focused replay regression preserves the EOS reconnect/EOR retirement fix;
see [the reconnect investigation](../../BN006_RECONNECT_INVESTIGATION_2026-11-07.md)
and the [reconnect evidence directory](../2026-11-07-bn006-reconnect/).

## Scope and limitations

This closes BN-006 for the tested IPv4 profile: cEOS 4.36.0F BMP lifecycle and
software FIB, plus SR Linux 26.3.1 native peer collection. The native collection
is initiated by an operator request and then runs through the CLI/API/MCP flow;
there is no periodic scheduler. No pre-policy EOS BMP feed, IPv6, hardware
forwarding, additional releases/vendors, production device, or broad scale/soak
claim is made. BMP gap signaling is distinct from withdrawals. The optional
legacy DuckDB writer still does not materialize retired route identities and
remains BN-007 work. These boundaries are part of the support profile, not
unverified general claims.

## Evidence map

- `second-run/` and `second-run.log`: full two-run BMP lifecycle acceptance and
  genuine BMP frames.
- `policy-final/` and `policy-final.log`: offline startup-policy change.
- `fib-default-current/`, `fib-blue-current/` and matching logs: forwarding and
  withdrawal probes for both instances.
- `operator-native-final.json` and `native-live.log`: live SR Linux collection
  and operator/MCP result.
- `bmp-journal.jsonl`, `event-journal.jsonl`: durable journals from the isolated
  live receiver. They include only test-lab routing observations.
- `container-identities.txt`, `eos-version.json`, `srl-version.json`, baseline
  and candidate configs: environment and policy provenance. The SR Linux SSH
  private key and known-hosts file are intentionally not included.
- `focused.log` and `imported.log`: final Rust focused and imported crate gates.
- `results.json`: source/evidence hashes and command summary (written after all
  gates and cleanup complete).

The broader repository contains user changes and unrelated uncommitted files;
this evidence identifies the tested base commit and scoped source hashes rather
than treating the entire working tree as a clean commit.

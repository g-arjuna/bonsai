# BN-003 genuine VRF export — lab bring-up progress — 8 November 2026

Base commit `8017090` plus uncommitted working-tree changes. Status: **Partial — lab deployed, genuine VRF BMP export not yet proven.**

## Environment work completed this session

- Removed stale containerlab containers: `clab-bonsai-srl-srl1/2/3` (exited ~5
  months), `clab-bonsai-bn006-live-65f2f2-{eos,srl}` (exited 8 days) and the
  orphaned `-source` BGP speaker container.
- Restarted NetBox stack (`docker/compose-netbox.yml`): all 4 containers
  healthy; API token auth verified, 8 seeded devices present (volume
  persisted, no re-seed needed). Splunk/Elasticsearch/Kibana/Grafana/
  Prometheus confirmed healthy on their compose-external ports.

## BN-003 VRF lab (new, task-dedicated)

- Topology: `lab/fast-iteration/frr-bmp-vrf.clab.yml` — lab name
  `bonsai-bn003-vrf`, mgmt `172.100.208.0/24`, FRR 10.6.1 nodes frr1/frr2,
  two links: eth1 global BGP session (AS 65001↔65002) and eth2 VRF BLUE
  session. frr1 has `vrf BLUE` with `rd 65000:100` and originates
  `10.99.1.0/24` in the VRF.
- Configs: `lab/fast-iteration/configs-bn003vrf/{frr1,frr2,daemons,vtysh}.conf`
- **Deployed and running** (`containerlab deploy` succeeded; left up for the
  next session — it is the task-dedicated lab).

## Key findings

1. FRR BMP targets are configured **per BGP instance** — `bmp targets` lives
   inside `router bgp`, not as a top-level `router bmp` node. For VRF export,
   a `bmp targets` block inside `router bgp <asn> vrf BLUE` is required; FRR
   10.x emits RD-instance peer headers (peer type + distinguisher) for VRF
   instance peers (FRRouting/frr#17555).
2. **Blocker**: zebra rejects `ip vrf forwarding BLUE` under `interface eth2`
   ("Unknown command", zebra daemon) on `quay.io/frrouting/frr:10.6.1` with
   `frr defaults datacenter`. VRF BLUE stays `inactive (configured)`, both VRF
   BGP sessions sit `Idle — Waiting for VRF to be initialized`. Kernel VRF
   creation itself works in the container (`ip link add BLUE type vrf table
   100` succeeded), so this is an FRR config/build issue, not container
   privileges.
3. Next debug steps: determine correct FRR 10.6 VRF interface syntax for this
   image (possibly datacenter-defaults interaction or a renamed command),
   then verify BMP peer-up/route-monitoring frames carry RD-instance headers
   with distinguisher 65000:100 before wiring into the acceptance test.

## Research findings (FRR docs, 8 Nov)

- BMP: all options live inside `router bgp ASN` per BGP instance — confirmed.
  `bmp targets` inside `router bgp <asn> vrf BLUE` is correct for RD-instance
  peer headers. Daemon flag `-M bgpd_bmp` correct.
- VRF: FRR 10.x zebra docs list `interface IFNAME vrf VRF` — interface-scoped
  VRF config replacing/augmenting `ip vrf forwarding`. FRR 10.x also has
  selectable VRF backends (kernel vs netns; global `-w/--vrfwnetns`). The
  `ip vrf forwarding` rejection is a syntax/backend mismatch, not container
  privileges (kernel `ip link add ... type vrf` works).
- Next attempts (cheap vtysh probes before redeploy):
  1. `interface eth2 vrf BLUE` scoped form, or `vrf BLUE` sub-command under
     `interface eth2`.
  2. Confirm `show vrf` → `active`, VRF BGP session Established, then verify
     BMP peer-up/route-monitoring frames carry RD-instance header +
     distinguisher 65000:100 (bytes `00 00 fd e8 00 00 00 64`).
- Fallback: `address-family vpnv4 unicast` + `bmp monitor vpnv4 unicast` on
  the global instance carries RD inside NLRI — first check whether
  rbmp-core/rbmp-rib extracts RD from VPNv4 NLRI (`crates/rbmp-core/src/bgp/nlri.rs`,
  `crates/rbmp-rib/src/manager.rs`).

## Not done

- No BMP capture yet (listener not started; no genuine RD frames observed).
- `tests/bn003_live_routing_browser_acceptance.rs` still injects synthetic
  `build_rd_bmp_frames` fixtures — unchanged.
- Deployed browser/graph restart journey remains open.

## Live probes (second pass, same day)

Lab `bonsai-bn003-vrf` still deployed; frr1/frr2 up. VRF remains
`inactive (configured)`; eth2 still `vrf: default`.

Probed on running frr1 via vtysh (no config file change, runtime only):

- `interface eth2 vrf BLUE` — accepted silently (no error) but had no effect:
  `show vrf` still inactive, `show interface eth2` still `vrf: default`.
- `interface eth2` → `vrf BLUE` — also no effect.
- Earlier: `ip vrf forwarding BLUE` → `% Unknown command` (zebra).

Interpretation: this FRR 10.6.1 image's zebra accepts neither binding form
into an active VRF — likely the VRF backend isn't instantiated at all
(possible missing `-w/vrfwnetns` daemon flag, datacenter-defaults
interaction, or image build without kernel-VRF backend). Next session:
check `zebra -b`/`vtysh` for backend options, try `frr defaults` removal or
the `-w` flag in `daemons`, and/or fall back to vpnv4 AF monitoring after
checking rbmp VPNv4 NLRI RD extraction.

Useful show commands for continuation:
- `show vrf`, `show interface eth2` (vrf field), `show bgp vrf BLUE summary`
- `show bgp summary` (global instance), `show bgp ipv4 unicast vrf BLUE`
- `show running-config` (verify bmp targets placement per instance)

## Root cause identified (third pass)

`docker exec -u frr ... ip link add TESTVRF type vrf table 101` →
`RTNETLINK answers: Operation not permitted`; same command as root succeeds.
Container is `Privileged: true` but `/proc/1/status` shows `CapInh: 0` — the
`frr` user that watchfrr drops zebra/bgpd/staticd to has **no capabilities**,
so zebra cannot create the kernel VRF device. `show vrf` →
`vrf BLUE inactive (configured)` is exactly this failure.

Note: `ip vrf forwarding` "Unknown command" errors in the log are from
non-zebra daemons parsing frr.conf — zebra owns that command; the real
blocker is the missing CAP_NET_ADMIN, not syntax.

### Fix options for next session (container has python3 + setpriv, no setcap)

1. File capabilities via python `os.setxattr` on `/usr/lib/frr/zebra`
   (security.capability v3 xattr, cap_net_admin+cap_net_raw, permitted+effective)
   then `docker restart clab-bonsai-bn003-vrf-frr1`.
2. Or run zebra with ambient caps: modify `daemons`/`zebra_options` or wrap
   the zebra exec so the frr user keeps `cap_net_admin` ambient.
3. Or simplest lab-only alternative: pre-create the kernel VRF as root
   (`ip link add BLUE type vrf table <id>`) — but zebra assigns its own
   table id, so verify `show vrf` adopts it before relying on this.

After VRF goes `active`: verify `show bgp vrf BLUE summary` Established,
then BMP capture on `172.100.208.1:11029` for RD-instance peer headers
(distinguisher `00 00 fd e8 00 00 00 64` = 65000:100).

## Capability fix attempts — results (fourth pass)

- **File caps on zebra**: `security.capability` v3 xattr set successfully via
  python `os.setxattr` (24-byte blob, magic 0x03000001, permitted=0x3000).
  xattr persists on disk BUT zebra still runs `CapEff=0` — watchfrr execs
  zebra as root and zebra self-`setuid()`s to `frr`, which clears
  permitted/effective caps. File caps cannot survive self-setuid. DEAD END
  (xattr still on the binary in the running container — harmless but should
  be removed or the container recreated before relying on clean state).
- **`zebra_options="-u root"`**: zebra exits —
  `privs_init: user(root) is not part of vty group specified(frrvty)`.
- **`zebra_options="-u root -g frrvty"`**: same rejection — the check is on
  the user, not just primary group. DEAD END unless a supplementary-group
  flag exists (FRR `-A`/`-G` variants worth one more probe).
- Ambient caps can't help: setuid root→frr clears pP/pE, and ambient only
  re-raises at execve (zebra doesn't re-exec).

### Remaining viable paths

1. One more privs probe: FRR common options may include a supplementary
   group flag (`-A`/`-G`) — `zebra_options="-u root -g frrvty -A frrvty"`
   or similar; if zebra stays root it has full caps → VRF works.
2. **Different image**: run the lab on an FRR image/build that grants the
   frr user cap_net_admin (e.g. check `quay.io/frrouting/frr` tags or the
   `frrouting/frr-vtysh`/`frouting/frr` variants; topotests run VRF in
   privileged docker where daemons keep caps).
3. **Alternative genuine-RD path**: `address-family vpnv4 unicast` +
   `bmp monitor vpnv4 unicast` on the global instance — RD travels inside
   VPNv4 NLRI; no kernel VRF needed at all. First check whether
   rbmp-core/rbmp-rib extracts RD from VPNv4 NLRI before committing.
4. Give the `frr` user the caps via container-level ambient set
   (`setpriv --ambient-caps` in an entrypoint wrapper) — requires image
   customization, heaviest option.

Lab state: `bonsai-bn003-vrf` running, daemons stock, global BGP session
Active, VRF still `inactive (configured)`.

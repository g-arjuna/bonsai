# Bonsai + RustyBMP: repository audit and Codex development handoff

Prepared 11 October 2026 for Arjuna Ganesan. Target: VS Code/Codex on the owner's Ubuntu machine with containerlab and locally available NOS images.

## 1. Decision and first instruction

**Proceed with Bonsai as the product, repository, user interface, inventory and authentication authority. Bring RustyBMP's routing components into that repository as an isolated Rust workspace component. Build one verified troubleshooting workflow before expanding the platform.**

The first useful outcome is: “Select a device, peer and prefix; see what changed, in which routing instance and BMP view, when it was observed, whether the observation is complete, and what supporting telemetry exists.” A network engineer should reproduce the answer from a lab action and captured evidence.

Do not implement this entire document in one Codex turn. Start with B00–B02, report the baseline, then work through one bounded task at a time. The large document is a reference; the next task and its acceptance criteria should fit in a short session prompt.

### How this changes the previous direction

Retrieved earlier planning favoured continuing a narrow RustyBMP product and pausing Bonsai's broader platform. Your current decision supersedes the product-name and repository choice. Preserve the earlier correctness-first scope, but deliver it inside Bonsai. A merger reduces maintenance only if duplicate parsers, authentication, inventory, UI and deployment paths actually retire.

The supplied private ChatGPT conversation URL could not be opened directly. Prior conclusions were retrieved through personal context; all findings below were checked against freshly fetched public code rather than accepted from that summary.

## 2. Review provenance and limits

| Repository | Audited default-branch commit | Last commit date | Tracked files |
|---|---|---|---:|
| [g-arjuna/bonsai](https://github.com/g-arjuna/bonsai/tree/20903963155c27b35a8c511df229d71b848f7aa0) | `20903963155c27b35a8c511df229d71b848f7aa0` | 2026-05-28 | 1,156 |
| [g-arjuna/rustybmp](https://github.com/g-arjuna/rustybmp/tree/e2a1189392db520b25a376795e1bfae72eca5b55) | `e2a1189392db520b25a376795e1bfae72eca5b55` | 2026-06-25 | 282 |

Bonsai contains 126 Rust, 132 Python and 55 Svelte files; RustyBMP contains 103 Rust, 48 Python and 30 Svelte files. These are tracked filename counts, not quality or test-coverage measurements. Bonsai's root `AGENTS.md` is empty. No tracked LICENSE-named file was found in either checkout; RustyBMP declares Apache-2.0 in its workspace manifest and README, whereas Bonsai's root manifest does not declare a license.

Review performed: full public clones; repository/manifests/workflow inventory; targeted source tracing through BMP framing, parsing, RIB updates, storage, graph projection, vendor detection, auth, HA, correlation and rollback; direct binary-fixture length inspection; standards checks against RFCs. This is a detailed architectural and correctness audit, not a claim to have read every line or completed a security audit.

**Execution boundary:** this environment had no `cargo`, `rustc`, Docker or containerlab executable. `python -m pytest tests/protocol/test_bmp_pdu.py -q` was attempted and failed because pytest was absent. No Rust build, test-suite pass, UI build, throughput benchmark or live-router interoperability is claimed. Source findings are distinguished from behavioural tests still required on Ubuntu. Both repository checkouts were left unchanged.

## 3. What to preserve, consolidate and defer

| Area | Existing value | Decision |
|---|---|---|
| Bonsai UI | Svelte 5, device/collector views, topology, incidents, investigations | Retain the main `ui/` shell; add Routing as a native workspace |
| Bonsai control plane | Registry, credentials, auth/RBAC, configuration, graph, provenance | Make these authoritative; fix security and identity issues before exposing routing |
| RustyBMP protocol code | `rbmp-core` already separated from server | Import and harden; make it the only BMP/BGP decoding implementation |
| RustyBMP RIB | `rbmp-rib` separation and route-event model | Preserve structure; replace incomplete keys and event contracts |
| RustyBMP history | DuckDB route history, queries, retention foundation | Retain initially behind one storage owner; repair durability and migrations |
| RustyBMP UI | Routing-specific tables and views | Port selected presentation components; do not embed a second application/login |
| Bonsai BMP parser | Better early frame-bound checks than RustyBMP receiver | Preserve this behaviour as regression coverage; retire duplicate decoding after parity |
| Vendor data | Bonsai YAML path profiles, syslog patterns, mappings; CLI adapters | Consolidate into a capability registry, not an all-purpose plugin framework |
| Python/ML | Two overlapping Python ecosystems and dashboards | Preserve source/history, disable from default core path; migrate only useful deterministic utilities |
| Kafka/NATS, HA, broad integrations | Existing optional functionality | Keep out of required MVP dependencies; assess later |
| Autonomous remediation | Trust/approval/rollback concepts | Default off; observations first. Do not advertise restart-safe rollback or HA safety yet |
| EVPN, VPN, FlowSpec, BGP-LS routing history | Parsers/types exist, end-to-end gaps remain | Explicit experimental/unsupported statuses until each family passes full lifecycle tests |
| BGPsec/ASPA/ML conclusions | Some implementation exists | Do not expose unverified output as valid/security assurance |

Avoid a clean-sheet rewrite, changing the frontend framework, replacing every database, or moving every Bonsai module into a new crate at once. Those consume the maintenance budget without proving the merger's value.

## 4. Prioritized findings

Paths in this section are relative to their original repository at the pinned commits. “Confirmed” means the source or bytes demonstrate the described issue; it does not mean a live exploit or end-to-end failure was executed.

### F01 — P0: fixtures and tests encode the same incorrect BMP format

**Confirmed.** RustyBMP `scripts/gen_bmp_fixtures.py::bmp_header` uses `struct.pack("!BBIB", 3, msg_type, total, 0)`: seven bytes in the wrong order. `tests/protocol/test_bmp_pdu.py` defines a seven-byte common header and explicitly expects `length == len(data) - 1`. RFC 7854 defines a six-byte common header: version, four-byte length, type. The test validates the generator's mistake.

Direct byte inspection of all ten committed `.bin` fixtures found incompatible RFC header lengths. Examples: `01_initiation.bin` is 31 bytes but the standard length field decodes to 67,108,864; `03_route_monitor_announce.bin` is 96 bytes but decodes length zero. Separately, the inline `ROUTE_MONITOR_PDU` in `tests/integration/bmp_pdu.rs` is 75 bytes while declaring 76; its embedded BGP message is 27 bytes while declaring 28.

Fix the generator and expectations together, but establish independent RFC-derived expectations and real captured fixtures first. Keep old invalid bytes as negative cases. Do not simply regenerate snapshots until tests turn green.

### F02 — P0: framing checks happen too late in RustyBMP

**Confirmed control flow.** `crates/rbmp-server/src/receiver.rs::handle_connection` waits until the buffered length reaches the advertised frame length before calling the parser that checks the maximum. A huge declared length can grow the buffer beyond the intended bound. A declared zero length consumes zero bytes, so the inner loop can repeatedly parse the same buffer without progress.

Reject invalid version, lengths below six and lengths above the configured bound immediately after the header is available. Add connection limits, header/body deadlines and cancellation. Audit `src/bin/collector.rs`, TLS receivers and collector-protocol framing for equivalent issues. Bonsai `src/streaming/bmp.rs::run_bmp_receiver` already checks the range before allocating the body; retain that property in the replacement.

### F03 — P0: route identity collapses independent observations

**Confirmed.** RustyBMP `rbmp-rib/src/manager.rs` stores `ribs: HashMap<IpAddr, RibTable>` with the explicit assumption that a peer is globally unique by address. Two exporting routers can monitor the same peer address; overlapping VRFs are also normal. Peer-down/termination can clear another observation's routes. `table.rs` further keys by rendered prefix and optional path ID within RIB type, rather than a complete typed identity.

Bonsai `src/graph/mod.rs::upsert_bmp_session` uses device address + peer address. `write_bmp_route_monitoring` includes device, peer, RIB type, AFI/SAFI and prefix, but omits peer distinguisher and Add-Path ID. Fix identity across memory, durable events, API, graph joins and UI together; patching only the hash map is insufficient.

### F04 — P0: both parsers misclassify some BMP information

**Confirmed against RFCs.**

- Both initiation parsers map TLV 0 to sysDescr, 1 to sysName and 2 to an administrative string. RFC 7854 assigns 0 to String, 1 to sysDescr and 2 to sysName. RustyBMP termination mappings also reverse String and Reason interpretation.
- RustyBMP `bmp/types.rs::RibType::from_peer` and Bonsai `streaming/bmp.rs::classify_rib_type` treat peer type 2 as Adj-RIB-Out. Type 2 is Local Instance Peer; the O flag determines outbound monitoring. Bonsai's classification also misses O-flag handling for other peer types.
- RustyBMP `PeerFlags::filtered` checks `0x08`. RFC 9069 defines the Loc-RIB F flag in the high bit (`0x80`) under a peer-type-specific layout. Do not interpret this as the IPv6 flag for a Loc-RIB peer.

Create table-driven tests for peer types, flag combinations, TLVs and unknown values. Decode flags in the context of peer type and message type. Keep raw values for diagnostics.

### F05 — P0: durability and success metrics are unreliable

**Confirmed.** `rbmp-store/src/writer.rs` receives a Tokio broadcast stream; lagged events are logged and lost. `flush_batch` logs insertion/commit errors but does not return a result. The caller clears the batch and increments `rib_events_written_total` regardless of commit success. Individual errors can leave a partial logical batch before commit. End-of-RIB is intentionally not persisted. `receiver.rs` archives best-effort and continues after archive errors.

Make a durable journal the recovery authority, with sequence/checkpoint replay, or explicitly bounded loss with persisted gaps. Recommended MVP is local durable journal + idempotent projections. Broadcast is acceptable for disposable UI notifications, not the only path to historical truth. Count committed records only. Define shutdown drain, disk-full handling and retention boundaries.

### F06 — P1: Add-Path, ASN width and parser length contracts are incomplete

**Confirmed.** `bmp/parser.rs::parse_route_monitoring` calls `parse_bgp_update(rest, false)`, so live BMP input does not enable Add-Path there. `BgpUpdate::all_*_with_path_id` returns `None` for MP prefixes; `RouteChange` drops the path ID even when the in-memory RIB has one. AS_PATH decoding attempts two-byte parsing and falls back to four-byte parsing on an error, instead of consistently using the wire context. Public BGP OPEN/UPDATE parser functions access fixed headers before checking their minimum length and ignore the declared BGP length.

Add a decode context from the BMP header and Peer Up OPEN capabilities, scoped to the correct session, direction and AFI/SAFI. Test two-/four-octet ASNs, AS_TRANS/AS4 reconstruction, MP next hops and malformed lengths. Either implement each feature end to end or refuse to claim support; never silently decode unsupported Add-Path as ordinary NLRI.

### F07 — P1: EVPN parsing does not establish EVPN history support

**Confirmed source path.** `bgp/attributes.rs` stores EVPN routes separately and leaves generic MP prefix vectors empty; `rbmp-rib/src/manager.rs` emits route changes by iterating generic prefix helpers. The store's EVPN insertion logic is nested under those route-change events. EVPN-only updates therefore lack the required generic event path; the writer handles EVPN announcements there but not a corresponding complete withdrawal lifecycle. Several family decoders also convert errors into empty vectors via `unwrap_or_default()`.

Use a typed NLRI event enum with family-specific keys when enabling additional families. Do not represent EVPN as a unicast prefix or claim that “parser exists” means the UI/history works. Test announce, replacement, withdraw, restart and malformed input for each family.

### F08 — P1: best-path and validation labels can overstate evidence

**Confirmed.** `rbmp-rib/src/table.rs::recompute_best_path` compares only LOCAL_PREF, AS_PATH length and MED; manager entries initially set `is_best = true`. This is not the exporting router's complete decision process. `rbmp-enrichment/src/bgpsec.rs::validate_path` can return `Valid` when keys exist without a verified signature input. Path-status code has a vendor/draft-specific constant and scanning assumptions; fixtures/documentation incorrectly attribute some path-status behaviour to RFC 9069, which defines Loc-RIB monitoring.

Separate `router_reported_selection`, `observed_view`, `heuristic_rank` and `validation_state`. Unknown is legitimate. Disable optimistic security claims immediately. Keep experimental path-marking decoding behind an explicit protocol profile backed by exact captures/specification revision; do not scan arbitrary bytes for coincidental TLVs.

### F09 — P1: Bonsai peer-change detection reads the state after overwriting it

**Confirmed ordering; regression test required.** In `src/graph/mod.rs::write_bmp_peer_state`, `upsert_bmp_session` updates `session_state` before `get_bmp_session_state` reads the “old” value using the same key. The subsequent comparison can suppress the transition event. Read previous state first, then atomically update and record the transition. Also review `BmpTargetMap::resolve`: fallback through `router_address` (assigned from remote peer BGP ID) and peer address can attribute observations to the remote device when the exporter is unknown.

### F10 — P1: correlation can merge ambiguous events or discard an expired slot

**Confirmed.** `src/correlation_buffer.rs::record` chooses the first matching slot when the incoming sub-key is empty; multiple peers/interfaces can be conflated. If an exact slot has expired before the sweeper drains it, `record` removes it without returning it for processing.

Preserve original events before correlation, emit expired slots rather than deleting them, and keep ambiguous evidence unresolved. Include instance identity, bounded capacity and deterministic time in the new tests. Correlation is an interpretation layer, never the primary record.

### F11 — P1: HA and rollback are not safe release promises

**Confirmed.** `src/ha_coordinator.rs::run_etcd_election` writes a per-node key and assumes leadership rather than winning exclusive ownership; startup errors also fall back to leader. `src/remediation/rollback.rs` holds rollback windows in an in-memory HashMap. Default the merged release to standalone, read-only observation. Later HA requires exclusive election, fencing and partition tests; write operations require durable action/rollback records and restart recovery.

### F12 — P1: security defaults and shared policy need consolidation

**Confirmed defaults.** Bonsai auth falls back to `BONSAI_REQUIRE_AUTH == 1`, and RustyBMP auth is disabled by default. Bonsai installs permissive CORS. Python SSH paths use `AutoAddPolicy`. These are development conveniences, not suitable defaults for an exposed combined product.

Use Bonsai auth as the only public authority. Bind local development UI/API to loopback unless deliberately configured otherwise; provide explicit isolated-lab mode. Secure the internal routing service, restrict proxy routes, use scoped service credentials, enforce request limits and filter credentials from logs. Route API reads need scope checks too. Do not enable raw SQL/Cypher or shell helpers merely to port an old page.

### F13 — P1: test discovery and CI can create false confidence

**Confirmed configuration.** RustyBMP is a virtual workspace, but its Rust integration tests sit at root `tests/integration/mod.rs`, without a package/test target wiring them into normal workspace tests. Its CI masks Python failures with `|| echo`, makes wiring checks advisory, and starts `rustybmp:ci` in a different job without an explicit image transfer/load step. A successful preceding Docker job does not put the image on the next runner.

Bonsai has numerous workflows and substantial tests, but its root Cargo config requires sccache and mold while the reviewed main workflow does not explicitly install them. Toolchain policy is also inconsistent: Bonsai pins 1.95.0; RustyBMP CI selects 1.85. Confirm these on Ubuntu rather than assuming a build failure or success. Enumerate collected tests and make required scenarios fail when prerequisites are missing.

### F14 — P2: build and licensing conventions need a deliberate policy

**Confirmed configuration.** Bonsai forces `--allow-multiple-definition` to mask a zstd linkage conflict, uses expensive optimized dependency builds, and carries 18 empty tracked files. Both projects have duplicate infrastructure surfaces. Record the native dependency/linking rationale and resolve the duplicate-library issue before treating suppressed linker errors as a standard solution. Add accurate license files/metadata and preserve upstream attribution before redistributing imported components; owner confirmation is needed for any genuinely ambiguous ownership/license choice.

## 5. Vendor hardcoding and portability register

Vendor-specific commands and YANG paths are necessary. The problem is vendor assumptions leaking into identity, capability, graph rules and generic workflow decisions.

| Location | Observed assumption | Required change |
|---|---|---|
| Bonsai `src/streaming/mod.rs` | Host prefixes `xrd`, `srl`, `ceos`, `crpd`; substring capability matching; fixed role lists | Use canonical platform IDs and explicit/observed capabilities; role is a suggested profile, not proof of protocol support |
| Bonsai `src/enrichment/parser_chain_enricher.rs` | Separate hostname detection and fixed CLI command table | Centralize adapter selection; return unsupported for unknown platforms |
| Bonsai `src/subscriber.rs` | Native SRL/XR model/path logic plus vendor-hint fallback | Retain native adapters; derive support from advertised models plus tested OS/version profiles |
| Bonsai `ui/src/lib/Onboarding.svelte` | Hardcoded vendor options such as `nokia-srl`, versus backend variants `nokia_srl`, `nokia-srlinux` | UI obtains platform list and aliases from backend schema |
| Bonsai `src/http_server/schema_components.rs` | Another vendor enum | Generate or validate against the same registry |
| Bonsai `src/enrichment/registry.rs` | Vendor fallback ordering | Prefer successful capability probes; record why an adapter was selected |
| Bonsai `src/graph/mod.rs` management-interface helper | Prefix guesses `mgmt`, `fxp0`, `me0`, etc. | Prefer interface metadata, then documented adapter fallback; no silent topology exclusion solely by name |
| Bonsai `python/bootstrap_agent.py` | Unknown Genie OS falls back to `iosxe` | Explicit unsupported/error with suggested manual adapter selection |
| RustyBMP `bmppy/policy_fetcher.py` | Unknown vendor uses IOS-XE and `show route-map`; host split at `:` | Remove guesses; parse IPv4/IPv6/hostname endpoints properly; validate command parameters |
| RustyBMP `bmp/path_status_tlv.rs` | Default type 6 with vendor/draft comments | Explicit experimental profile and verified wire contract; no universal support assertion |
| Lab topologies/configs | Fixed `172.20.*` subnets, router names, destination addresses, image versions | These are legitimate fixtures; move variable host endpoints/subnets into one lab manifest and detect conflicts |
| Both defaults | Ports, localhost URLs, data paths, resource limits | Document/configure deployment-specific values; preserve protocol constants |

Proposed registry fields: `platform_id`, `aliases`, `vendor`, `os`, optional version constraint, supported encodings/models, feature support status, adapter ID, fixture IDs and last verified lab record. Canonical examples: `nokia.srlinux`, `cisco.iosxr`, `arista.eos`, `juniper.junos`, `frr`. These are proposed IDs, not existing ones.

Resolution priority: explicit operator selection → authenticated/probed platform evidence → known alias → unknown. Hostname inference may suggest an onboarding choice, but cannot establish support. Unknown BMP exporters can still supply standards-based data; lack of a CLI adapter must not block valid BMP ingestion.

Search the remaining Rust, Python, UI, YAML and shell paths during B07. Classify every match as protocol constant, adapter-specific mapping, deployment default, lab fixture, or incorrect global assumption. Do not replace every IP, enum or constant with arbitrary configuration. Add tests where a router is renamed `edge-a`, unknown platform input stays unknown, and two VRFs share a peer IP.

## 6. Target architecture and ownership

### Recommended staged architecture

Use a Bonsai monorepo containing the routing crates and, initially, a **private routing process** managed by the same Compose profile. This avoids loading another heavy embedded database into Bonsai's existing binary before measuring build/RSS costs. It also preserves crash isolation while the protocol engine is hardened. One product can use two internal processes.

Keep existing Bonsai paths stable in the first import. Add `crates/rbmp-core`, `crates/rbmp-rib`, `crates/rbmp-store` and a reduced routing-service package to the root workspace. Preserve crate names initially to avoid a cosmetic rename diff. Import only the runtime dependencies actually needed; make Kafka, NATS, ML and external enrichers optional. `rbmp-server` currently pulls several of these unconditionally, so this is an explicit extraction task.

Public requests enter Bonsai only. Bonsai validates user scope, resolves inventory IDs and calls the private routing API. RustyBMP's standalone UI, auth, credentials and separate inventory are transitional source, not new public surfaces. The existing Bonsai BMP receiver must be disabled for streams assigned to the routing service. There must never be two authorities for the same route stream.

| Component | Authoritative ownership |
|---|---|
| Bonsai | Users/RBAC, device/site identity, credential references, runtime configuration, UI, incidents |
| Routing process | BMP sessions, protocol context, accepted raw observations, durable routing event sequence, RIB projections, route history |
| Graph | Device/peer/instance relationships, summaries, provenance links and incident context |
| DuckDB | Initial route-history query projection, with one writer/connection ownership policy |
| Local journal | Recovery source for accepted observations and rebuildable projections |

Do not copy every route update into the graph. Graph consumers keep checkpoints and rebuild from durable events. Cross-database updates are eventually consistent; expose lag and watermarks rather than pretending they form a distributed transaction. A later ADR may combine processes if measured complexity outweighs isolation, without changing public contracts.

### Identity contract — settle before migrating data

Use typed fields, not colon-concatenated strings:

- `ObservationSource`: collector ID + stable exporter/device ID + exporter instance + connection/session epoch. TCP source IP/port are attributes, not a durable identity.
- `PeerKey`: exporter identity + peer type + eight-byte peer distinguisher + peer address and protocol peer context. Preserve peer ASN/BGP ID; define how changes affect identity. Loc-RIB has its own instance semantics and no remote peer.
- `ViewKey`: peer/instance identity + AFI/SAFI + direction + pre/post-policy or Loc-RIB view + filtered-completeness metadata.
- `RouteKey`: view + canonical family-specific NLRI + optional Add-Path ID. Future VPN/EVPN keys include their own RD and route-type fields; a BMP peer distinguisher is not a universal substitute for an NLRI RD.
- `EventKey`: persisted source sequence/epoch, stable across replay, plus event schema version. Content hash alone must not deduplicate two legitimate repeated announcements.

Session epochs fence old disconnects: an old connection closing must not delete a newer connection's state. Across collectors, retain separate observations unless an explicit exporter/source mapping identifies replicas and defines the preferred source. Never merge based on DNS name alone.

### Data quality contract

Every history/current-state response should include source, view, observation time, receive time, durable sequence and completeness state. Suggested states: `syncing`, `complete_for_observed_view`, `stale`, `gap`, `unsupported`. EOR marks completion only for the appropriate view/family; it does not prove completeness for the whole router. A quiet established connection is not necessarily stale: distinguish transport health, last update and last successful synchronization.

BMP transport loss means observation loss, not proof that BGP withdrew every route. Mark the view stale/unavailable; preserve last-known state and a gap event. Peer Down, explicit withdrawal, GR/LLGR retention and collector restart are distinct lifecycle events. Historical data with missing identity cannot be repaired by inventing values.

### Persistence contract

Define accepted as “journal append committed under the configured durability policy.” BMP itself supplies no application-level per-message commit acknowledgement: document the crash window and use reconnect/resynchronization when continuity is uncertain. Local sequence allocation, journal records and checkpoints must survive restart together.

Recommended flow: bounded frame decode → journal accepted observation → RIB/history projection → checkpoint → optional graph/UI notifications. Retry idempotently after crashes. The store writer must return errors, roll back failed batches, retain/replay uncommitted work, and run blocking DB work outside asynchronous network tasks. Apply explicit disk/queue limits; when unable to preserve accepted input, fail visibly and force a new synchronization instead of continuing with a healthy badge.

Keep DuckDB initially; validate its write/query contention with representative replay before choosing a replacement. Specify journal retention, checkpoint compaction, backup consistency and schema versions. Avoid introducing Kafka solely to solve a single-machine problem.

### Proposed public API and UI scope

Proposed routes, not existing APIs: `/api/routing/v1/speakers`, `/peers`, `/routes`, `/prefix-history`, `/coverage`, `/events`. Require stable cursors, bounded page size/time range, documented error envelopes and scope filtering. Do not expose raw DB queries as the normal UI contract. Live events may be lossy notifications only if clients can resume from durable cursors or explicitly refetch after a gap.

First UI: exporter/session list; route table with view/VRF filters; prefix timeline with before/after path attributes; freshness/gap indicator; links to the Bonsai device and original evidence. Show collector down, empty complete RIB, not-yet-synchronized and unsupported family as different states. Defer porting the full routing dashboard catalogue.

## 7. Ordered implementation backlog

Each ID is a reviewable outcome, not necessarily one commit. `P0` blocks trusting routing data. Tasks can be split when their edit surface grows; do not close them using mocked end-to-end evidence. “Depends” identifies prerequisites, not permission gates.

| ID | Priority / depends | Work and primary files | Acceptance evidence |
|---|---|---|---|
| B00 | P0 / none | Inventory Ubuntu versions, available images, active labs, Git state and resources; record pinned SHAs | `docs/baseline.md` with exact commands, results, blockers; no existing lab altered |
| B01 | P0 / B00 | Repair test discovery/CI masking; put Rust tests under a workspace package or explicit target; align toolchain/build prerequisites | Collected-test list; deliberately failing assertion makes required job red; cross-job image explicitly built/loaded |
| B02 | P0 / B01 | Correct fixtures and BMP length/TLV/view contracts in both source baselines, using RustyBMP as eventual canonical parser | Independent binary validation, exact RFC cases, malformed corpus retained; no golden tests that merely repeat generator logic |
| B03 | P0 / B02 | Harden framing and public BGP parsers; audit collector/TLS entry points | Zero/short/huge/truncated input terminates safely; fragmented/coalesced frames decode; bounded-memory and timeout tests |
| B04 | P0 / B02 | Define typed source/peer/view/route identities and versioned event DTOs | Same peer/prefix on two exporters and two distinguishers stays isolated; schema serialization tests; migration ADR |
| B05 | P0 / B03,B04 | Session-aware decode: ASN width, Add-Path, EOR, MP next hops; feature support truth | Context-driven decode tests; unsupported combinations yield explicit status; no silent default ASN-width guessing |
| B06 | P0 / B04,B05 | Durable journal, transaction error handling, replay/checkpoints and lifecycle handling | Forced crash/restart equals replay oracle; disk-full/lag visible; committed metric matches durable rows; EOR persisted |
| B07 | P1 / B00 | Canonical vendor/capability registry and alias migration | All vendor matches classified; unknown has no Cisco fallback; `edge-a` works; UI/API identifiers agree; IPv6 endpoints tested |
| B08 | P1 / B01 | Establish standalone read-only defaults and truthful claims; gate BGPsec, HA and auto-remediation | Feature manifest drives UI/docs; unsupported features cannot report verified/healthy/valid |
| B09 | P1 / B04,B06 | Fix Bonsai peer-transition ordering and identity projection; repair correlation loss/ambiguity | Single peer down produces exactly one transition; two ambiguous peers remain separate; expiry/restart does not erase evidence |
| B10 | P1 / B01–B08 | Import required routing crates into Bonsai, preserving original provenance and lockfiles until resolver changes are understood | Import manifest with SHA/file mapping; both baseline tests retained; reduced routing process runs without ML/Kafka/NATS |
| B11 | P1 / B10 | Versioned internal API, configuration validation and service health | Contract tests with actual processes; routing unavailable does not crash Bonsai; no public bypass API |
| B12 | P1 / B11 | Shared identity/auth/RBAC; same-origin API; secret and SSH host-key policy | Unauthenticated denied, viewer cannot mutate, scoped reads enforced, secrets redacted, unknown host requires enrollment policy |
| B13 | P1 / B06,B09,B11 | Routing summaries/provenance into graph with replay checkpoint | Projector restart neither duplicates nor loses accepted events; graph lag visible; raw route stream not duplicated into graph |
| B14 | P1 / B12,B13 | Port prefix timeline, peers and routing table into Bonsai `ui/`; add component/API tests | Real backend E2E from lab event to visible evidence; loading/stale/empty/error/unsupported states verified |
| B15 | P1 / B07,B14 | One minimal Compose profile; documented local dev commands; disable old BMP path | One documented launch path, one public login, one listener per source, clean shutdown; no secrets in committed config |
| B16 | P1 / B15 | Versioned config/DB migration with backup/export and rollback procedure | Copies of old data migrate with counts/quality flags; missing identity labelled legacy; old deployment restores from backup |
| B17 | P1 / B15 | Portable FRR containerlab scenario runner and independent expected-state oracle | Announce/change/withdraw and collector reconnect reproduce from clean start twice; machine-readable evidence bundle |
| B18 | P1 / B17 | Add one available non-FRR NOS; version-specific capability matrix | Same core scenario passes; unsupported features skipped with explicit reason; exact image/version/capture recorded |
| B19 | P1 / B17 | Fault/replay/load tests; baseline budgets and retention/backup tests | Overflow, restart, malformed frames, disk pressure and slow queries tested; counts/digest match oracle, gaps explicit |
| B20 | P2 / B16–B19 | Retire duplicate UI/auth/inventory/parser/deployment instructions; release docs/license/CI cleanup | Code search finds one canonical BMP decoder; old RustyBMP release remains restorable; no unsupported claims in default UX |
| B21 | Later / B20 | Family expansion: EVPN first if it serves a real use case, then others | Per-family announce/withdraw/restart/identity tests plus a real capture; do not batch all families |
| B22 | Later / B20 | Optional deterministic analytics and read-only AI explanation | Every conclusion cites stored evidence, confidence/uncertainty and bounded query results; no autonomous device writes |

For B10, use a dedicated integration branch and preserve RustyBMP history through a deliberate subtree/import commit or retained upstream history. Record imported SHAs and licenses. Do not archive the public RustyBMP repository until release parity and migration documentation exist. Do not force-push or mutate existing lab data as part of this plan.

### Milestones and stop conditions

1. **M0 — credible baseline:** B00–B03. No UI work before fixtures and framing are trustworthy.
2. **M1 — trustworthy routing core:** B04–B06 plus B08. Replay and identity pass; feature claims are bounded.
3. **M2 — one usable Bonsai workflow:** B07, B09–B15. Prefix history appears in Bonsai with authentication and quality status.
4. **M3 — reproducible personal release:** B16–B20. Ubuntu lab, restart, migration and one additional NOS validated.

At 4–6 focused hours/week, treat this as a multi-month project. A planning allowance of roughly 80–140 engineering hours is reasonable for this bounded scope, but is not a measured estimate; revise after M0. Do not promise production-grade multi-vendor observability in a handful of AI sessions. If persistence or parser work expands, defer more UI/analytics rather than relax the evidence gates.

## 8. Ubuntu development and test workflow

### Safe baseline commands

Use VS Code on Ubuntu, or Remote SSH into Ubuntu, so compilation and lab tests run where containerlab and images exist. These commands inspect the environment; they do not deploy a lab:

```bash
uname -a
lsb_release -ds
free -h
df -h .
rustc --version
cargo --version
rustup show
node --version
npm --version
python3 --version
docker version
docker compose version
containerlab version
containerlab inspect --all
docker images --digests
command -v clang cmake protoc pkg-config sccache mold
git status --short
```

Record installed versions before changing dependencies. Bonsai's pinned Rust toolchain and frontend package engines govern the initial build; RustyBMP's old 1.85 claim must be revalidated against its lockfile. Do not install “latest everything” or delete caches to fix an unexplained build. Inspect `.cargo/config.toml` first; make optional acceleration portable instead of assuming sccache/mold on every host.

Start from clean clones or safe worktrees, preserving any local work. Suggested sibling layout: `~/src/bonsai`, `~/src/rustybmp-reference`; keep this document in `bonsai/docs/merger/`. On a fresh clone, `git switch -c integration/routing-core` creates an isolated branch. If that name already exists, inspect it rather than overwrite it. Compare current HEADs to this audit before relying on line-level observations.

### Existing baseline checks

In RustyBMP first run `cargo metadata --no-deps --format-version 1` and inspect test targets. Then use focused commands such as:

```bash
cargo test -p rbmp-core --locked
cargo test -p rbmp-rib --locked
cargo test --workspace --locked -- --list
```

The root integration folder is not evidence of automatic discovery; B01 must repair it. Run the existing Python protocol test separately and report collected count, skips and failures. It currently checks the wrong wire convention, so a green result does not establish BMP compliance.

For Bonsai, inventory the tests before expensive full builds. Run `cargo fmt --all -- --check`, targeted tests for changed modules, and eventually workspace Clippy/tests with the unified toolchain. Both frontend projects have different Vite/Svelte integration versions; port components into Bonsai's existing main UI rather than merging package manifests blindly. `ui/package.json` in Bonsai currently has build/dev/preview only; add meaningful checks as part of B14.

### Commands to create during implementation

The following are proposed targets, **not currently available commands**:

| Target | Purpose |
|---|---|
| `just doctor` | Non-mutating dependencies, ports, disk and image checks |
| `just check-fast` | Formatting, changed-package lint/unit/contract checks |
| `just test-protocol` | Real parser golden/negative/property corpus |
| `just test-replay` | Journal, RIB and history equivalence without routers |
| `just dev-up` / `just dev-down` | One minimal product deployment |
| `just lab-up frr` | Dedicated named lab after conflict check |
| `just lab-test frr` | Independent scenario assertions and evidence bundle |
| `just lab-down frr` | Remove only this run's named resources |
| `just verify-release` | Required checks with explicit blocked/failed status |

Use a run ID and dedicated subnet/network. Never use broad `docker system prune`, destroy all labs, overwrite existing NOS configuration, or add production devices to a test topology. Licensed NOS images remain local and must not be committed or uploaded.

### Verification layers

| Layer | Required cases | Independent evidence |
|---|---|---|
| Protocol | Header sizes, TLVs, flags, zero/oversized lengths, truncated OPEN/UPDATE, unknown extensions, IPv4/IPv6 | RFC-derived vectors plus sanitized live captures; feed actual Rust parser |
| RIB | Same peer IP on two routers/instances; pre/post/in/out isolation; Add-Path; withdraw; peer down; EOR | Expected typed keys and final route-set digest |
| Recovery | Crash before/after journal/projection commit; replay twice; old session closes after new one connects | Stable event IDs, checkpoint positions, counts and final-state equality |
| API | Scope, cursor bounds, time range, unsupported family, missing backend, gap metadata | Actual API against temporary persistent stores |
| UI | Selected view, prefix timeline, reconnect, authorization, empty/stale/error | Playwright against real API, with screenshots as supporting evidence only |
| Lab | Announce, attribute change, withdraw, BGP flap, BMP-only disconnect | Router CLI JSON/raw output + packet capture + application events |
| Load | Repeatable replay size/rate, slow consumer, disk pressure, sustained query | Loss accounting, RSS/CPU, queue occupancy, ingest and visibility latency |

Use fixed small prefixes such as documentation ranges, fixed clocks in unit tests, seeded generators and monotonic waits with deadlines. Avoid sleep-only success checks. A CLI snapshot must be matched to the same RIB/view being compared; selected routes are not equivalent to all Adj-RIB-In paths.

Initial load experiment: 1k, 10k, then 100k deterministic route events at controlled rates. These are test sizes, not advertised capacity. Measure on the actual Ubuntu hardware; set budgets after a baseline. Do not carry README throughput targets into marketing without measurements.

### Minimal live scenarios

1. Start two FRR routers and the merged stack using a dedicated lab network. Verify FRR BMP support and required configuration in the actual pinned image; existing topology is a starting point, not proof it starts correctly.
2. Announce one IPv4 and one IPv6 prefix. Assert exporter, peer, instance, view, attributes and completed synchronization.
3. Change a path attribute and withdraw one prefix. Match timeline and final state to router evidence.
4. Disconnect BMP while keeping BGP up. Assert a monitoring gap/stale view rather than a fabricated BGP withdrawal.
5. Restart collector and core independently, then restart both. Compare replay and fresh synchronization; stale old connection shutdown must not clear new state.
6. Exercise duplicate peer addresses/distinguishers first with deterministic replay; add an appropriate real VRF/NOS topology when available.
7. Run the same supported subset on one locally available non-FRR image. Record unsupported combinations explicitly; vendor family alone is not a support guarantee.

Each run produces `manifest.json`, `results.json`, version/image digests, source SHAs, sanitized config, router evidence, bounded application logs, event IDs/cursors, counts and artifact checksums. The record lists passed/failed/skipped/blocked separately. Never treat missing images, server unavailability or zero discovered tests as a pass.

## 9. Repository standards and maintenance cadence

Create a short root `AGENTS.md`, a canonical `docs/merger/STATUS.md`, `docs/merger/BACKLOG.md`, and ADRs for identity, persistence and service boundaries. Archive obsolete planning documents instead of maintaining a growing collection of contradictory “complete” backlogs. Preserve historical context through Git.

Required conventions: committed application lockfiles; one documented toolchain; explicit feature flags; no runtime guesses that turn unknown into supported; additive/versioned event schemas; ordered migrations with backups; structured errors; bounded queues and queries; graceful shutdown; health separated from readiness/data completeness; sensitive-value redaction; evidence-backed README claims. Run dependency/license/security checks at release and on lockfile changes. Do not globally rewrite formatting or dependencies during protocol fixes.

Observability should cover accepted/committed/replayed/rejected events, durable gaps, queue depth, projection lag, active sessions, synchronization state, disk usage and parse errors by bounded reason. Avoid prefix/peer-as-unbounded metric labels that can explode time-series cardinality.

Practical cadence: one small task session, one verification session, one short status update per week. Keep work in progress to one task; release a small internal milestone every few completed outcomes rather than accumulate a giant integration branch. Every session records what changed, what was actually executed, unresolved risks and the next task. A week without feature work can still improve a capture, regression case or deployment reproducibility.

## 10. Codex model and token strategy

Official model guidance checked on 11 October 2026 recommends GPT-6.1 Sol for complex coding when available, Luna for focused repeatable tasks, and Astra for the most demanding work. Availability depends on the account/client. Source: [OpenAI model guidance](https://learn.chatgpt.com/docs/models).

My workflow recommendation:

| Work | Model / effort | Usage rule |
|---|---|---|
| Normal implementation, API/UI integration, debugging | GPT-6.1 Sol, default or Medium | Primary model; one task with named files and acceptance tests |
| Mechanical alias migration, docs, bounded fixture metadata, simple test plumbing | GPT-6 Luna, High | Only when expected output is precise; stronger review for protocol/security implications |
| Identity/durability design and difficult parser review | GPT-6.1 Sol, High first | Escalate only if a concrete unresolved design issue remains |
| Rare hard architecture/recovery issue | GPT-6 Astra, Light/Low initially | One bounded decision/review, not the default implementation loop |

If 6.1 Sol is absent, use the available Sol model and keep the workflow unchanged. Select through the VS Code model picker; do not assume this chat's model access proves extension availability. No exact token quota or guaranteed savings is promised. API dollar rates are not interchangeable with a ChatGPT Plus allowance.

The larger saving comes from less repeated context and rework:

1. Read this handoff fully once. Thereafter use `STATUS.md`, the relevant task and named source paths; do not repaste every backlog and log.
2. Start a new task conversation after a completed slice. Carry a 200–400-word handoff with commit, tests, evidence location and next step.
3. Use `rg` and targeted file ranges. Store full build logs locally and return the failing diagnostic/tail, not megabytes of dependency output.
4. Keep architecture stable during implementation. Do not ask the agent to “improve the whole repo” or add a new framework while fixing a parser.
5. Retain Cargo/npm caches; run focused checks while editing and required full gates at milestones. Do not repeatedly compile both heavy database stacks for a UI label change.
6. After two unsuccessful attempts at the same failure, stop patch guessing: capture minimal reproduction, state competing explanations, then escalate one model/effort level.
7. Avoid parallel agents by default: they duplicate repository context and can conflict on schemas. Use them only when you deliberately authorize independent bounded work.
8. Track usage per accepted task alongside retries and test failures. Cheap initial output that requires substantial repair is not cheaper overall.

### Suggested root AGENTS.md content

```markdown
# Development contract

- Product: Bonsai. Routing protocol/history components originate in RustyBMP.
- Read docs/merger/STATUS.md and the assigned task; inspect current Git state.
- Work on one task at a time. Preserve unrelated local changes.
- Reproduce correctness bugs before fixing them; use independent expected values.
- Protocol parsers must reject malformed input without panic or unbounded allocation.
- Preserve exporter, instance, view, family, path and session identity end to end.
- Unknown/unsupported/stale/gap must remain explicit; never fabricate a success state.
- Do not use disposable broadcast as the sole path to durable history.
- No live device writes, broad lab teardown, data deletion or force-push.
- Use only the task's dedicated containerlab resources.
- Do not weaken assertions, hide failures, change goldens blindly or count skips as passes.
- Run focused checks, then the task's required gate. Report unexecuted checks honestly.
- No broad dependency updates, cosmetic rewrites or new infrastructure without task need.
- Finish with changed files, executed commands/results, evidence path and next step.
```

### First prompt to paste into Codex on Ubuntu

```text
Read docs/merger/BONSAI_RUSTYBMP_CODEX_HANDOFF.md and any applicable AGENTS.md.
Implement B00 and B01 only, then prepare the B02 reproduction cases.

Goal: establish an honest baseline for merging RustyBMP into Bonsai. Inspect Git
state and compare HEADs with the audited SHAs. Inventory Rust/Node/Python,
Docker/containerlab, active labs, available NOS images and host resources.
Do not deploy/destroy labs or change live routers in this task.

Check actual Cargo test discovery, CI failure masking and build prerequisites.
Create docs/merger/STATUS.md, BACKLOG.md and a baseline report with exact test
counts and blockers. Make the smallest test/CI wiring changes justified by the
baseline. Preserve unrelated edits. Do not begin UI integration or broad imports.

For B02, independently validate the committed BMP fixture headers against RFC
7854 and add failing regression cases against the real parser; do not rewrite
expected output merely to match existing generated bytes.

Run the relevant available checks and finish with files changed, commands and
results, checks not run, and the precise next task. Do not claim completion of
the merger. Do not push or publish anything in this session.
```

### Reusable implementation prompt

```text
Implement task Bxx from docs/merger/BACKLOG.md.
Read STATUS.md, the task's acceptance criteria and named source files first.
Confirm prerequisites from recorded evidence. Reproduce the issue, make a
minimal coherent change, run focused tests and the acceptance gate, and update
STATUS.md. Do not broaden scope or disable failing tests. If blocked, preserve
the reproduction and explain the specific blocker. End with a concise handoff.
```

## 11. Release definition of done

- [ ] One Bonsai UI/login/inventory/configuration authority; no publicly exposed duplicate routing control plane.
- [ ] One tested BMP decoder and explicit family/view support matrix.
- [ ] No cross-exporter/VRF/path collisions; monitoring loss distinct from withdrawal.
- [ ] Durable replay and idempotency proven; history and current state agree after restart.
- [ ] Authentication/scope/secret handling tested across API and streaming paths.
- [ ] FRR scenario reproducible; at least one additional available NOS tested before a multi-vendor claim.
- [ ] UI distinguishes unknown, stale, incomplete, empty and unsupported results.
- [ ] Migration preserves backups; legacy identity gaps are labelled; rollback documented.
- [ ] Required CI failures fail; integration tests are collected; optional tests have explicit skip reasons.
- [ ] README lists tested functionality and limitations; HA/autonomous remediation/security validation are not overstated.
- [ ] License/provenance, install guide, evidence bundle and next-task status are committed.

## 12. Source index

All repository findings refer to the exact snapshots in section 2. The source links below remain useful if the default branch changes.

### RustyBMP source

- [Workspace](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/Cargo.toml)
- [Fixture generator](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/scripts/gen_bmp_fixtures.py)
- [Python protocol tests](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/tests/protocol/test_bmp_pdu.py)
- [Rust fixture tests](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/tests/integration/bmp_pdu.rs)
- [Receiver](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-server/src/receiver.rs)
- [BMP parser](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-core/src/bmp/parser.rs)
- [BMP types/flags](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-core/src/bmp/types.rs)
- [RIB manager](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-rib/src/manager.rs)
- [RIB table](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-rib/src/table.rs)
- [Store writer](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-store/src/writer.rs)
- [BGPsec validator](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/crates/rbmp-enrichment/src/bgpsec.rs)
- [CI](https://github.com/g-arjuna/rustybmp/blob/e2a1189392db520b25a376795e1bfae72eca5b55/.github/workflows/ci.yml)

### Bonsai source

- [Manifest](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/Cargo.toml)
- [BMP implementation](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/streaming/bmp.rs)
- [Graph writes](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/graph/mod.rs)
- [Vendor/support logic](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/streaming/mod.rs)
- [Correlation](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/correlation_buffer.rs)
- [HA](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/ha_coordinator.rs)
- [Auth](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/src/http_server/auth.rs)
- [Build configuration](https://github.com/g-arjuna/bonsai/blob/20903963155c27b35a8c511df229d71b848f7aa0/.cargo/config.toml)

### Standards and model guidance consulted

- [RFC 7854 — BMP](https://www.rfc-editor.org/rfc/rfc7854.html): common/per-peer headers, information and termination TLVs.
- [RFC 8671 — Adj-RIB-Out](https://www.rfc-editor.org/rfc/rfc8671.html): O flag and view semantics.
- [RFC 9069 — Loc-RIB](https://www.rfc-editor.org/rfc/rfc9069.html): peer type 3, F flag and Loc-RIB identity.
- [RFC 9972 — advanced BMP statistics](https://www.rfc-editor.org/rfc/rfc9972.html): further statistics conformance reference for the existing XRd scenarios; not proof of implementation conformance.
- [OpenAI models](https://learn.chatgpt.com/docs/models): current model selection and availability guidance. Effort allocation and task workflow in this document are recommendations for this project.

**Next action:** copy this file into the Bonsai checkout, open that checkout in VS Code on Ubuntu, and use the first-session prompt above. A successful first session produces a trustworthy baseline and failing regression cases—not a large unverified merger diff.

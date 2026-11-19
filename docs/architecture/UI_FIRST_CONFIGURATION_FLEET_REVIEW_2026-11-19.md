# UI-first configuration and centralized collector control — 2026-11-19

Status: source review and implementation intake, not implementation or browser acceptance.
Baseline main: `728926150c311f1e96c4dfe5a58cf3a3fa4a93d8`.
Extends BN-015 onboarding/config, BN-019 operational proof, BN-020 UI, BN-021
trusted assignment, BN-022 security, BN-023 deployment/HA and PRs #34/#35.
One canonical backlog, no new top-level epic or dependency.

## Product contract

After installation/enrollment, every supported operator tunable must be
manageable through Core UI and its authenticated API, including settings that
require restart. Core is authoritative for desired fleet configuration, not
local hand-edited TOML/YAML/env. A versioned, typed configuration catalogue
classifies each field as hot apply, component restart, process restart,
rolling deployment or immutable/bootstrap/platform-owned. The UI shows
actual effective values, source, desired/applied revision, health, pending
restart, errors and rollback. Unsupported or unsafe features are visibly
disabled, never success-by-placeholder. A small installer/bootstrap trust
root, config DB locator, node identity, initial Core address and OS/container
resource/provisioning credentials remain essential; they are not a second
operational authority. Platform-owned resources (PVC expansion, network
firewall, external secrets, host packages) require supported provider
workflow or explicit blocked status; no arbitrary remote root/shell access.

## Source-verified defects and gaps (static inspection)

| Finding / evidence | Required outcome |
|---|---|
| **C1 — false configuration success.** settings.rs:1560–1660 list reports presence only; GET returns null for non-DB fields, DB values not effective values. Generic PATCH accepts any JSON <=512 KiB, no per-section typed/range/cross-field validation, persists only and returns ok; export dumps enabled DB rows, not complete effective config. Settings.svelte:245–315,550–596 raw JSON textarea and success toast, no apply/restart operation. | Typed merge/validation, reject unknown keys and invalid values before commit, revision/CAS, effective/default/provenance readback, explicit restart classification; no empty-default editor or generic arbitrary JSON bypass. Redacted export/import dry run, atomic commit and rollback. |
| **C2 — boot order prevents persisted values taking effect.** server_startup.rs:32–100 builds logger from early TOML; :111–174 probes profile, selects role, installs metrics listener, creates bus and debouncer; :336–416 opens graph, governor, captures auto_investigate. Only :430–438 loads DB overrides. Thus logging, bus capacity, debounce/backpressure/memory and early AI decision cannot be assumed applied even after restart. Config read failure/invalid runtime section falls back to TOML (settings.rs:281–350). | Resolve complete validated effective config before any consuming subsystem; config authority must be openable without graph-dependent bootstrap cycle (reuse existing SQLite control store, secure migration). Explicit last-known-good or recovery mode, not silent fallback; measure actual instantiated values. |
| **C3 — partial streaming snapshot and stale merge.** settings.rs:167–250 stores only enable/address, not full protocol knobs; :375–404 GET returns AppState startup copies, :404–658 PATCH merges from same copies and never replaces them. Settings.svelte:113–135 sends all receivers each save. UI also omits sFlow although backend supports it. | Full typed protocol descriptors; update desired/effective separately, targeted diffs only, no lost sequential edits/stale GET/restart of unchanged receivers. Expose supported sFlow and detailed controls. |
| **C4 — receiver lifecycle/readiness.** settings.rs:556–658 respawns patched receivers without uniform enabled/role checks; PCEP is persisted but no restart branch (startup:858–861 registers disabled); syslog UDP/TCP toggles map to one enabled flag, UDP takes precedence when both supplied; legacy BMP spawn predicate can filter a disable request without stopping existing task. Canonical bonsai-routing is separate. receiver_supervisor.rs:70–115 marks Listening immediately before actual bind and task errors merely log, :139–149 aborts immediately after signal. | Reuse one start/stop/reconcile path for boot and live changes, wait bind/ready or terminal error, disable actually closes socket, no double bind or lost task; distinguish desired/observed status and unsupported PCEP. Separate or clearly coupled syslog transports; never create parallel BMP authority. |
| **C5 — live receiver restart loses dependencies.** startup:748–809 passes shun engine, pattern watch, governor and supervisor to syslog/SNMP; settings.rs:637–652 passes None for several of those, and static targets. Startup receiver role gate run_collector differs from generic Core settings respawn. | Shared receiver factory preserving identity, source scope, shun/filter/pattern reload, governance, telemetry counters and role; verify after UI edit, not just boot. |
| **C6 — HA UI saves wrong authority.** ha.rs:130–255 writes [ha] TOML only if BONSAI_CONFIG_PATH present, otherwise returns requested values without write. Startup :1630–1668 reads BONSAI_HA_MODE/NODE_ID/ETCD_* env, not [ha] (Config has no HA struct); main config locator uses --config/BONSAI_CONFIG. | Authoritative typed HA policy, actual fenced topology, no success on missing persistence, controlled rollout/preflight. Existing BN-023 single-writer/consensus gate remains; no unsupported HA enablement. |
| **C7 — unsafe restart.** ha.rs:271–295 spawns shell without passing executable/args, ignores spawn result and calls process::exit(0); cert apply also exits directly. Does not establish drain, graceful shutdown, replacement health or manager policy. | One audited restart operation with durable job, graceful drain/checkpoint, role-aware supervisor integration (systemd/container/Kubernetes), boot revision and readiness verification, timeout/rollback. Standalone without manager explicitly unsupported until mechanism exists; no false restart success. |
| **C8 — adapters configuration vs running tasks.** outputs.rs:68–100 upsert/remove registry only; traits.rs:266–305 logs SQLite failures but still changes in-memory list; startup :1960–1996 clones enabled config into background tasks once. Removal/edit does not prove task cancellation/restart. Generic syslog type accepted but no builder (output/mod.rs:41–70). | Propagate commit failure; supervised reconcile with stop/start/revision, actual sink ack/health, no orphan task using revoked endpoint/credentials, unsupported types hidden. Coordinate PR #35 delivery/placement. |
| **C9 — inventory/file precedence.** registry.rs:485–520 merges TOML seed entries every startup, including missing devices, then persists; a UI-deleted seed device can return. YAML migration and disk fallback startup:419–478; config::load :2234 requires file. | One-time idempotent migration with tombstones/source report, no post-enrollment reseeding or silent file fallback; protect trusted identity and assignment lifecycle. |
| **C10 — cert bundles and security.** settings.rs:1951–2105 applies alias rows one at a time, so later validation/write failure can leave partial bundle; clear explicitly falls back to TOML. Certs.svelte provides apply/restart but no proof peer sees new cert. auth.rs:933–968 general settings PATCH Operator, generic GET Viewer, restart Admin; generic config read/export must not leak secrets or expose privileged security/HA changes. | Validate bundle all at once (key pair, chain, SAN, expiry, pins, trust overlap), transactional aliases, managed activation and rollback, redacted per-field reads, field/action RBAC and scoped collector credentials. No weakened BN-021/022 fail-closed authority. |
| **C11 — historical TSDB proxy exists but incomplete.** settings.rs:1356–1553 GET config/query supports Prometheus/VictoriaMetrics/Thanos and InfluxDB Flux. max_range displayed but not enforced in inspected handler; caller metric expression may bypass optional device/interface matchers, strings interpolated, default time handling/credential failure and unbounded response need proof. This corrects any implication in PR #35 that no read path exists; extend it, not new proxy. | Approved typed metric/label/time/step query, enforce scope and max range/work/bytes, safe escaping, fail closed on missing credentials, cancellation, normalized coverage, actual receiver-backed UI/MCP query; no arbitrary external query authority. |
| **C12 — multiple config stores/HA replication.** runtime_config:* graph ConfigItems, SQLite settings/devices/adapters, vault, legacy JSON/YAML/TOML/env and HA etcd coexist. ha_coordinator.rs:398–455 applies device/enricher/adapter changes, not complete runtime revision reconciliation. | One logical desired state authority and migration per class, secrets referenced not copied, separate configuration from telemetry graph, explicit snapshot/revision replication and single writer. No parallel authoritative config stores. |

## Configuration coverage matrix

All top-level Config fields (config.rs:9–72) and representative nested fields
are mapped below. Fifteen generic section names in settings.rs:261–277 are
not proof of coverage; they are raw persistence endpoints. Dedicated UI
routes supplement but do not prove runtime application. Future generated
leaf-level manifest must cover all nested fields, env/CLI overrides, service
configs, deployment values and added controls, not just this grouping.

| Config family | Current UI/API coverage | Missing/required |
|---|---|---|
| runtime, graph, graph_path, api_addr, http_addr, metrics_addr | Operations/HA/status partial; absent generic sections | Profile, buffer pool, listeners, role/placement, safe path/volume migration, restart; immutable bootstrap classification; no raw arbitrary path edit |
| collector | Collectors.svelte monitors, assigns devices; no generic section | Graph budget/path, diagnostic port, queue path/bytes/age/batch/log, counter debounce/raw/summary/window/idle, per-site overrides and applied revisions |
| retention, archive, storage | Generic JSON save | Typed bytes/durations, effective enforced caps and per-class policies PR #35, archive owner/compression/rotation/flush, preview expiry, pins/reserve |
| event_bus, ingest | Generic JSON save, early boot issue C2 | Capacity, cache memory, debounce/pressure thresholds/exemptions, effective readback and restart safety |
| logging | Generic JSON save, early boot issue C2 | Levels/module filters, rotation/retention/min free space, safe path, actual logger reload or restart; RUST_LOG precedence removed/explicit |
| signals, streaming | Dedicated enable/address; partial snapshot C3–C5 | Syslog timeouts/frame/archive; SNMP community/USM refs and OID limits; BMP authority/routing URL/frame; BGP-LS/PCEP support; OTLP/sFlow identity; IPFIX TTL/context/domain/template/field/cache bounds; all receiver roles and scope; protected identity not editable as free text |
| credentials, tls, auth, security | Vault/Credentials/Certs/Users/Security; security generic JSON | LDAP URL/search/filter/role mapping/TLS, service TLS/mTLS rotation, auth mode, trust flags and vault lifecycle; no plaintext secret export or arbitrary downgrade |
| layered_ingestion, yang | Profiles/Enrichment/config-items and pattern endpoints | Parser sidecar URLs/priorities/consensus, change schedule/reparse/history/paths, YANG catalogue roots/cache/bundle ref, pattern policy/revisions; reconcile file fallback |
| assignment, target | Rules/override, Devices/Profiles/Sites/Onboarding | Full typed assignment rules precedence and persistent actual apply, every target acquisition/security/path/profile field, no TOML re-seed; scope/credential/epoch rules preserved |
| integrations | Integrations/Adapters, generic ServiceNow and TSDB sections, query proxy | Typed ServiceNow EM/AIOps/change/webhook controls, TSDB query config/limits and sink placement/delivery, adapter lifecycle C8/C11 |
| remediation, ddos | Playbook/Governance/Security/DDoS read surfaces; remediation generic, ddos missing | Dry-run/thresholds/windows/approval/rollback/cooldown/rule defaults, no bypass of approval or automatic live writes |
| logging, ai, gnn, lab | Generic JSON; LLM vault UI | Provider activation/budgets/auto-investigate, model/calibration/threshold, lab-only controls explicit; ensure runtime consumers use current revision, no real provider paid calls in tests |
| managed_sidecar | Sidecar start/stop/restart/status | Auto-start, bounded backoff, approved package/runtime/env refs, no arbitrary executable/working-dir remote shell; required sidecar policy |
| HA, routing service, deployment (outside Config) | HA UI flawed C6, routing status, Helm/Compose files | RoutingServiceConfig env inputs including listener/journal/identity/mTLS/limits, separate service desired state; Helm/Compose/PVC/network policies and image version through supported provider, classify infrastructure vs tunable; no new routing authority |

## Centralized fleet management

Existing `RegisterCollector` streams `AssignmentUpdate` (proto:425–516)
and Heartbeat reports queue/memory/subscriber/receiver status and applied
assignment generation/authority epoch. Preserve BN-021g session generation,
credential rejection, full-sync fence and disconnect lease. This is a
device-assignment protocol, not full remote config management:
`ingest.rs:2424–2648` consumes local RuntimeConfig, _collector_cfg unused
in manager; no desired runtime profile/receiver/storage revision or apply
result in proto. Collectors.svelte:39–92,159–319 has monitoring and manual
assignment, no fleet config/deploy/restart operation. Helm currently does
not render collector workloads merely because fleet replica values exist.

Extend the existing authenticated fleet channel or negotiated compatible
control RPC, not a second ownership service. Core stores versioned global
defaults -> region/site/role profile -> per-node override with explicit
precedence, capability/version, desired/applied digest, actor and audit.
Collector validates signed/authenticated scoped configuration, commits
atomic last-known-good local cache (generated, not hand edited), reports
pending/applied/failed per component, and rejects stale epoch/generation,
unsupported schema or unauthorized endpoints. Offline policy distinguishes
bounded local recovery/telemetry staging from assignment authority lease;
never extend stale device ownership merely because config cached. Rejoin
reconciles, not overwrite Core; no duplicate sessions after restart.

Lifecycle UI: inventory and enroll with short-lived scoped credential and
pinned Core trust, bootstrap installer/provisioner, group/profile assignment,
health/config diff, staged rollout/canary, pause/drain, restart/reconfigure,
signed version upgrade/rollback, certificate rotation/revocation,
decommission and evidence retention. Support Linux/systemd and container/
Kubernetes via explicit provider adapters as deployment work permits; Core
cannot create arbitrary remote infrastructure without authorized provider.
No default privileged agent, arbitrary shell, raw secret injection or
automatic live NOS configuration changes. Restrict artifact provenance,
egress, permissions and resource budgets. Report deployment job stages and
actual ready heartbeat at new revision; no fabricated success.

## Implementation order and proof

1. BN-015/019/020/022: field catalogue (type, units, constraints, default,
   sensitivity, role, source/precedence, apply mode, runtime consumer,
   owner, UI route/test). Generate UI schema/API and coverage report from
   canonical descriptors; CI fails on new unclassified Config/env/CLI/
   routing/sidecar/deployment tunable. Migrate once into existing SQLite
   control store with backup/rollback, validate before component boot,
   secrets in vault; no independent file overrides after enrollment.
2. BN-015/016/019/020/021/022: reproduce C1–C10 then repair actual consumers,
   shared lifecycle factory, desired/applied readback and restart job.
   Regression: invalid/unknown/partial JSON, optimistic revision conflict,
   DB failure, nondefault values pre/post restart, file drift/deleted seed,
   bind conflict, enable/disable, repeated patch, unchanged receiver,
   Core-only vs collector, syslog transport/shun/reload parity, PCEP
   unsupported, sFlow, canonical BMP, adapter edit/remove/disk failure,
   certificate partial failure/rollback, role/secret negative.
3. BN-021/023/019/015: fleet config revision handshake, durable cache,
   assignment/trust fences preserved, group/site inheritance, remote
   apply/restart/rollback and approved deployment provider. Two collectors
   different profiles and protocol sets, partition/reconnect, stale/
   duplicate revisions, unsupported versions, revocation, failed rollout,
   manager unavailable, upgrade/drain/restart and health/readback. No
   HA/rolling availability claim until fenced ownership proof.
4. BN-024-20/013/015/022: fix existing TSDB query and historical evidence
   contract (C11), backend credential/scoped query/retention/coverage tests.
5. BN-019/020/023: full real browser journeys (not screenshot or HTTP 200)
   for every supported setting class: edit -> validate -> commit -> apply/
   pending -> independent runtime evidence -> restart -> persist -> undo.
   Audit Core/collector from fresh packaged install with immutable config
   files removed or conflicting post-migration; no silent drift, false
   healthy/readiness or unaccounted data loss. Report per-field coverage
   and remaining bootstrap/platform exceptions. No paid remote CI,
   no runtime tests performed in this documentation PR.

Secure Core sequence and existing P0 identity/storage gates unchanged.
Control plane correctness is prerequisite for UI-first/fleet-managed
enterprise claims, not a reason to weaken the existing security boundary.

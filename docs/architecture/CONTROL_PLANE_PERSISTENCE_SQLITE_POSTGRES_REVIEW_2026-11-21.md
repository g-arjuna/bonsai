# Bonsai control-plane persistence: SQLite, PostgreSQL and operational data ownership

**Date:** 21 November 2026. **Status:** source-reviewed architecture and backlog intake; not a production change, database migration, HA certification or executed runtime benchmark. **Source baseline:** g-arjuna/bonsai main at a30191df21b80421125cf00ff693bbd59864a018. Respect [AGENTS.md](../../AGENTS.md), [the single backlog](../../BONSAI_BACKLOG.md), [UI-first/fleet review](UI_FIRST_CONFIGURATION_FLEET_REVIEW_2026-11-19.md), [telemetry storage review](TELEMETRY_PERSISTENCE_LIFECYCLE_2026-11-19.md), [resource durability review](RESOURCE_DURABILITY_ARCHIVE_SIZING_REVIEW_2026-11-16.md), [auth storage](../auth_storage.md) and BN-023 HA readiness.

## 1. Decision and constraints

**ADR recommendation (proposed, not yet a deployed feature):**
1. Keep embedded SQLite as the default community/standalone **transactional control-plane** database. It is an appropriate fit for read-mostly, local, single-authority inventory and configuration and adds no mandatory database service.
2. First fix source-of-truth ambiguity, secret persistence, atomicity, upgrade/restore, lifecycle and HA invariants. A PostgreSQL migration by itself fixes none of these.
3. Introduce an **internal typed repository/transaction boundary** independent of HTTP/UI, graph and device-side effects. Initially implement SQLite and parity tests. Add an **optional PostgreSQL backend** only after measured requirements or a qualified multi-Core topology justify it. Never put SQL credentials in collectors or the browser.
4. LadybugDB remains the typed topology/state/evidence graph; routing journals remain durable routing evidence; Parquet/approved TSDBs remain historical telemetry; the encrypted credential vault remains secret custody. PostgreSQL must **not** become a high-frequency raw metric store, the routing event journal, a second graph authority, or a mandatory deployment prerequisite.
5. One Core API owns changes, authorization and policy, regardless of backend. Remote collectors/sidecars access scoped authenticated Core APIs/gRPC with versioned desired/applied state and fenced leases; never access SQL directly.
6. Default packaged single-node remains fully functional offline and without PostgreSQL, etcd, Kafka, external cloud services or a hosted account. Both storage backends, should PostgreSQL be implemented, are community-supported and compatibility-tested; do not paywall ordinary HA/security portability in a purported open-source core.

The supported multi-tenant, multi-writer and multi-node product scopes remain **unqualified** until independently demonstrated. BN-005 temporary isolation exceptions and BN-022 security restrictions are not relaxed by this design. An HA control database does not replicate LadybugDB, routing journals or collector queues.

## 2. Source-verified current state and ownership

| Class | Existing owner / evidence | Desired ownership after remediation |
|---|---|---|
| Device inventory, source bindings, YANG applicability/approvals, remote-discovery jobs | SQLite configuration store (src/sqlite_store.rs, schema v12); ApiRegistry state cache (src/registry.rs) | Canonical transactional configuration repository; versioned inventory/authority; protect security and active-job history against pruning. |
| Adapters, enrichers, settings and assignment epochs | SQLite and process memory; JSON migration and TOML seeds; runtime ConfigItems also in graph | Same logical transactional repository, typed schema, CAS revision, applied/status journal and transactional outbox. |
| Users, password hashes, API key hashes, JWT keys, revocations, service-token records | Separate protected SQLite AuthStore (src/auth_store.rs, src/graph/mod.rs); verified auth backup CLI/runbook | Preserve protected auth store boundary initially; only migrate with independent security threat model, backup/rotation, revocation and live-restore tests. |
| Device passwords and private material | Encrypted vault (src/credentials.rs) **plus legacy inline TargetConfig fields** in config SQLite/JSON/TOML | Vault reference only; no new plaintext password, PEM private key bytes, bearer token, password-equivalent material or old/new audit-value copies in config tables/outbox. |
| Network topology/state, evidence, saved queries and `runtime_config:*` ConfigItems | LadybugDB via GraphStore and UI settings handlers | Graph for observed/provenance-rich state and derived read model; migrate *operational desired configuration* out of graph to one config authority without changing graph's query contract. |
| Device config snapshots and secure capture versions | Encrypted filesystem ConfigStore, src/config_store.rs | Object/file content with hashes, scoped version metadata and retention references in control repository; no large secrets or full captures in transactional rows. |
| Routing replay | BmpJournal/RibEventJournal, routing service, separate projection checkpoint | Keep durable append/replay/checkpoint parity; not SQL transaction tables. |
| Analytic/raw telemetry and ML | Parquet/archive/TSDB/output adapters; message_templates.db and sidecar scheduler SQLite | Distinct lifecycle/quotas by data class, never assume control-plane DB retention covers them. |
| Audit | Config SQLite audit_log, auth_audit, src/audit.rs JSONL and graph event records | One declared audit taxonomy/retention and cryptographically verifiable export contract; mutation-level atomic evidence, redaction, per-class policy, no blind global deletion. |

**Concrete code observations (static, not penetration tests):**

- `src/sqlite_store.rs:111-225`: one `Arc<Mutex<rusqlite::Connection>>` per store; `open_file` calls `migrate()` then WAL; no explicit busy timeout/foreign key enable/checkpoint scheduling visible in this constructor. Multiple store instances can open the same DB.
- `src/sqlite_store.rs:230-714`: `PRAGMA user_version` migrates v1–v12. Early ALTER calls discard failures; v12 is current, but there is no explicit higher/unknown-version refusal at the front of `migrate()`; migrations are not one encompassing transaction.
- `src/registry.rs:135-151,688-780`: `ApiRegistry::open` falls back to JSON on SQLite initialization failure and discards legacy import errors; seed targets re-merge at boot. This creates ambiguity over source of truth. `server_startup.rs:1685-1694` independently opens SQLite and panics on failure; behavior therefore varies by entry path.
- `src/config.rs:1958-1992`: serialized `TargetConfig` includes a lab-only inline password and client-key reference; `src/sqlite_store.rs:999-1094` persists device.password and serializes full old/new device objects to `audit_log`. Thus a configured inline secret can be persisted twice and in backups/WAL. No real-secret exposure was tested.
- `src/sqlite_store.rs:999-1145`: device mutation, audit insert, and asynchronous optional replication publish are separate effects rather than one durable atomic mutation/outbox transaction. Elsewhere database and in-memory state must be checked per call site for the same guarantee.
- `src/sqlite_store.rs:113-164`: `config_replicator` initializes to None; no setter/builder was found. `src/server_startup.rs:1703-1759` creates a watcher-side replicator. `src/ha_coordinator.rs:347-455` publishes to etcd and applies a subset (device/enricher/adapter), not full revision-fenced state; no evidence that the per-store publisher is connected in normal startup.
- `src/sqlite_store.rs:2477-2486`: remote YANG discovery pruning retains newest IDs globally; retention is not shown to protect all non-terminal jobs before deleting older rows.
- `src/http_server/settings.rs:1-7,250-350` and [UI-first review](UI_FIRST_CONFIGURATION_FLEET_REVIEW_2026-11-19.md) document TOML/DB/graph precedence, unvalidated generic JSON sections, early startup before effective overrides and settings UI success without actual apply.
- `src/auth_store.rs:223-315,700-745,3168+`: auth path trust, owner-only files, schema gate, bounded transaction retries, retired JWT key pruning and consistent `VACUUM INTO` backup/manifest are good foundations. Do **not** claim config DB receives these guarantees automatically.
- [Auth backup runbook](../auth_backup_restore_runbook.md) explicitly notes graph-only backup is not complete DR. [Deployment volumes](../deployment_volumes.md) still frames graph volume as low backup priority; this is misleading where auth/config DB and routing artifacts live alongside graph runtime.
- Helm `deploy/helm/bonsai/templates/statefulset.yaml` mounts **per-pod RWO graph PVCs**, not a qualified shared transactional authority or cross-core graph/routing replication. Compose mounts runtime volumes for Core/routing. The deployment manifest alone is not HA proof.

## 3. Domain architecture: HLD

```text
Browser / operator / external agent
  -> authenticated API, RBAC, tenant/site/field policy, quotas, idempotency
  -> typed ConfigCommandService (validate + compare-and-swap)
        -> ConfigRepository transaction (SQLite default; PostgreSQL optional later)
             desired state + revision + audit metadata + transactional outbox
        -> durable revision reconciler
             component/collector desired -> applied/failed/stale, fenced generation
        -> status/read models, UI change history and health
  -> CredentialVault / protected AuthStore (separate custody, opaque references)
  -> GraphStore (observed state/evidence; explicit projection, never shadow authority)
  -> Routing / collector protocols (no SQL endpoint)
```

The repository abstraction needs *domain operations*, not an unbounded generic key-value SQL facade: e.g. `mutate_device(expected_revision, change, principal, idempotency_key)`, `approve_schema_binding(...)`, `set_config_section(...)`, `claim_outbox_batch(...)`, `list_changes(scope, cursor, limit)`, and an atomic snapshot/read revision. Typed errors: validation, authorization, stale revision, unavailable, corrupt/unknown version, quota, retention hold, migration-in-progress and dependency failed. Avoid a lowest-common-denominator ORM that erases SQLite/PG locking and transaction behavior.

**Control-plane invariants:** one owner per field/class; actor is authenticated server-side, not client-supplied; immutable revision/epoch and audit ID; compare-and-swap for concurrent edits; idempotency for retries; reapplication cannot resurrect deleted seed configuration; grant/revoke and assignment operations fence stale actors; device/collector side effects execute **after durable desired-state commit** and report independent observed applied state; no cross-DB or graph transaction pretended atomic. A config revision is not an assurance proof that its receiving component applied it.

## 4. LLD and migration strategy

- Prefer typed, bounded tables for devices/collectors/bindings/approvals/config versions/audit/outbox/reconcile state; JSON only for validated, schema-versioned extensible fragments. Define immutable stable IDs distinct from IP address, tenant/site scope, unique and partial indexes, referential checks, `created/updated/deleted` timestamps, tombstones, provenance and explicit lifecycle values.
- Migration ledger should record version, digest, deployment generation, applied time and rollback support; refuse unknown/newer schema. **Transactional per-version** SQLite DDL/data, verified column/index/foreign-key constraints and representative data invariants; PostgreSQL schema operations follow the same controlled binary/schema compatibility contract. No swallowed ALTER/import/deserialize errors, no silent missing-table acceptance.
- Existing SQLite DBs, old JSON, graph ConfigItems and TOML must be reconciled with a **one-time**, audited import: dry-run inventory and conflict report, secure consistent preflight backup, strict data validation, migration, idempotent marker and authenticated operator recovery. Do not make a file reauthoritative after import or erase unknown/legacy data on failed conversion.
- For SQLite, use explicitly tested URI/path trust and permissions (existing auth store patterns), local persistent disk, WAL with measured `busy_timeout` and `synchronous` policy, `foreign_keys=ON`, bounded read transactions, index/query review, `quick_check`/`integrity_check` plus `foreign_key_check`, measurable checkpoint policy and disk headroom; no blanket `PRAGMA synchronous=OFF`. WAL requires host-local locking and shared-memory semantics; do not place it on unqualified NFS/SMB/RWX multi-host mounts.
- Single connection mutex is an acceptable starting point if measured latency and lock holds meet budgets. First profile critical sections and remove blocking SQL from async runtime paths; use `spawn_blocking`/dedicated bounded worker for synchronous operations, statement caching, bounded projections, pagination and short transactions. Add a small read pool only if actual contention shows benefit; no cargo-wide driver rewrite just to add a pool.
- For PostgreSQL, optional `sqlx` or `tokio-postgres` domain adapter with measured bounded pool, TLS certificate/hostname verification, SCRAM/approved managed auth, least-privilege DB roles, migrations and health checking. No direct untrusted SQL APIs; parameterize all dynamic identifiers and values; enforce scopes at the service boundary and optionally PG RLS as defense in depth. Avoid treating PG RLS as authorization replacement.
- One durable outbox row should be created in the **same transaction** as mutation+redacted audit. Publisher consumes ordered revisions with retry, backoff, dead-letter/degraded state and idempotent receiver ack. Use monotonic generation/leader fencing and per-object revision CAS; no wall-clock last-write-wins, best-effort tokio spawn, process-local epoch or etcd watcher as the sole durability mechanism.
- No requirement to run etcd for SQLite standalone. For eventual multi-Core: compare **(A)** leader-owned control DB with API proxy and replay, **(B)** central PostgreSQL single authoritative primary plus optional replicas/PITR, and **(C)** a formally proven consensus/replicated-SQL strategy. Do not promote a multi-writer SQLite-on-network-volume configuration. Do not introduce two concurrent independent authority planes (PG leader + etcd leader) without clear fencing and failure semantics.

## 5. Security and threat model

**Assets:** authentication signing keys; user and service credentials; device secrets; canonical device/exporter identities; tenant/site inventory; management endpoints, OOB/ISP accounts; approved automation policies; security audit history; backups and replicas. **Adversaries:** compromised browser/agent, malformed API body, stolen backup, hostile insider with partial privileges, compromised collector, untrusted document/inventory, host-level attacker or stale failover leader.

- New secrets are **references** to the vault or external secret manager; never put plaintext or password-equivalent material in device/config/audit/outbox/evidence/graph/logs/metrics. Do not assume JSON serialization has redaction. Review and migrate historical inline values, WAL, backups and exports with an explicit incident/custody procedure; distinguish *potential data presence* from confirmed exfiltration.
- Config DB: owner-only files/dir with no symlink traversal, restricted service identity, encrypted underlying volume where required. Auth DB already has stronger protections; extend only after tests. SQLite core data is not automatically encrypted by WAL or file permissions, and PostgreSQL does not automatically encrypt every application secret. Protect encryption keys separately from backup artifacts, support rotation and offsite restore.
- API: require authenticated principal, per-role/site/tenant/field scope and endpoint-specific permission; never expose raw SQL, unrestricted table browsing or unredacted audit via REST/UI/MCP. Consistent authorization on list, count, export, restore, search and historical versions. Negative tests for collector impersonation, stale token, readonly views, cross-tenant enumeration and offline cached config reuse.
- Audit: immutable event identity + authenticated actor/session/source, before/after **allowed-field diff**, change reason, correlation/request/idempotency ID, policy decisions, rollback ID; secret field names and redacted metadata only. Consider integrity-chained exported journals for tamper evidence; protect retention/legal hold, restrict destructive maintenance. DB local audit tables alone do not establish non-repudiation.
- PostgreSQL mode: `sslmode=verify-full` or equivalent, no public network listener by default, short-lived credentials/rotation, service-specific roles, separate schema/backup privileges, no superuser app account, periodic privilege review, parameterized SQL, statement timeout and row access policy tests.
- Backups: encrypted, access-controlled, separately protected keys, verified hashes plus signed/manifests as policy requires, never allow a downloaded graph archive to implicitly expose auth/config secrets. Restore must consider JWT revocation rollback and replay of previously authorized but now revoked collector/device bindings.

## 6. Maintenance, pruning and quotas

Define an owner-by-class retention policy, not one global “delete data older than N days”:
- **Canonical inventory, approvals, bindings, explicit deletes:** keep current records and historical revision/tombstone evidence by policy. No automatic age eviction of live security authority. Permanent identifiers must not be silently reused.
- **Audit/security/revocations:** class-specific legal/security retention and incident holds; expired JWT revocation rows may be pruned only after the maximum possible token validity + key overlap, with clock-skew and restore semantics proved. Separate retention of sensitive auth change metadata from operational low-value logs.
- **Discovery jobs, enrollments, sidecar state:** terminal-only TTL/row/byte caps; preserve non-terminal jobs, retries, reconciliation cursors and incident-pinned failures until resolved or explicitly expired with clear terminal reason. Never delete arbitrary oldest IDs if still active.
- **Configuration snapshots/YANG catalogues:** immutable hashes, approved-active protection, referenced bundles kept, eventual physical reclamation; expired or superseded bundles only with evidence references and verify-before-delete.
- **Telemetry/graph/ML/export artifacts:** delegate to their existing lifecycle review and storage owners. Include config, auth, journal, archive, backup staging, index rebuild, WAL, log, spool, graph, sidecar and temporary files in aggregate disk budgeting.
- Scheduled, bounded sweep batches, rate limits and priority caps; record rows examined/deleted, duration, blocked/pinned count, estimated bytes freed and actual filesystem bytes. Test crash during sweep, restart, clock jumps, disk-full, concurrent mutation, long reader, active legal hold and restore. Default policy conservative and visible before any deletion.
- SQLite `DELETE` does not imply the file shrinks: assess auto-vacuum policy separately from low-priority maintenance-window `VACUUM`/snapshot replacement. Measure WAL growth and checkpoint stalls, page free list and disk wear. PostgreSQL: rely on tuned autovacuum/ANALYZE for routine MVCC reclamation, partition+drop eligible high-churn event tables when justified, monitor bloat/long transactions/replication slots, use `VACUUM FULL` only in an explicit maintenance window. No automatic hot-path destructive compaction.

## 7. Backup, DR and upgrades

A **Bonsai recovery set** includes: matching generation of control DB, protected auth DB + separate encryption key, credential vault + key, graph snapshot, routing journals/checkpoint, referenced config files/YANG bundles, collector desired/applied cursor evidence, manifests and code/schema compatibility metadata. A coherent cross-store snapshot may require a bounded quiesce/epoch barrier; absent one, record explicitly different component snapshot times, post-restore validation/replay and gap state. An auth-only or graph-only snapshot is **not** a complete restore.

- SQLite: use Online Backup API or proven `VACUUM INTO` for live consistent snapshots (not plain live `cp`/volume tar of one DB file in WAL mode); include validation, owner-only staging, checksum and encryption, external offsite storage, restore smoke test. Freeze or checkpoint appropriately for cross-file coherence. Keep and validate key custody independently.
- PostgreSQL: use physical base backups + WAL archive for PITR where required, or logical dumps for smaller/portable workflows; verify replica retention and restore to exact timestamp, not merely “backup command succeeded”. External managed PG still requires Bonsai-level restore of vault/graph/journals and revocation review.
- Upgrade: preflight disk, WAL/backup space, schema compatibility, readiness and recovery rollback. Take verified snapshot, perform bounded migration, validate independent schema/data invariants, then start consumer components. For schema changes requiring long locks, deploy expand/backfill/dual-read/contract **only** where measured and bounded; no unsafe downgrade after irreversible migration. Encryption key rotation and schema upgrade are distinct change operations.
- Every restore must declare **RPO/RTO measured in drills**, possible replay gap, user/session/token and assignment revocation exposure, stale desired/applied state, bad-cursor detection, and process identity. Emergency JWT/credential rotation and binding revocation must be explicitly reconciled, not silently assumed to survive a snapshot rollback.

## 8. APIs and operator UI

One authenticated service supports config discovery, typed Get/Validate/Preview/Apply/Rollback, optimistic concurrency with `expected_revision`, change history, effective/applied state, filtered export/import dry-run, and maintenance health. API responses distinguish **desired**, **durably committed**, **dispatched**, **applied**, **failed**, **pending restart** and **unavailable**, with audit revision and timestamp. Return conflict (409), invalid (4xx), backpressure (429/503) and backend failure (5xx) truthfully; never success after DB write failure.

The Admin UI should expose backend type, connection health and version, schema compatibility, backup age/restore-validation timestamp, retained-data/pin counts, WAL/DB space and headroom, contention/busy metrics, pending migrations, replication lag and current leader **only when actually verified**, pending changes and actual effective component status. Admin-only operations: schedule backup, initiate verified export, safe prune preview, dry-run migration and audited apply with preflight; no arbitrary SQL editor or unguarded browser-triggered VACUUM. Viewer/Operator roles see only scoped safe status; sensitive DB settings/credentials must not be echoed. Install/upgrade, PVC resizing, PostgreSQL provisioning and OS key access remain platform-owned with visible actionable instructions rather than fake UI success.

Collector/sidecar management remains through Core APIs and authenticated revision/lease protocol. Disconnected collectors may use a signed/verified bounded last-known-good configuration but cannot originate central authority, grant themselves permissions or extend stale leases.

## 9. Deployment matrix

| Profile | Default | Storage/connectivity | Supported claim / qualification |
|---|---|---|---|
| Developer, lab, single-node community | SQLite embedded | Local private persistent volume, backups + vault | Default **after** critical fixes, no external DB. Do not require PostgreSQL or external services. |
| Small/medium single-Core with remote collectors | SQLite embedded | Core-only local control DB; collectors access gRPC, not DB | Suitable unless measured volume/contention, remote admin services or HA requirements exceed it; no claim of Core host-loss HA. |
| Multi-Core enterprise control plane | PostgreSQL **candidate**, not implemented | Independently operated/managed primary, TLS, bounded pool, backup/PITR, optional replicas | Requires BN-023 fencing, auth/vault consistency, graph/routing single-writer/recovery, UI/collector failover and real fault tests. Database HA alone does not certify Bonsai HA. |
| Air-gapped, edge, resource-constrained | SQLite preferred | Offline local DB, safe disk quotas | Critical open-source profile; no licensing/infrastructure regression. |
| Multi-region/multi-tenant hosted | Future architecture decision | PG may help concurrent transactional access | Do not claim global active-active or tenant isolation without independently proven policy, residence, revocation and eventual/strong consistency rules. |

For Helm, differentiate runtime persistent volumes by security class and lifecycle: graph/routing journals, control/auth databases, vault/keys, archive and scratch. Validate mount location, ownership, access mode, node relocation, snapshots and restore; one per-replica RWO PVC does **not** make config synchronized or safe to mount across hosts. Do not assume the `graph` PVC is disposable while it carries control data. Require exact resource requests, free-space reserve and restore staging budget rather than hardcoded magic database size.

## 10. Sizing model and performance qualification

Configuration capacity is dominated by **records, indexes, revision/audit churn, snapshot history and backups**, not the flow/BMP/gNMI events/s. Use a reproducible worksheet:

```text
control_live_bytes =
  sum_over_tables(row_count * measured_avg_or_p95_row_plus_index_bytes)
control_history_bytes =
  mutations_per_day * retained_days * measured_redacted_audit_bytes
reserve_bytes =
  measured_wal_peak + checkpoint_margin + migration_scratch
  + concurrent_backup_staging + free_disk_failure_headroom
required_local_disk =
  control_live_bytes + control_history_bytes + reserve_bytes
  + auth/vault/graph/journals/archives/spool sizes measured separately
```

Run representative 1k/10k/100k-device **synthetic** scenarios, with separately parameterized collectors, policy revisions, users, discovery jobs and retention durations; numbers are experiment sizes, not support claims. Benchmark concurrent readonly UI/API browsing, typed writes from multiple admins, many read-only routing-consumer connections, collector assignment churn, security revocation, large audit history, cold boot, migration and restore with 10%/1% free-space failure. Record p50/p95/p99/API latency and SQLITE_BUSY/lock waits, Core CPU/RSS, fsync/WAL I/O, checkpoint stalls, bytes-per-object, index plans, graph ingestion impact and end-to-end reconciliation lag.

Compare SQLite local versus PostgreSQL on the **same workload semantics** and equivalent durability/security settings; include actual network latency, TLS, connection pooling, failover and operations burden. Do not infer Postgres is faster because it is a server, or SQLite is inadequate because device count grows. Proposed adoption triggers require sustained operational evidence: unavoidable multi-process/multi-Core concurrent writers, remote SQL consumers that cannot safely use Core API, qualified cross-host failover and PITR need, or write-lock/latency/recovery objectives repeatedly missed after measured SQLite optimization. Verify each by a complete fault/restore benchmark and a cost/sizing worksheet before switching the default or adding deployment complexity.

## 11. Acceptance sequence and non-negotiable gates

**G0 inventory/decision** (BN-029-01): comprehensive write/read/backup ownership manifest including sidecars, auth, graph ConfigItems and vault; real schema inventory and migration matrix; default SQLite ADR with explicit supported topology and PostgreSQL threshold.

**G1 security/correctness** (BN-029-02/03/04): proof of secret-safe config writes and audits (incl. historical-data plan); one-transaction desired/audit/outbox; deterministic failure, CAS/idempotency, fail-closed startup and upgrades; no JSON fallback or resurrected seed. Unit/integration + process-kill/ENOSPC/injected failpoint and negative auth tests.

**G2 lifecycle/recovery** (BN-029-05/06): terminal-state-safe retention/incident holds/physical reclamation and maintenance; cross-store backup/restore manifest, offline restore and revocation/replay drills; real disk quota and auth/key loss gates. Docs/volume ownership synchronized with packages/Helm/systemd.

**G3 UI/API/fleet** (BN-029-07): one typed command service, revision tracking, truthful apply/readback and scoped role-based browser tests, collector offline/rejoin, runtime migration and restart. No duplicate authority with BN-015/020/021/024.

**G4 HA/backend decision** (BN-029-08/09): rigorous SQLite optimized baseline; optional PG adapter parity and migration/rollback PoC behind flag if triggers are met; no external DB dependency for default installation. If HA: two Core hosts, leader fault, old leader fencing, network partition, config CAS/concurrent write, lost outbox delivery, secrets/rotation, disk/full, graph/routing/collector checkpoint/replay and independently compared expected state before promotion.

All work is **intake**. Do not interrupt M6/YANG, Secure Core or release gates; avoid unreviewed runtime changes. Use standard L0–L3 gates for code slices, L4 where a real deployment/browser/HA proof is named. This document is research, not executable validation.

## 12. Primary upstream references

- SQLite, [Appropriate Uses](https://sqlite.org/whentouse.html), [WAL](https://sqlite.org/wal.html), [backup API](https://sqlite.org/backup.html), [network filesystem caveats](https://sqlite.org/useovernet.html), [PRAGMA](https://sqlite.org/pragma.html) and [VACUUM INTO](https://sqlite.org/lang_vacuum.html).
- PostgreSQL, [Routine Maintenance](https://www.postgresql.org/docs/current/maintenance.html), [Row Security Policies](https://www.postgresql.org/docs/current/ddl-rowsecurity.html), [WAL and PITR](https://www.postgresql.org/docs/current/continuous-archiving.html), and [connection configuration](https://www.postgresql.org/docs/current/runtime-config-connection.html).

References describe database capabilities, **not** features already implemented or performance benchmarked in Bonsai.

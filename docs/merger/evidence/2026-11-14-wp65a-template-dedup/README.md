# WP6.5A — persistent semantic message-template recognition and embedding reuse

14 November 2026. Slice of the WP5D/WP6 semantic-enrichment workstream
(see BONSAI_BACKLOG BN-011 / EV1-3). Branch `feat/wp65a-template-dedup`.

## What was built

Local-first three-stage deduplication in front of the syslog embedding
provider, in `python/bonsai_ml/message_templates.py`:

1. **Exact fingerprint** — sha256 of the raw message, mapped through a
   persistent `exact_map` table to a template embedding.
2. **Template fingerprint** — sha256 of the normalized template produced by
   `MessageNormalizer` (versioned `syslog-tpl-v1`).
3. **Provider call** — only for templates absent from the registry.

`syslog_embedding_worker.py` resolves each fetched batch through
`TemplateDedup`; every event still receives its own `EventEmbedding` record
(raw evidence and occurrences remain independently queryable). Per-event
legacy behavior is preserved via `dedup_enabled: false`.

### Template mining evaluation

Drain/Drain3 was evaluated and **not** adopted: it adds an unvendored runtime
dependency unavailable in the offline build environment, and its
token-similarity merging can collapse operationally distinct messages
(different error codes/thresholds) and makes template boundaries
insertion-order dependent. Vendor TextFSM/pyATS parsers target structured
command output, not free-form syslog. Selected: a conservative deterministic
token normalizer that masks only unambiguous variable fields (IPv4/IPv6/MAC/
UUID/hex identifiers, ISO timestamps, common interface names, identifier
`key=value` pairs). Bare numbers, `up`/`down`/`established`, error codes,
thresholds and admin-vs-failure wording always stay literal.

### Persistence, versioning, scope

- SQLite registry (`runtime/message_templates.db`, override via
  `BONSAI_TEMPLATE_REGISTRY_PATH` or `template_registry_path`), WAL mode,
  `PRAGMA user_version` schema versioning. Survives Collector/Core/BonPy
  restarts (local to the embedding worker).
- Cache key = sha256(scope, model_id, model_revision, normalization_version,
  fingerprint). `scope` (`dedup_scope` / `BONSAI_EMBEDDING_SCOPE`, default
  `local`) isolates tenants — no cross-scope reuse. Model or normalization
  upgrades produce new keys (controlled re-embedding; old vectors retained).
- `EmbeddingConfig.model_revision` added; `model_revision` is sent in the
  embedding write-back payload (ignored by current Rust schema, forward-
  compatible).

### Concurrency and failure safety

- New templates are claimed via atomic SQLite `BEGIN IMMEDIATE` with a
  bounded lease (`dedup_claim_ttl_secs`, default 600s). Concurrent workers
  observe `pending` and skip; stale claims are reclaimable.
- Provider output is validated (dimensionality, finite values, non-zero
  norm, response count). Zero-filled or mismatched provider responses mark
  the template `failed` with exponential backoff — never cached.
- The worker's `MlMemoryManager` LRU class (`_LruCache`) fronts the registry;
  the registry is the durable backing, not the ephemeral cache.

### Metrics

`TemplateDedup.stats_snapshot()` / `syslog_embedding_worker.get_dedup_stats()`
expose: exact-match hits, template-match hits, new templates, provider calls,
provider calls avoided, embedding cache hit ratio, template cardinality
(ready/pending/failed + occurrences), and estimated provider-ms saved.
Emitted as a structured log line each worker cycle.

## Benchmark (this host, 14 Nov 2026)

Corpus: 10,000 synthetic mixed-vendor syslog messages, 5% unique.
Full data: `bench-10k.json`, `bench-10k-st.json`, `bench-10k-repetitive.json`.

| Embedder | Strategy | Provider texts | Wall | Notes |
|---|---|---|---|---|
| hash (deterministic) | per-event | 10,000 | 0.29s | baseline |
| hash | dedup | 4,219 | 2.45s | 57.8% provider-text reduction; dedup adds ~0.2ms/msg SQLite+normalize overhead |
| bge-small-en-v1.5 (real ST, CPU) | per-event | 10,000 | 106.3s (~94 texts/s) | baseline |
| bge-small-en-v1.5 | dedup | 4,219 | 54.7s | ~71s estimated provider time saved |

Template cardinality is driven by semantically significant literal fields
(error codes, SNMP OIDs, states) — conservative non-collapse working as
intended. Identical-message acceptance (10,000 occurrences → 1 provider
text) is covered by `test_10000_identical_messages_one_embedding`.

## Tests

`python/tests/test_message_templates.py` — 22 tests covering all WP6.5A
acceptance criteria: 10k identical → 1 embed, variable-field template reuse,
no-collapse of up/down/error-code/threshold/admin-vs-failure, restart
persistence, model-revision re-embedding, scope isolation, provider-failure
non-poisoning, invalid-vector rejection, cross-worker claim exclusion,
per-event record preservation.

Executed: `cd python && PYTHONPATH=. .venv/bin/python -m pytest
tests/test_message_templates.py -q` → 22 passed. Full `python/tests` suite:
213 passed, 17 failed — all 17 failures are pre-existing on `main` e071cc7
in unrelated suites (`test_f11_rule_fixes`, `test_gnn_scaffolding`,
`test_t0_fixes`, `test_validate_playbooks`; e.g. `_FakeClient` missing
`device_vendor`, feature-name count drift). Not caused by this change.

## Known limitations / next steps

- WP6.2 worker defects (zero-vector returns inside `text_embeddings`
  ollama/openai backends, newest-first fetch order, UUID-per-retry
  write-back, etc.) are **not** fixed here; dedup validation prevents bad
  vectors from being cached, but the underlying defects remain open.
- `run_once` job-engine invocations still rebuild the embedder per cycle
  (model reload) — tracked under WP6.2.F.
- Metrics are log/module-function exposed; Prometheus wiring and core-side
  `/api/ml/embeddings/stats` enrichment are follow-ups.
- On-disk amplification of per-event `EventEmbedding` records and
  100/1k/10k-device capacity modeling remain WP6.5 open items.
- Registry path defaults to `runtime/message_templates.db` relative to the
  sidecar cwd; deployments should set `BONSAI_TEMPLATE_REGISTRY_PATH` or
  `template_registry_path` explicitly.

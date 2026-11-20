# WP6.5A storage sizes — post-graceful-shutdown measurement

Measured 2026-11-16 on retained benchmark workdirs after the runner's
graceful SIGTERM→checkpoint shutdown. No WAL sibling files remain
(`bonsai.db.wal` absent = fully checkpointed). `template_registry.db`
figures are the actual SQLite file size after worker exit — the in-run
harness `dir_bytes` reading (4096) sampled the file before close and
under-reports.

| run dir | events committed | bonsai.db | registry.db | archive.jsonl |
|---|---|---|---|---|
| r100k-rep-ingest500-1794843478 | 100,000 pending | 282,615,808 | 32,768 | 37,320,000 |
| r100k-rep-drain500-1794843713 | 100,000 embedded | 258,957,312 | 32,768 | 37,320,000 |
| m100k-rep-ingest (overload ~920eps) | ~92k | 290,918,400 | 32,768 | 37,320,000 |
| m100k-rep-drain (overload) | ~96k | 219,508,736 | 32,768 | 37,320,000 |
| m100k-hc-drain (overload) | 96,261 | 375,250,944 | 99,565,568 | 53,425,212 |
| m10k-rep-drain | 10,000 | 47,005,696 | 32,768 | 3,732,000 |
| m10k-var-drain | 10,000 | 45,731,840 | 9,334,784 | 3,760,606 |
| m10k-rep-conc | 10,000 | 45,514,752 | 32,768 | 3,732,000 |

## Observations (no per-event byte inference)

- Post-enrichment `bonsai.db` is consistently SMALLER than the
  ingestion-only snapshot at the same scale (258.9MB vs 282.6MB @100k;
  219.5MB vs 290.9MB overload). Enrichment SETs lifecycle properties on
  every event and adds EventEmbedding nodes + EMBEDDED_AS rels, yet the
  checkpointed file shrinks — consistent with dead-page reclamation when
  the many small lifecycle UPDATEs are checkpointed versus first-write
  layout. Snapshots are not byte-comparable for incremental vector cost;
  per-event/per-embedding byte figures are NOT derived here.
- Registry size tracks template cardinality + occurrence bookkeeping:
  5-template repetitive → 32KB; 3-template varied → 9.3MB; ~96k-template
  high-cardinality → 99.5MB.
- Archive is append-only JSONL, identical across rep runs at same scale.

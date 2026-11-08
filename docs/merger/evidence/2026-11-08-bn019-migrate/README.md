# BN-019 bonsai-migrate live drill — 8 November 2026

Base commit `0dad9b8` (main, post install/upgrade drill). Third bounded
BN-019 slice: exercise the BN-007 `bonsai-migrate` operator CLI
(`backup` / `migrate-jsonl` / `migrate-toml` / `rollback`, SHA-256
verified) against real deployment data.

## Result: PASS — host-side procedure validated, fail-closed confirmed

### Decision: host-side, not image-shipped

`bonsai-migrate` is **not** in `docker/Dockerfile.bonsai` (image ships
`bonsai`, `bonsai-routing`, `healthcheck` only). The drill ran the binary
host-side (`cargo build --bin bonsai-migrate`, dev profile) against
**copies** of live journal/config files. Rationale: the tool mutates
files in place and the live `bonsai-routing` process holds its journals
open — in-place migration of a running service's journal is out of scope
for a drill and would be an operational write. The supported procedure is
therefore: quiesce the writer → copy artifact out of the volume → migrate
→ verify → rollback if needed. Shipping the binary in the image (usable
via `docker run --rm -v <vol>:/data bonsai:latest bonsai-migrate ...`)
remains a possible follow-up; not required for this gate.

### Inputs (real data)

| Artifact | Source | Records |
|---|---|---|
| `live-events.snapshot.jsonl` | copy of `/tmp/bn003-journal-r07/events.journal` — genuine FRR 10.6.1 BMP event journal from the running lab `bonsai-routing` (172.100.207.1:11019) | 2,890 lines, all `schema_version:2` |
| `lab-events.snapshot.json` | copy of `/tmp/bn003-journal/events.journal` | 9 lines (spare) |
| `legacy-v1.jsonl` | synthetic v1/v2 mix + 1 corrupt line (modeled on `tests/b16_b17_b18_b19_m3_release_gates.rs` fixture) | 4 lines |
| `legacy-bonsai.toml` | legacy config with `vendor = "srl"`/`"xrd"`/`"unknown_appliance_os"` | — |

The packaged deployment's own `routing-events.jsonl` /
`routing-bmp.journal` in `bonsai_bonsai_graph_dev` are **0 bytes** (no BMP
feed attached — honest `syncing` state), so the lab journal provided the
real-data coverage.

### Executed commands and results

Binary: `target/debug/bonsai-migrate` (built `0dad9b8`, 2m39s incremental).

1. **Live snapshot (all-v2) migrate** — `migrate-jsonl`:
   `total_records_read:2890, migrated_v1_to_v2:0, already_v2_records:2890,
   corrupt_lines_skipped:0`. **Honest finding:** the tool re-serializes
   every line, so `output_sha256 != source_sha256` even with zero
   migrations — verified the rewrite is *semantically* identical
   (2,890/2,890 parsed records equal). Rollback restored byte-exact
   original (`restored_sha256` = source sha `42e32662…`).
   → Operators should treat migrate-jsonl as normalize-on-write; always
   keep the `.pre-m3.bak` + manifest.

2. **Legacy v1 fixture** — `migrate-jsonl`: 2 v1→v2 migrations, 1 already
   v2, **2 `legacy_identity_incomplete`** flags with synthesized
   `event_key.source` (`collector_id:"legacy-migrated"`,
   `session_epoch:0`) and `quality_flags:["legacy_missing_identity"]`;
   1 corrupt line skipped (not fatal). Content inspected — flags and
   synthetic identity present as designed. Rollback → byte-exact restore
   (`6fd1d2ef…`).

3. **Legacy TOML** — `migrate-toml`: `srl→nokia.srlinux`,
   `xrd→cisco.iosxr`, `unknown_appliance_os` preserved and reported in
   `unsupported_vendors` (no silent Cisco fallback), `[bmp]
   authority="routing_service"` appended. Rollback → byte-exact restore
   (`d5078838…`).

4. **Fail-closed negative** — appended a byte to a `.pre-m3.bak`, then
   `rollback` → exit 1, `Backup file corrupted: expected …, got …`.
   Source file untouched.

### Procedure (for operators / future image shipping)

```
# inside the deployment host, with the stack quiesced or on a copy:
docker run --rm -v bonsai_bonsai_graph_dev:/data -w /data \
  bonsai:latest bonsai-migrate migrate-jsonl runtime/routing-events.jsonl
# stdout JSON report contains the BackupManifest; save it:
bonsai-migrate rollback manifest.json   # restores + SHA-verifies
```

Until the binary ships in the image, build host-side:
`cargo build --bin bonsai-migrate` and run against files copied out of
the volume (`docker run --rm -v <vol>:/data -v $PWD:/out alpine cp …`).

### Not done / limits

- No in-place migration of the live lab journal (writer active; out of
  scope — would need a quiesced maintenance window).
- `bonsai.db` (Kuzu) graph migration is not covered by this CLI — it
  migrates the JSONL event journal and `bonsai.toml` only.
- Empty packaged-deployment journals mean the all-v2 path was proven on
  the lab journal, not the packaged deployment's own files.

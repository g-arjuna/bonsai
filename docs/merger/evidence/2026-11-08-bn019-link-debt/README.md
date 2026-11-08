# BN-019 documentation link/provenance debt review — 8 November 2026

Base commit `8017090` plus uncommitted working-tree changes. Scope: the
"documented link/provenance debt" remainder of BN-019, following the UI
warning closeout in `2026-11-08-bn019-ui-warnings`.

## Method

Scripted markdown link check over all live docs (`*.md`, `docs/*.md`,
`docs/architecture/*.md`, `docs/merger/*.md`, `ui/`, `python/`, `crates/`,
`deploy/`, `lab/` READMEs): 164 relative links checked across 84 files.
`docs/backlog_archive/` was excluded — its README explicitly freezes archived
backlogs as point-in-time records ("Do not edit archived backlogs"), so
archive-internal relative links that predate the 13 Oct reorganization are
expected and were not rewritten.

Provenance artifacts verified present: `docs/merger/B10_IMPORT_MANIFEST.md`,
root `LICENSE` (Apache-2.0), `NOTICE` with RustyBMP attribution.

## Findings fixed (4 dead links in live docs)

- `docs/DEVELOPMENT.md` — absolute machine-specific link
  `/home/arjuna/Desktop/bonsai/python/pyproject.toml:1` → relative
  `../python/pyproject.toml`.
- `docs/review_discipline.md` — `../BONSAI_CONSOLIDATED_BACKLOG_DV1.md` moved
  during consolidation → `backlog_archive/2026-10-13/BONSAI_CONSOLIDATED_BACKLOG_DV1.md`.
- `docs/merger/BN005_DEPLOYMENT_AND_SCOPE_REVIEW_2026-10-14.md` —
  `file:///home/arjuna/...` absolute URI → relative
  `../../tests/bn005_deployment_and_scope_hardening.rs`.
- `docs/architecture/sidecars.md` — two links to
  `BONSAI_CONSOLIDATED_BACKLOG_CV7.md`, which exists nowhere in the repo
  (archive holds BV3–BV6, CV1–CV5; CV6/CV7 snapshots were never committed) →
  now points to the archive index with an explicit note. The quoted ADR entry
  "2026-05-14 — CV7 T4" does not exist in `DECISIONS.md`; corrected to the
  actual D12 "event_detection.rs retirement" entry (header quote and
  cross-reference). Also dropped the stale "(to be authored)" note —
  `src/sidecar_registry.rs` now exists.

## Result

Re-run of the same check: **0 missing links** across live docs.

## Limits

- Anchor fragments (`#tier-4` etc.) were not validated beyond file existence;
  the CV7 anchors are moot since the file is absent.
- Archived backlog internal links remain broken by design (frozen snapshots).
- This clears documentation link/provenance debt only; BN-019's
  install/upgrade, packaged build, lab browser acceptance, backup/recovery,
  observability and soak gates remain open.

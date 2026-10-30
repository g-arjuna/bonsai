# Fresh verification evidence — 13 October 2026

Source commit: `e4d8c4489da4b0048a72cd383685131c66adc7ae`.
[Claim validation](../../CLAIM_VALIDATION_2026-10-13.md) explains conclusions and limits.
[results.json](results.json) records exact commands, exit status, log hashes and counts.
[documentation-checks.json](documentation-checks.json) records archive/link/crosswalk checks.

Rust, Python, formatting, core lint, UI build and live FRR logs are retained here.
Generated scenario/FRR files are copies of the fresh runs; prior committed files
were restored. Some harness JSON dates are hardcoded to historical dates. This
manifest and review record the actual fresh execution date.

`executor-probes.rs` reproduces four current gaps. To rerun, copy it into a
scratch test target under `tests/claim_validation_executor_probes.rs`, run the
command from results.json, then remove that scratch target. A passing diagnostic
probe confirms the observed gap. Fixes must assert the opposite safe behavior.
The model/provider, browser, second-NOS and operational gates were not run.

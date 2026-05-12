# Gemini Task Template

Use this template when handing an operational verification task to Gemini CLI.

```text
Task: <short task name>
Goal: <what should be proven or disproven>

Context:
  - Environment: laptop DC | cloud spike | external infra
  - Bonsai endpoint: http://127.0.0.1:3000
  - Lab topology: <path if relevant>
  - External dependency: <Splunk / Elastic / ServiceNow / NetBox / none>
  - Constraints: read-only unless the script explicitly performs a test mutation

Inputs:
  - Brief: docs/gemini_cli_brief.md
  - Result location: <target json or markdown path>
  - Required env vars: <list or "none">

Steps:
  1. <command 1>
  2. <command 2>
  3. <verification step>
  4. <write artifact step>

Expected evidence:
  - <specific API response, log line, event count, or artifact>
  - <what counts as a pass>

If it fails:
  - Capture the exact command run
  - Capture stderr or relevant log excerpt
  - Record current environment state
  - Do not change source code

Branch policy:
  - Use `test-results/gemini` or `gemini/daily-<YYYY-MM-DD>`
  - Commit only test artifacts if a commit is requested

Time budget: <for example 30 minutes>
Token budget: <for example 50K>
```

## Example

```text
Task: Verify Splunk HEC adapter end to end
Goal: Prove Bonsai can emit observable events to a live Splunk target

Context:
  - Environment: laptop external infra
  - Bonsai endpoint: http://127.0.0.1:3000
  - Lab topology: lab/dc/dc-evpn-srv6.clab.yml
  - External dependency: Splunk
  - Constraints: do not change Bonsai source code

Inputs:
  - Brief: docs/gemini_cli_brief.md
  - Result location: docs/test_results/e2e_output_adapters/<YYYYMMDD>-splunk-<pass|fail>.md
  - Required env vars: SPLUNK_PASSWORD, SPLUNK_HEC_TOKEN

Steps:
  1. Run `scripts/sprint5_preflight.sh --check`
  2. Run `scripts/e2e_output_adapters_test.sh --adapter splunk`
  3. Verify the emitted result artifact and any generated log path
  4. Summarize pass or fail in the dated markdown result

Expected evidence:
  - Splunk HEC health responds
  - Bonsai adapter registration succeeds
  - Result markdown includes command, outcome, and supporting logs

If it fails:
  - Capture the exact failing command
  - Include the relevant log excerpt
  - Include preflight state
  - Stop without changing source

Branch policy:
  - Use `gemini/daily-<YYYY-MM-DD>` for the result commit if a commit is requested

Time budget: 75 minutes
Token budget: 50K
```

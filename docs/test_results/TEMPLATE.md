# <feature> integration test

**Date**: YYYY-MM-DD
**Operator**: <your name>
**Bonsai version**: <!-- git rev-parse --short HEAD -->
**Lab topology**: lab/fast-iteration/bonsai-phase4
**External versions**: <!-- e.g. NetBox 3.7.4, ContainerLab 0.60.0 -->

## Prerequisites met

- [ ] ContainerLab topology running (`clab inspect`)
- [ ] bonsai binary built (`cargo build --release`)
- [ ] Required Docker services started
- [ ] Credentials available (env vars set)

## Setup

<!-- Describe any setup steps taken before the test -->

## Test Results

- [ ] Step 1: ...
- [ ] Step 2: ...
- [ ] Step 3: ...

## Assertions Verified

<!-- Paste curl output or relevant log excerpts proving each assertion passed -->

```
# Example:
$ curl -s http://localhost:3000/api/topology | jq '.devices | length'
4
```

## Observations

<!-- Anything notable: latency, warnings, unexpected behavior, flakiness -->

## Logs

<!-- Path to log file captured during this run (gitignored) -->
- `/tmp/bonsai-e2e-YYYYMMDD-HHMMSS.log`

## Result

**PASS** / **FAIL** <!-- circle one -->
